use axum::{extract::State, http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{page_bounds, List};
use crate::{
    auth::AuthBusiness,
    error::AppError,
    extract::{ApiJson, ApiPath, ApiQuery},
    state::AppState,
};

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Customer {
    pub id: Uuid,
    pub name: String,
    pub email: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCustomer {
    name: String,
    email: String,
}

#[derive(Deserialize)]
pub struct ListParams {
    limit: Option<i64>,
    offset: Option<i64>,
}

pub async fn create(
    State(state): State<AppState>,
    AuthBusiness(business_id): AuthBusiness,
    ApiJson(body): ApiJson<CreateCustomer>,
) -> Result<(StatusCode, Json<Customer>), AppError> {
    let name = body.name.trim();
    let email = body.email.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(AppError::validation("name must be 1-200 characters"));
    }
    if !looks_like_email(email) {
        return Err(AppError::validation("email is not a valid address"));
    }

    let customer = sqlx::query_as::<_, Customer>(
        "INSERT INTO customers (business_id, name, email) VALUES ($1, $2, $3) \
         RETURNING id, name, email, created_at",
    )
    .bind(business_id)
    .bind(name)
    .bind(email)
    .fetch_one(&state.pool)
    .await?;

    Ok((StatusCode::CREATED, Json(customer)))
}

pub async fn get_one(
    State(state): State<AppState>,
    AuthBusiness(business_id): AuthBusiness,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<Customer>, AppError> {
    // Scoped by business_id: another business's customer is indistinguishable from
    // a nonexistent one (404, not 403).
    let customer = sqlx::query_as::<_, Customer>(
        "SELECT id, name, email, created_at FROM customers \
         WHERE id = $1 AND business_id = $2",
    )
    .bind(id)
    .bind(business_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound("customer"))?;

    Ok(Json(customer))
}

pub async fn list(
    State(state): State<AppState>,
    AuthBusiness(business_id): AuthBusiness,
    ApiQuery(params): ApiQuery<ListParams>,
) -> Result<Json<List<Customer>>, AppError> {
    let (limit, offset) = page_bounds(params.limit, params.offset)?;

    let data = sqlx::query_as::<_, Customer>(
        "SELECT id, name, email, created_at FROM customers \
         WHERE business_id = $1 \
         ORDER BY created_at DESC, id DESC \
         LIMIT $2 OFFSET $3",
    )
    .bind(business_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(List { data }))
}

/// Intentionally loose: the only real validation of an email is sending to it.
fn looks_like_email(e: &str) -> bool {
    if e.len() > 254 || e.chars().any(char::is_whitespace) {
        return false;
    }
    match e.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !domain.contains('@')
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        }
        None => false,
    }
}