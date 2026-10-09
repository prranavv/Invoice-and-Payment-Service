use axum::{extract::State, http::StatusCode, Json};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::{page_bounds, List};
use crate::{
    auth::AuthBusiness,
    error::AppError,
    events,
    extract::{ApiJson, ApiPath, ApiQuery},
    state::AppState,
};

const MAX_LINE_ITEMS: usize = 100;
const STATES: [&str; 4] = ["draft", "open", "paid", "void"];


#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Invoice {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub state: String,
    pub total_cents: i64,
    pub due_date: NaiveDate,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub paid_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct LineItem {
    pub description: String,
    pub quantity: i32,
    pub unit_amount_cents: i64,
    pub amount_cents: i64,
}

#[derive(Debug, Serialize)]
pub struct InvoiceDetail {
    #[serde(flatten)]
    pub invoice: Invoice,
    pub line_items: Vec<LineItem>,
}


// deny_unknown_fields: a client sending its own "total_cents" gets an error instead of
// having it silently ignored. The server is the only thing that computes totals.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateInvoice {
    customer_id: Uuid,
    due_date: NaiveDate,
    line_items: Vec<CreateLineItem>,
}

// quantity and unit_amount_cents are integer types, so serde rejects 10.5 or 1e2.
// No float ever enters the money path.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateLineItem {
    description: String,
    quantity: i32,
    unit_amount_cents: i64,
}

#[derive(Deserialize)]
pub struct ListParams {
    state: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}


pub async fn create(
    State(state): State<AppState>,
    AuthBusiness(business_id): AuthBusiness,
    ApiJson(body): ApiJson<CreateInvoice>,
) -> Result<(StatusCode, Json<InvoiceDetail>), AppError> {
    validate_line_items(&body.line_items)?;
    let total_cents = compute_total(&body.line_items)?;
    if total_cents == 0 {
        return Err(AppError::validation(
            "invoice total must be greater than zero",
        ));
    }

    let mut tx = state.pool.begin().await?;

    // The customer must belong to the authenticated business. (The composite FK on
    // invoices enforces this too; checking first gives a clean 404 instead of a 500.)
    let customer: Option<(i32,)> =
        sqlx::query_as("SELECT 1 FROM customers WHERE id = $1 AND business_id = $2")
            .bind(body.customer_id)
            .bind(business_id)
            .fetch_optional(&mut *tx)
            .await?;
    if customer.is_none() {
        return Err(AppError::NotFound("customer"));
    }

    let invoice_id: Uuid = sqlx::query_scalar(
        "INSERT INTO invoices (business_id, customer_id, total_cents, due_date) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(business_id)
    .bind(body.customer_id)
    .bind(total_cents)
    .bind(body.due_date)
    .fetch_one(&mut *tx)
    .await?;

    let positions: Vec<i32> = (0..body.line_items.len() as i32).collect();
    let descriptions: Vec<String> = body
        .line_items
        .iter()
        .map(|i| i.description.trim().to_string())
        .collect();
    let quantities: Vec<i32> = body.line_items.iter().map(|i| i.quantity).collect();
    let unit_amounts: Vec<i64> = body.line_items.iter().map(|i| i.unit_amount_cents).collect();

    sqlx::query(
        "INSERT INTO invoice_line_items (invoice_id, position, description, quantity, unit_amount_cents) \
         SELECT $1::uuid, t.position, t.description, t.quantity, t.unit_amount_cents \
         FROM UNNEST($2::int[], $3::text[], $4::int[], $5::bigint[]) \
              AS t(position, description, quantity, unit_amount_cents)",
    )
    .bind(invoice_id)
    .bind(&positions)
    .bind(&descriptions)
    .bind(&quantities)
    .bind(&unit_amounts)
    .execute(&mut *tx)
    .await?;

    // Outbox write inside the same transaction: the invoice.created event exists if and
    // only if the invoice does. Delivery happens later, in a background worker.
    events::emit(
        &mut tx,
        business_id,
        invoice_id,
        "invoice.created",
        json!({
            "invoice_id": invoice_id,
            "customer_id": body.customer_id,
            "state": "draft",
            "total_cents": total_cents,
            "due_date": body.due_date,
        }),
    )
    .await?;

    tx.commit().await?;

    let detail = fetch_invoice(&state.pool, invoice_id, business_id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("invoice vanished after commit")))?;

    Ok((StatusCode::CREATED, Json(detail)))
}

