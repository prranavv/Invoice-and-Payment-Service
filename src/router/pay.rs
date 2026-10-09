use axum::{extract::State, http::HeaderMap};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth::AuthBusiness,
    error::AppError,
    extract::{ApiJson, ApiPath},
    payments::{self, Outcome},
    state::AppState,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayRequest {
    card_token: String,
}

pub async fn pay(
    State(state): State<AppState>,
    AuthBusiness(business_id): AuthBusiness,
    ApiPath(invoice_id): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<PayRequest>,
) -> Result<Outcome, AppError> {
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::BadRequest("Idempotency-Key header is required".to_string()))?;
    if key.len() > 255 {
        return Err(AppError::validation("Idempotency-Key must be at most 255 characters"));
    }

    let token = body.card_token.trim();
    if token.is_empty() || token.len() > 100 {
        return Err(AppError::validation("card_token must be 1-100 characters"));
    }

    payments::start_payment(&state, business_id, invoice_id, key, token).await
}