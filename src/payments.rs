//! The payment flow. Read this top to bottom; it is the core of the service.
//!
//!   Tx 1 (short): claim idempotency key + lock invoice row + validate + insert `pending` attempt
//!   --- no DB lock is held from here on ---
//!   PSP call (5 s timeout), keyed by the attempt id
//!   Tx 2 (short): attempt pending->final, invoice open->paid (status-conditional),
//!                 outbox event, store idempotent response
//!
//! If the PSP outcome is ambiguous (timeout / network error / 5xx) the attempt stays
//! `pending`; the sweeper re-asks the PSP using the SAME key, which cannot double-charge.
//! After 120 s it becomes `unknown`: the invoice stays locked, and the sweeper keeps
//! re-checking about once a minute until the PSP gives a definitive answer.

use std::time::Duration;

use axum::{
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{error::AppError, events, psp::PspResult, state::AppState};

/// A pending attempt older than this is picked up by the sweeper.
const STALE_AFTER_SECS: i32 = 15;
/// A pending attempt still unresolved after this is marked `unknown`: automatic retries
/// continue, but it is now alert-worthy, and the invoice stays locked until it resolves.
const GIVE_UP_AFTER_SECS: i32 = 120;
const SWEEP_INTERVAL: Duration = Duration::from_secs(10);
/// `unknown` attempts are re-checked every this-many sweeps (6 x 10 s = once a minute).
const UNKNOWN_RECHECK_EVERY: u32 = 6;

#[derive(Debug, sqlx::FromRow)]
pub struct Attempt {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub business_id: Uuid,
    pub amount_cents: i64,
    pub card_token: String,
    pub idempotency_key: String,
}

/// What POST /pay returns, and what is stored for idempotent replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentResponse {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub status: String,
    pub amount_cents: i64,
    pub psp_ref: Option<String>,
    pub failure_code: Option<String>,
    pub invoice_state: String,
}

pub struct Outcome {
    pub http_status: u16,
    pub body: PaymentResponse,
    pub replayed: bool,
}

impl IntoResponse for Outcome {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.http_status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = (status, Json(self.body)).into_response();
        if self.replayed {
            response
                .headers_mut()
                .insert("idempotent-replayed", HeaderValue::from_static("true"));
        }
        response
    }
}

enum Resolution {
    Succeeded { psp_ref: String },
    Failed { code: String },
}


pub async fn start_payment(
    state: &AppState,
    business_id: Uuid,
    invoice_id: Uuid,
    idem_key: &str,
    card_token: &str,
) -> Result<Outcome, AppError> {
    let request_hash = hash_request(invoice_id, card_token);

    let mut tx = state.pool.begin().await?;

    // Claim the idempotency key INSIDE this transaction. The key row and the attempt row
    // commit together or not at all, so a crash can never leave a key with no attempt.
    // If another request holds the same key uncommitted, this blocks until it finishes.
    let claimed = sqlx::query(
        "INSERT INTO idempotency_keys (business_id, key, request_hash) \
         VALUES ($1, $2, $3) ON CONFLICT (business_id, key) DO NOTHING",
    )
    .bind(business_id)
    .bind(idem_key)
    .bind(request_hash.as_slice())
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;

    if !claimed {
        tx.rollback().await?;
        return replay(state, business_id, invoice_id, idem_key, &request_hash).await;
    }

    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT state, total_cents FROM invoices \
         WHERE id = $1 AND business_id = $2 FOR UPDATE",
    )
    .bind(invoice_id)
    .bind(business_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (invoice_state, total_cents) = row.ok_or(AppError::NotFound("invoice"))?;

    if invoice_state != "open" {
        return Err(AppError::conflict(
            "invoice_not_payable",
            format!("invoice is {invoice_state}; only open invoices can be paid"),
        ));
    }

    let in_flight: Option<(i32,)> = sqlx::query_as(
        "SELECT 1 FROM payment_attempts \
         WHERE invoice_id = $1 AND status IN ('pending', 'unknown') LIMIT 1",
    )
    .bind(invoice_id)
    .fetch_optional(&mut *tx)
    .await?;
    if in_flight.is_some() {
        return Err(payment_in_progress());
    }

    // The unique indexes on payment_attempts are the backstop if the checks above ever
    // have a bug: a second in-flight attempt fails here instead of charging twice.
    let attempt_id: Uuid = match sqlx::query_scalar(
        "INSERT INTO payment_attempts (invoice_id, amount_cents, card_token, idempotency_key) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(invoice_id)
    .bind(total_cents)
    .bind(card_token)
    .bind(idem_key)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(id) => id,
        Err(e) if is_unique_violation(&e) => return Err(payment_in_progress()),
        Err(e) => return Err(e.into()),
    };

    tx.commit().await?;

    let attempt = Attempt {
        id: attempt_id,
        invoice_id,
        business_id,
        amount_cents: total_cents,
        card_token: card_token.to_string(),
        idempotency_key: idem_key.to_string(),
    };
    drive(state, &attempt).await
}


