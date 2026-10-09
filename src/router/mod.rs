mod customers;
mod invoices;
mod pay;
mod webhook_endpoints;

use axum::{
    routing::{get, post},
    Router,
};
use serde::Serialize;

use crate::{error::AppError, state::AppState};

pub fn init_router(state: AppState) -> Router {
    Router::new()
        .route("/customers", post(customers::create).get(customers::list))
        .route("/customers/{id}", get(customers::get_one))
        .route("/invoices", post(invoices::create).get(invoices::list))
        .route("/invoices/{id}", get(invoices::get_one))
        .route("/invoices/{id}/finalize", post(invoices::finalize))
        .route("/invoices/{id}/void", post(invoices::void))
        .route("/invoices/{id}/pay", post(pay::pay))
        .route("/webhook_endpoints", post(webhook_endpoints::create))
        .fallback(not_found)
        .with_state(state)
}

async fn not_found() -> AppError {
    AppError::NotFound("route")
}

/// List responses are wrapped so fields (e.g. a next-page cursor) can be added later
/// without breaking clients.
#[derive(Serialize)]
pub struct List<T> {
    pub data: Vec<T>,
}

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 100;

/// Validates ?limit= and ?offset=. Offset pagination is a deliberate simplification
/// (see DESIGN.md "what I cut": cursor pagination).
fn page_bounds(limit: Option<i64>, offset: Option<i64>) -> Result<(i64, i64), AppError> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    let offset = offset.unwrap_or(0);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(AppError::validation(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    if offset < 0 {
        return Err(AppError::validation("offset must be 0 or greater"));
    }
    Ok((limit, offset))
}