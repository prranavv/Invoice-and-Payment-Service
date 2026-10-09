use axum::{extract::State, http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{auth::AuthBusiness, error::AppError, extract::ApiJson, state::AppState};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateEndpoint {
    url: String,
}

/// The only response that ever contains the signing secret.
#[derive(Serialize, sqlx::FromRow)]
pub struct EndpointCreated {
    id: Uuid,
    url: String,
    secret: String,
    is_active: bool,
    created_at: DateTime<Utc>,
}

pub async fn create(
    State(state): State<AppState>,
    AuthBusiness(business_id): AuthBusiness,
    ApiJson(body): ApiJson<CreateEndpoint>,
) -> Result<(StatusCode, Json<EndpointCreated>), AppError> {
    let raw = body.url.trim();
    if raw.is_empty() || raw.len() > 2048 {
        return Err(AppError::validation("url must be 1-2048 characters"));
    }
    let parsed = reqwest::Url::parse(raw)
        .map_err(|_| AppError::validation("url is not a valid absolute URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(AppError::validation("url must be an http or https URL with a host"));
    }

    // 244 bits from the OS CSPRNG (uuid v4 draws from getrandom), so no extra dependency.
    // The secret is stored as-is because the worker must read it to sign; it is returned
    // once, here, and no endpoint returns it again.
    let secret = format!(
        "whsec_{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    );

    let endpoint = sqlx::query_as::<_, EndpointCreated>(
        "INSERT INTO webhook_endpoints (business_id, url, secret) VALUES ($1, $2, $3) \
         RETURNING id, url, secret, is_active, created_at",
    )
    .bind(business_id)
    .bind(parsed.as_str())
    .bind(&secret)
    .fetch_one(&state.pool)
    .await?;

    Ok((StatusCode::CREATED, Json(endpoint)))
}