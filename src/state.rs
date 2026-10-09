use sqlx::PgPool;

use crate::psp::PspClient;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub psp: PspClient,
}