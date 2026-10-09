//! Webhook delivery worker.
//!
//! The API never calls a business's server. It writes `webhook_events` and one
//! `webhook_deliveries` row per endpoint in the SAME transaction as the state change
//! (see events.rs). This worker, a background task, does the HTTP.
//!
//! Each pass:
//!   1. CLAIM due deliveries with one statement: `FOR UPDATE SKIP LOCKED` picks rows no
//!      other worker holds, and the same statement pushes their `next_attempt_at` forward
//!      by a lease. No transaction or row lock is held while we do HTTP.
//!   2. POST each one, signed, with a hard timeout.
//!   3. Record the result: delivered, rescheduled with backoff, or dead-lettered.
//!
//! If the process dies mid-delivery, the lease expires and the row is picked up again, so
//! delivery is AT LEAST ONCE. Receivers dedupe on the `Webhook-Id` header (the event id,
//! identical across retries).

use std::time::Duration;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use sqlx::PgPool;
use tokio::task::JoinSet;
use uuid::Uuid;

/// Delay before retry N after attempt N fails. 1 initial attempt + 6 retries = 7 attempts.
/// Total span: 10 s + 30 s + 2 m + 10 m + 1 h + 6 h, about 7 h 13 m. Then dead-lettered.
const RETRY_DELAYS_SECS: [i32; 6] = [10, 30, 120, 600, 3_600, 21_600];

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const BATCH_SIZE: i64 = 20;
/// How long a claimed delivery is invisible to other workers. Must exceed HTTP_TIMEOUT.
const LEASE_SECS: i32 = 60;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, sqlx::FromRow)]
struct Due {
    delivery_id: Uuid,
    attempts: i32,
    event_id: Uuid,
    event_type: String,
    payload: serde_json::Value,
    event_created_at: DateTime<Utc>,
    url: String,
    secret: String,
}

enum Outcome {
    Delivered { status: i32 },
    Failed { status: Option<i32>, error: String },
}

pub fn spawn_worker(pool: PgPool) -> anyhow::Result<()> {
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(HTTP_TIMEOUT)
        // A redirect is a failed delivery, not something to chase to another host.
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            loop {
                match run_batch(&pool, &http).await {
                    Ok(n) if n as i64 == BATCH_SIZE => continue,
                    Ok(_) => break,
                    Err(err) => {
                        tracing::error!(error = ?err, "webhook worker batch failed");
                        break;
                    }
                }
            }
        }
    });
    Ok(())
}

async fn run_batch(pool: &PgPool, http: &reqwest::Client) -> Result<usize, sqlx::Error> {
    // Claim: SKIP LOCKED lets several workers/instances run side by side without
    // blocking or double-claiming. Rows for deactivated endpoints are never claimed.
    let due = sqlx::query_as::<_, Due>(
        "UPDATE webhook_deliveries d \
         SET next_attempt_at = now() + ($1::int * interval '1 second') \
         FROM (SELECT id FROM webhook_deliveries \
               WHERE status = 'pending' AND next_attempt_at <= now() \
               ORDER BY next_attempt_at \
               LIMIT $2 \
               FOR UPDATE SKIP LOCKED) due, \
              webhook_events e, \
              webhook_endpoints ep \
         WHERE d.id = due.id AND e.id = d.event_id AND ep.id = d.endpoint_id AND ep.is_active \
         RETURNING d.id AS delivery_id, d.attempts, e.id AS event_id, e.type AS event_type, \
                   e.payload, e.created_at AS event_created_at, ep.url, ep.secret",
    )
    .bind(LEASE_SECS)
    .bind(BATCH_SIZE)
    .fetch_all(pool)
    .await?;

    let claimed = due.len();
    let mut set = JoinSet::new();
    for d in due {
        let pool = pool.clone();
        let http = http.clone();
        set.spawn(async move { deliver(&pool, &http, d).await });
    }
    while set.join_next().await.is_some() {}
    Ok(claimed)
}

async fn deliver(pool: &PgPool, http: &reqwest::Client, d: Due) {
    let outcome = send(http, &d).await;
    if let Err(err) = record(pool, &d, outcome).await {
        tracing::error!(delivery_id = %d.delivery_id, error = ?err, "could not record webhook result");
    }
}

