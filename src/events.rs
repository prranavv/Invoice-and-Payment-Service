use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;

/// Transactional outbox write. Records the event and one pending delivery per active
/// endpoint using the CALLER'S transaction, so the event exists if and only if the state
/// change committed. No HTTP happens here; a background worker delivers later.
pub async fn emit(
    tx: &mut Transaction<'_, Postgres>,
    business_id: Uuid,
    invoice_id: Uuid,
    event_type: &str,
    payload: Value,
) -> Result<(), AppError> {
    let event_id: Uuid = sqlx::query_scalar(
        "INSERT INTO webhook_events (business_id, type, invoice_id, payload) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(business_id)
    .bind(event_type)
    .bind(invoice_id)
    .bind(payload)
    .fetch_one(&mut **tx)
    .await?;

    sqlx::query(
        "INSERT INTO webhook_deliveries (event_id, endpoint_id) \
         SELECT $1, id FROM webhook_endpoints WHERE business_id = $2 AND is_active",
    )
    .bind(event_id)
    .bind(business_id)
    .execute(&mut **tx)
    .await?;

    Ok(())
}