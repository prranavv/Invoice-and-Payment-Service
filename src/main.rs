mod auth;
mod database;
mod error;
mod events;
mod extract;
mod payments;
mod psp;
mod router;
mod state;
mod webhooks;

use tracing_subscriber::EnvFilter;

use crate::{
    database::establish_connection, psp::PspClient, router::init_router, state::AppState,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgresql://postgres:change_me@localhost:5432/appdb".to_string());
    let psp_url = std::env::var("PSP_URL").unwrap_or_else(|_| "http://mock-psp:9000".to_string());

    let pool = establish_connection(&database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    webhooks::spawn_worker(pool.clone())?;

    let state = AppState {
        pool,
        psp: PspClient::new(psp_url)?,
    };

    tokio::spawn(payments::run_sweeper(state.clone()));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await?;
    let app = init_router(state);

    tracing::info!("listening on port 8000");
    axum::serve(listener, app).await?;
    Ok(())
}