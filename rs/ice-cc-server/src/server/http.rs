//! The HTTP server.
//!
//! This uses [axum](https://docs.rs/axum), a popular Rust web framework. It
//! exposes a single endpoint, `POST /data`, that logs the JSON body it receives
//! and answers `OK`. Like the TCP server, it is started and stopped from the
//! control center.

use std::io;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::log::LogSink;

/// The JSON body we expect clients to POST.
///
/// The `#[derive(Deserialize)]` attribute asks the `serde` library to generate,
/// at compile time, the code that turns JSON like `{"message":"hi"}` into this
/// struct. We only declare the field we care about; extra JSON keys are ignored.
#[derive(Deserialize)]
struct Payload {
    message: String,
}

/// Handle `POST /data`: log the message and acknowledge.
///
/// `State(log)` is axum's way of injecting shared application state into a
/// handler without global variables: whatever value we gave the router via
/// `.with_state(...)` is passed here. `Json(payload)` parses the request body.
/// Returning a `&'static str` is a valid axum response (it becomes `text/plain`).
async fn receive(State(log): State<LogSink>, Json(payload): Json<Payload>) -> &'static str {
    log.log(format!("[http] POST /data: {}", payload.message));
    "OK"
}

/// Run the HTTP server until `cancel` is triggered.
pub async fn run(listener: TcpListener, cancel: CancellationToken, log: LogSink) -> io::Result<()> {
    let addr = listener.local_addr()?;

    // Build the router, then attach our shared log sink as application state so
    // the handler can reach it. `post(receive)` registers `receive` for the
    // `/data` path and only for POST requests.
    let app = Router::new()
        .route("/data", post(receive))
        .with_state(log.clone());

    log.log(format!("[http] listening on http://{addr}/data"));

    // `with_graceful_shutdown` tells axum to stop accepting new connections and
    // let in-flight requests finish when the given future resolves. The future
    // here simply waits for our cancellation token to be cancelled.
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await?;

    log.log("[http] shutting down");
    Ok(())
}