/// Ask the PSP about an attempt and record a definitive answer. Safe to call repeatedly
/// for the same attempt (request path, sweeper, several instances): the PSP dedups on the
/// attempt id, and `finalize` only ever moves an attempt out of `pending` once.
pub async fn drive(state: &AppState, attempt: &Attempt) -> Result<Outcome, AppError> {
    match state
        .psp
        .charge(attempt.id, &attempt.card_token, attempt.amount_cents)
        .await
    {
        Ok(PspResult::Succeeded { psp_ref }) => {
            finalize(state, attempt, Resolution::Succeeded { psp_ref }).await
        }
        Ok(PspResult::Failed { code }) => {
            finalize(state, attempt, Resolution::Failed { code }).await
        }
        Err(err) => {
            // Timeout, dropped connection, 5xx: we do NOT know if the card was charged.
            // Leave the attempt pending (so the invoice stays locked against a second
            // attempt) and let the sweeper resolve it with the same PSP key.
            tracing::warn!(
                attempt_id = %attempt.id,
                error = %err,
                "PSP outcome ambiguous; attempt left pending for recovery"
            );
            current_outcome(&state.pool, attempt.id, false).await
        }
    }
}

async fn finalize(
    state: &AppState,
    attempt: &Attempt,
    resolution: Resolution,
) -> Result<Outcome, AppError> {
    let (status, psp_ref, failure_code) = match &resolution {
        Resolution::Succeeded { psp_ref } => ("succeeded", Some(psp_ref.as_str()), None),
        Resolution::Failed { code } => ("failed", None, Some(code.as_str())),
    };

    let mut tx = state.pool.begin().await?;

    // Status-conditional update: only the caller that moves the attempt out of an
    // unresolved state continues. A concurrent sweeper/retry that lost the race updates
    // zero rows. 'unknown' is included so a PSP that recovers late can still resolve an
    // attempt the sweeper had given up on.
    let moved = sqlx::query(
        "UPDATE payment_attempts \
         SET status = $2, psp_ref = $3, failure_code = $4, completed_at = now() \
         WHERE id = $1 AND status IN ('pending', 'unknown')",
    )
    .bind(attempt.id)
    .bind(status)
    .bind(psp_ref)
    .bind(failure_code)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    if moved == 0 {
        tx.rollback().await?;
        return current_outcome(&state.pool, attempt.id, false).await;
    }

    let invoice_state = if status == "succeeded" {
        // No lock is held across the PSP call, so this is a status-conditional update:
        // it only succeeds if the invoice is still 'open'.
        let paid = sqlx::query(
            "UPDATE invoices SET state = 'paid', paid_at = now(), updated_at = now() \
             WHERE id = $1 AND state = 'open'",
        )
        .bind(attempt.invoice_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if paid == 0 {
            // Should be impossible: void refuses while an attempt is in flight. If it ever
            // happens, roll back (attempt stays pending) and fail loudly for a human.
            return Err(AppError::Internal(anyhow::anyhow!(
                "invariant violated: PSP charged attempt {} but invoice {} is not open",
                attempt.id,
                attempt.invoice_id
            )));
        }
        "paid"
    } else {
        "open" // a failed payment leaves the invoice open so the customer can retry
    };

    let body = PaymentResponse {
        id: attempt.id,
        invoice_id: attempt.invoice_id,
        status: status.to_string(),
        amount_cents: attempt.amount_cents,
        psp_ref: psp_ref.map(str::to_string),
        failure_code: failure_code.map(str::to_string),
        invoice_state: invoice_state.to_string(),
    };
    let http_status: u16 = if status == "succeeded" { 200 } else { 402 };
    let body_json =
        serde_json::to_value(&body).map_err(|e| AppError::Internal(e.into()))?;

    let event_type = if status == "succeeded" {
        "invoice.paid"
    } else {
        "invoice.payment_failed"
    };
    events::emit(
        &mut tx,
        attempt.business_id,
        attempt.invoice_id,
        event_type,
        json!({
            "invoice_id": attempt.invoice_id,
            "invoice_state": invoice_state,
            "payment_attempt": body_json,
        }),
    )
    .await?;

    sqlx::query(
        "UPDATE idempotency_keys \
         SET status = 'completed', response_status = $3, response_body = $4, completed_at = now() \
         WHERE business_id = $1 AND key = $2",
    )
    .bind(attempt.business_id)
    .bind(&attempt.idempotency_key)
    .bind(i32::from(http_status))
    .bind(body_json)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Outcome {
        http_status,
        body,
        replayed: false,
    })
}


