//! # ice-cc-server
//!
//! A tiny terminal "control center": one binary that opens a full-screen TUI
//! from which you can start, stop, and check the status of two servers that run
//! *in-process* as background tasks:
//!
//! * a **TCP echo server** (default port 7878), and
//! * an **HTTP server** with a single `POST /data` endpoint (default port 3000).
//!
//! ## Thread and runtime model (read this first)
//!
//! The program has one tokio **runtime** and two distinct kinds of work:
//!
//! * The **main thread** runs the terminal UI. The UI is deliberately
//!   *synchronous*: it reads keystrokes and draws frames, and calls plain
//!   functions like `manager.start(...)` / `manager.stop(...)`.
//! * The **runtime's worker threads** run the servers, which are *asynchronous*
//!   (`async fn`) and `await` their network operations.
//!
//! The bridge between the two worlds is a [`tokio::runtime::Handle`], held by
//! the [`ServerManager`]. `handle.spawn(future)` is a synchronous call that
//! schedules a server onto the runtime, so the UI never has to write `async`.
//! Servers send log messages back to the UI through a channel (see `log`).
//!
//! See `doc/architecture.md` in the repository root for a fuller walkthrough.

mod log;
mod server;
mod ui;

use std::time::Duration;

use crate::log::LogSink;
use crate::server::ServerManager;

/// The program entry point.
///
/// `Box<dyn std::error::Error>` is the idiomatic "any error type" return for a
/// small application: every error type in the standard library implements
/// `std::error::Error`, so `?` works on all of them.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1) Build the async runtime. Its worker threads start immediately and run
    //    in the background for the whole program. `enable_all` turns on the
    //    timer and I/O drivers that network code needs.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    // `handle` is a cheap clone-able pointer to the runtime; the manager uses it
    // to spawn server tasks.
    let handle = runtime.handle().clone();

    // 2) Create the log channel: servers send `LogLine`s here, the UI drains it.
    let (log, log_rx) = LogSink::new();

    // 3) The manager owns the running servers and the runtime handle.
    let mut manager = ServerManager::new(handle, log);

    // 4) Run the TUI. This blocks the main thread until the user quits.
    ui::run(&mut manager, log_rx)?;

    // 5) Clean shutdown: cancel every server, then give the runtime up to two
    //    seconds to let in-flight work finish before forcing it down.
    manager.stop_all();
    runtime.shutdown_timeout(Duration::from_secs(2));

    Ok(())
}
