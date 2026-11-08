use axum::{routing::post, Json, Router};
use serde::Deserialize;
use std::net::SocketAddr;

#[derive(Deserialize)]
struct Payload {
    message: String,
}

async fn receive(Json(payload): Json<Payload>) -> String {
    println!("Got: {}", payload.message);
    "OK".to_string()
}

#[tokio::main]
async fn main() {
    let app = Router::new().route("/data", post(receive));
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    println!("HTTP server listening on http://{addr}/data");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}