/// The key already exists. Same request -> return the stored/current result.
/// Different request -> 422.
async fn replay(
    state: &AppState,
    business_id: Uuid,
    invoice_id: Uuid,
    idem_key: &str,
    request_hash: &[u8],
) -> Result<Outcome, AppError> {
    let row: Option<(Vec<u8>,)> = sqlx::query_as(
        "SELECT request_hash FROM idempotency_keys WHERE business_id = $1 AND key = $2",
    )
    .bind(business_id)
    .bind(idem_key)
    .fetch_optional(&state.pool)
    .await?;
    let (stored_hash,) =
        row.ok_or_else(|| AppError::Internal(anyhow::anyhow!("idempotency record vanished")))?;

    if stored_hash.as_slice() != request_hash {
        return Err(AppError::Coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "idempotency_key_reuse",
            "this Idempotency-Key was already used with a different request".to_string(),
        ));
    }

    let attempt_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM payment_attempts WHERE invoice_id = $1 AND idempotency_key = $2",
    )
    .bind(invoice_id)
    .bind(idem_key)
    .fetch_optional(&state.pool)
    .await?;
    let attempt_id = attempt_id
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("idempotency key without attempt")))?;

    current_outcome(&state.pool, attempt_id, true).await
}

#[derive(sqlx::FromRow)]
struct AttemptRow {
    id: Uuid,
    invoice_id: Uuid,
    business_id: Uuid,
    status: String,
    amount_cents: i64,
    psp_ref: Option<String>,
    failure_code: Option<String>,
    idempotency_key: String,
    invoice_state: String,
}

/// Where an attempt stands right now. Finished attempts return the exact response that
/// was stored when they finished; in-flight ones return 202 with their current status.
async fn current_outcome(
    pool: &PgPool,
    attempt_id: Uuid,
    replayed: bool,
) -> Result<Outcome, AppError> {
    let row = sqlx::query_as::<_, AttemptRow>(
        "SELECT a.id, a.invoice_id, i.business_id, a.status, a.amount_cents, a.psp_ref, \
                a.failure_code, a.idempotency_key, i.state AS invoice_state \
         FROM payment_attempts a JOIN invoices i ON i.id = a.invoice_id \
         WHERE a.id = $1",
    )
    .bind(attempt_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound("payment attempt"))?;

    if row.status == "succeeded" || row.status == "failed" {
        let stored: Option<(Option<i32>, Option<serde_json::Value>)> = sqlx::query_as(
            "SELECT response_status, response_body FROM idempotency_keys \
             WHERE business_id = $1 AND key = $2 AND status = 'completed'",
        )
        .bind(row.business_id)
        .bind(&row.idempotency_key)
        .fetch_optional(pool)
        .await?;

        if let Some((Some(code), Some(body))) = stored {
            let body: PaymentResponse =
                serde_json::from_value(body).map_err(|e| AppError::Internal(e.into()))?;
            return Ok(Outcome {
                http_status: u16::try_from(code).unwrap_or(500),
                body,
                replayed,
            });
        }
    }

    let http_status = match row.status.as_str() {
        "succeeded" => 200,
        "failed" => 402,
        _ => 202, // pending or unknown
    };
    Ok(Outcome {
        http_status,
        body: PaymentResponse {
            id: row.id,
            invoice_id: row.invoice_id,
            status: row.status,
            amount_cents: row.amount_cents,
            psp_ref: row.psp_ref,
            failure_code: row.failure_code,
            invoice_state: row.invoice_state,
        },
        replayed,
    })
}


