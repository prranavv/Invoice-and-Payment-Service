use std::time::Duration;

use anyhow::Ok;
use sqlx::{PgPool, postgres::PgPoolOptions};



pub async fn establish_connection(db_url:&str)-> anyhow::Result<PgPool>{
    let pool = PgPoolOptions::new()
                    .max_connections(10)
                    .acquire_timeout(Duration::from_secs(10))
                    .idle_timeout(Duration::from_secs(10))
                    .connect(db_url)
                    .await?;
    Ok(pool)
}