async fn send(http: &reqwest::Client, d: &Due) -> Outcome {
    let envelope = json!({
        "id": d.event_id,
        "type": d.event_type,
        "created_at": d.event_created_at,
        "data": d.payload,
    });
    let body = match serde_json::to_string(&envelope) {
        Ok(b) => b,
        Err(e) => {
            return Outcome::Failed {
                status: None,
                error: format!("could not serialize payload: {e}"),
            }
        }
    };

    // Fresh timestamp on every attempt, so a retry is never rejected as a replay.
    // We sign exactly the bytes we send.
    let timestamp = Utc::now().timestamp();
    let signature = sign(&d.secret, timestamp, &body);

    let result = http
        .post(&d.url)
        .header("Content-Type", "application/json")
        .header("User-Agent", "invoice-service-webhooks/1.0")
        .header("Webhook-Id", d.event_id.to_string())
        .header("Webhook-Event", &d.event_type)
        .header("Webhook-Attempt", (d.attempts + 1).to_string())
        .header("Webhook-Signature", format!("t={timestamp},v1={signature}"))
        .body(body)
        .send()
        .await;

    match result {
        Ok(resp) => {
            let code = i32::from(resp.status().as_u16());
            if resp.status().is_success() {
                Outcome::Delivered { status: code }
            } else {
                Outcome::Failed {
                    status: Some(code),
                    error: format!("endpoint returned HTTP {code}"),
                }
            }
        }
        Err(e) => Outcome::Failed {
            status: None,
            error: e.to_string().chars().take(500).collect(),
        },
    }
}

async fn record(pool: &PgPool, d: &Due, outcome: Outcome) -> Result<(), sqlx::Error> {
    match outcome {
        Outcome::Delivered { status } => {
            sqlx::query(
                "UPDATE webhook_deliveries \
                 SET status = 'delivered', attempts = attempts + 1, last_attempt_at = now(), \
                     last_status_code = $2, last_error = NULL, delivered_at = now() \
                 WHERE id = $1 AND status = 'pending'",
            )
            .bind(d.delivery_id)
            .bind(status)
            .execute(pool)
            .await?;
            tracing::info!(delivery_id = %d.delivery_id, event_id = %d.event_id, attempt = d.attempts + 1, "webhook delivered");
        }
        Outcome::Failed { status, error } => {
            let attempt_no = d.attempts as usize + 1;
            match RETRY_DELAYS_SECS.get(attempt_no - 1) {
                Some(delay) => {
                    sqlx::query(
                        "UPDATE webhook_deliveries \
                         SET attempts = attempts + 1, last_attempt_at = now(), \
                             last_status_code = $2, last_error = $3, \
                             next_attempt_at = now() + ($4::int * interval '1 second') \
                         WHERE id = $1 AND status = 'pending'",
                    )
                    .bind(d.delivery_id)
                    .bind(status)
                    .bind(&error)
                    .bind(*delay)
                    .execute(pool)
                    .await?;
                    tracing::warn!(delivery_id = %d.delivery_id, attempt = attempt_no, retry_in_secs = *delay, error = %error, "webhook failed; will retry");
                }
                None => {
                    // Retry budget exhausted: dead letter. The event itself stays in
                    // webhook_events, so a business can still reconcile from the log.
                    sqlx::query(
                        "UPDATE webhook_deliveries \
                         SET status = 'failed', attempts = attempts + 1, last_attempt_at = now(), \
                             last_status_code = $2, last_error = $3 \
                         WHERE id = $1 AND status = 'pending'",
                    )
                    .bind(d.delivery_id)
                    .bind(status)
                    .bind(&error)
                    .execute(pool)
                    .await?;
                    tracing::error!(delivery_id = %d.delivery_id, attempts = attempt_no, error = %error, "webhook exhausted retries; dead-lettered");
                }
            }
        }
    }
    Ok(())
}

/// HMAC-SHA256 over "<unix timestamp>.<raw body>", hex encoded.
/// Receivers recompute it with their endpoint secret, compare in constant time, and reject
/// timestamps more than ~5 minutes old (replay protection).
pub fn sign(secret: &str, timestamp: i64, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sign;

    // Expected value computed independently with Python's hmac/hashlib:
    //   hmac.new(b"whsec_test_secret", b"1700000000." + body, sha256).hexdigest()
    #[test]
    fn signature_matches_reference_hmac() {
        let body = r#"{"id":"evt_1","type":"invoice.paid"}"#;
        assert_eq!(
            sign("whsec_test_secret", 1_700_000_000, body),
            "46dc069361a7691082640523fa33fe5d6c8c88d5f5e257cdf8cf11539d166595"
        );
    }

    #[test]
    fn signature_changes_with_timestamp_or_body() {
        let a = sign("s", 1, "x");
        assert_ne!(a, sign("s", 2, "x"));
        assert_ne!(a, sign("s", 1, "y"));
        assert_ne!(a, sign("t", 1, "x"));
    }
}