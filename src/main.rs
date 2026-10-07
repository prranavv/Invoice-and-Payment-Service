mod router;
mod database;

use crate::{database::establish_connection, router::init_router};

#[tokio::main]
async fn main(){
    let pool = establish_connection("postgresql://postgres:change_me@localhost:5432/appdb").await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await.unwrap();
    let app = init_router();
    println!("Listening on Port 8000");
    axum::serve(listener, app).await.unwrap()
}

