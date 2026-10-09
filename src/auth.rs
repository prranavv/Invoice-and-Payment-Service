use axum::{extract::FromRequestParts, http::header::AUTHORIZATION, http::request::Parts};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

/// The authenticated business. Add this as a handler argument to require a valid key.
/// Every query in a handler must be scoped by this id; it is the tenant boundary.
pub struct AuthBusiness(pub Uuid);

impl FromRequestParts<AppState> for AuthBusiness {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(AppError::Unauthorized)?;
        let (scheme, key) = header.split_once(' ').ok_or(AppError::Unauthorized)?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(AppError::Unauthorized);
        }
        let key = key.trim();
        let prefix = parse_prefix(key).ok_or(AppError::Unauthorized)?;

        let row: Option<(Uuid, Vec<u8>)> = sqlx::query_as(
            "SELECT business_id, key_hash FROM api_keys \
             WHERE prefix = $1 AND revoked_at IS NULL",
        )
        .bind(prefix)
        .fetch_optional(&state.pool)
        .await?;
        let (business_id, stored_hash) = row.ok_or(AppError::Unauthorized)?;

        // Compare hashes in constant time. Unknown prefix, wrong secret and revoked key
        // all return the same 401, so a caller learns nothing about which part failed.
        let presented = Sha256::digest(key.as_bytes());
        if bool::from(presented.as_slice().ct_eq(stored_hash.as_slice())) {
            Ok(AuthBusiness(business_id))
        } else {
            Err(AppError::Unauthorized)
        }
    }
}

/// "sk_demo0001_abc..." -> Some("demo0001"). The random part must be non-empty.
fn parse_prefix(key: &str) -> Option<&str> {
    if key.len() > 128 {
        return None; // don't hash arbitrarily large input
    }
    let rest = key.strip_prefix("sk_")?;
    let (prefix, secret) = rest.split_once('_')?;
    if prefix.is_empty() || secret.is_empty() {
        return None;
    }
    Some(prefix)
}