/// Background loop: resolves attempts the request path could not (PSP timeout, network
/// error, or a crash between the PSP call and Tx 2). Re-asking the PSP with the same
/// attempt id is safe: it returns the original result and never charges again.
pub async fn run_sweeper(state: AppState) {
    let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
    let mut pass: u32 = 0;
    loop {
        ticker.tick().await;
        // Pending attempts are re-driven every pass; unknown ones only every Nth pass, so a
        // PSP that is down isn't hammered every 10 s for each stuck attempt.
        let include_unknown = pass % UNKNOWN_RECHECK_EVERY == 0;
        pass = pass.wrapping_add(1);
        if let Err(err) = sweep_once(&state, include_unknown).await {
            tracing::error!(error = ?err, "payment sweeper pass failed");
        }
    }
}

async fn sweep_once(state: &AppState, include_unknown: bool) -> Result<(), AppError> {
    // 'unknown' still blocks new attempts (the card may have been charged); step 2 keeps
    // re-asking the PSP and the first definitive answer resolves it.
    let given_up: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE payment_attempts SET status = 'unknown', completed_at = now() \
         WHERE status = 'pending' AND created_at < now() - ($1::int * interval '1 second') \
         RETURNING id",
    )
    .bind(GIVE_UP_AFTER_SECS)
    .fetch_all(&state.pool)
    .await?;
    for id in given_up {
        tracing::error!(attempt_id = %id, "attempt unresolved after give-up window; marked unknown (invoice stays locked), will keep re-checking the PSP");
    }

    // 2. Re-drive stale pending attempts, and (on every Nth pass) unknown ones, with their
    //    original PSP idempotency key. Safe to repeat: the PSP returns the original result
    //    and never charges twice.
    let stale = sqlx::query_as::<_, Attempt>(
        "SELECT a.id, a.invoice_id, i.business_id, a.amount_cents, a.card_token, a.idempotency_key \
         FROM payment_attempts a JOIN invoices i ON i.id = a.invoice_id \
         WHERE (a.status = 'pending' AND a.created_at < now() - ($1::int * interval '1 second')) \
            OR ($2::bool AND a.status = 'unknown') \
         ORDER BY a.created_at LIMIT 20",
    )
    .bind(STALE_AFTER_SECS)
    .bind(include_unknown)
    .fetch_all(&state.pool)
    .await?;

    for attempt in stale {
        tracing::info!(attempt_id = %attempt.id, "sweeper re-driving unresolved attempt");
        match drive(state, &attempt).await {
            Ok(outcome) if outcome.body.status == "succeeded" || outcome.body.status == "failed" => {
                tracing::info!(attempt_id = %attempt.id, status = %outcome.body.status, "sweeper resolved attempt");
            }
            Ok(_) => {} // PSP still unreachable; try again on a later pass
            Err(err) => {
                tracing::error!(attempt_id = %attempt.id, error = ?err, "sweeper could not resolve attempt");
            }
        }
    }
    Ok(())
}


fn payment_in_progress() -> AppError {
    AppError::conflict(
        "payment_in_progress",
        "a payment for this invoice is already in progress",
    )
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .and_then(|d| d.code())
        .map(|c| c == "23505")
        .unwrap_or(false)
}

/// Hash of what the request MEANS (invoice + card token), not its raw bytes, so JSON
/// whitespace or key order can't make an honest retry look like a different request.
fn hash_request(invoice_id: Uuid, card_token: &str) -> Vec<u8> {
    Sha256::digest(format!("POST /invoices/{invoice_id}/pay\n{card_token}").as_bytes()).to_vec()
}