pub async fn get_one(
    State(state): State<AppState>,
    AuthBusiness(business_id): AuthBusiness,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<InvoiceDetail>, AppError> {
    let detail = fetch_invoice(&state.pool, id, business_id)
        .await?
        .ok_or(AppError::NotFound("invoice"))?;
    Ok(Json(detail))
}

pub async fn list(
    State(state): State<AppState>,
    AuthBusiness(business_id): AuthBusiness,
    ApiQuery(params): ApiQuery<ListParams>,
) -> Result<Json<List<Invoice>>, AppError> {
    let (limit, offset) = page_bounds(params.limit, params.offset)?;

    if let Some(s) = &params.state {
        if !STATES.contains(&s.as_str()) {
            return Err(AppError::validation(format!(
                "state must be one of: {}",
                STATES.join(", ")
            )));
        }
    }

    let data = sqlx::query_as::<_, Invoice>(
        "SELECT id, customer_id, state, total_cents, due_date, created_at, updated_at, paid_at \
         FROM invoices \
         WHERE business_id = $1 AND ($2::text IS NULL OR state = $2::text) \
         ORDER BY created_at DESC, id DESC \
         LIMIT $3 OFFSET $4",
    )
    .bind(business_id)
    .bind(params.state.as_deref())
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(List { data }))
}


async fn fetch_invoice(
    pool: &PgPool,
    id: Uuid,
    business_id: Uuid,
) -> Result<Option<InvoiceDetail>, AppError> {
    let Some(invoice) = sqlx::query_as::<_, Invoice>(
        "SELECT id, customer_id, state, total_cents, due_date, created_at, updated_at, paid_at \
         FROM invoices WHERE id = $1 AND business_id = $2",
    )
    .bind(id)
    .bind(business_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };

    // Line items never change after creation, so reading them in a separate
    // statement can't produce an inconsistent view.
    let line_items = sqlx::query_as::<_, LineItem>(
        "SELECT description, quantity, unit_amount_cents, amount_cents \
         FROM invoice_line_items WHERE invoice_id = $1 ORDER BY position",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    Ok(Some(InvoiceDetail { invoice, line_items }))
}

fn validate_line_items(items: &[CreateLineItem]) -> Result<(), AppError> {
    if items.is_empty() {
        return Err(AppError::validation("line_items must not be empty"));
    }
    if items.len() > MAX_LINE_ITEMS {
        return Err(AppError::validation(format!(
            "line_items may contain at most {MAX_LINE_ITEMS} entries"
        )));
    }
    for (i, item) in items.iter().enumerate() {
        let desc = item.description.trim();
        if desc.is_empty() || desc.chars().count() > 500 {
            return Err(AppError::validation(format!(
                "line_items[{i}].description must be 1-500 characters"
            )));
        }
        if item.quantity < 1 {
            return Err(AppError::validation(format!(
                "line_items[{i}].quantity must be at least 1"
            )));
        }
        if item.unit_amount_cents < 0 {
            return Err(AppError::validation(format!(
                "line_items[{i}].unit_amount_cents must not be negative"
            )));
        }
    }
    Ok(())
}

/// The only place an invoice total is computed. Integer cents, checked arithmetic:
/// overflow becomes a 422 instead of silently wrapping.
fn compute_total(items: &[CreateLineItem]) -> Result<i64, AppError> {
    items
        .iter()
        .try_fold(0i64, |acc, item| {
            let line = i64::from(item.quantity).checked_mul(item.unit_amount_cents)?;
            acc.checked_add(line)
        })
        .ok_or_else(|| AppError::validation("invoice total exceeds the maximum amount"))
}


pub async fn finalize(
    State(state): State<AppState>,
    AuthBusiness(business_id): AuthBusiness,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<InvoiceDetail>, AppError> {
    change_state(&state, business_id, id, "open").await.map(Json)
}

pub async fn void(
    State(state): State<AppState>,
    AuthBusiness(business_id): AuthBusiness,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<InvoiceDetail>, AppError> {
    change_state(&state, business_id, id, "void").await.map(Json)
}


/// The states an invoice may move to `to` from. The single place that encodes the
/// manual transitions; /pay handles open -> paid itself.
fn allowed_sources(to: &str) -> &'static [&'static str] {
    match to {
        "open" => &["draft"],
        "void" => &["draft", "open"],
        _ => &[],
    }
}

async fn change_state(
    state: &AppState,
    business_id: Uuid,
    id: Uuid,
    to: &'static str,
) -> Result<InvoiceDetail, AppError> {
    let mut tx = state.pool.begin().await?;

    // Same row lock /pay takes in its first transaction, so a void cannot slip in between
    // /pay's check and its attempt insert.
    let current: Option<(String,)> = sqlx::query_as(
        "SELECT state FROM invoices WHERE id = $1 AND business_id = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(business_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (current,) = current.ok_or(AppError::NotFound("invoice"))?;

    if !allowed_sources(to).contains(&current.as_str()) {
        return Err(AppError::conflict(
            "invalid_state_transition",
            format!("cannot move invoice from {current} to {to}"),
        ));
    }

    if to == "void" {
        // A pending/unknown attempt may have charged the card. Voiding now
        // would leave money taken for an invoice we have abandoned.
        let in_flight: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM payment_attempts \
             WHERE invoice_id = $1 AND status IN ('pending', 'unknown') LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        if in_flight.is_some() {
            return Err(AppError::conflict(
                "payment_in_progress",
                format!("a payment for this invoice is in progress; it cannot be moved to {to} yet"),
            ));
        }
    }

    sqlx::query("UPDATE invoices SET state = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(to)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    fetch_invoice(&state.pool, id, business_id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("invoice vanished after commit")))
}