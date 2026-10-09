use axum::extract::{FromRequest, FromRequestParts};

use crate::error::AppError;

// Thin wrappers around axum's Json / Path / Query whose rejections are AppError,
// so malformed input produces our standard JSON error instead of plain text.

#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(AppError))]
pub struct ApiJson<T>(pub T);

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(AppError))]
pub struct ApiPath<T>(pub T);

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(AppError))]
pub struct ApiQuery<T>(pub T);