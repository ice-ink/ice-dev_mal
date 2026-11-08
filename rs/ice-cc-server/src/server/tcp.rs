//! The TCP echo server.
//!
//! This is the simplest possible "real" network server: it accepts connections
//! and sends back a copy of every line it receives. It is useful for testing
//! that the control center can actually bind a socket and pass traffic.

use std::io;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::log::LogSink;

/// Run the TCP echo server until `cancel` is triggered.
///
/// `listener` is already bound and listening: the [`ServerManager`] did that
/// synchronously so it could report "port already in use" immediately. This
/// function's only jobs are to accept connections and echo lines back.
pub async fn run(listener: TcpListener, cancel: CancellationToken, log: LogSink) -> io::Result<()> {
    let addr = listener.local_addr()?;
    log.log(format!("[tcp] listening on {addr}"));

    loop {
        // `tokio::select!` races two futures against each other:
        //
        //   1. `cancel.cancelled()` — resolves when the manager asks us to stop.
        //   2. `listener.accept()` — resolves when a client connects.
        //
        // Whichever completes first wins, and the other is automatically
        // dropped. This gives us a clean, non-blocking way to watch for both
        // "shut down" and "a new connection" at the same time.
        tokio::select! {
            _ = cancel.cancelled() => {
                log.log("[tcp] shutting down");
                return Ok(());
            }
            accepted = listener.accept() => {
                let (socket, peer) = accepted?;
                log.log(format!("[tcp] client connected: {peer}"));

                // Each client gets its own task so many clients can be served
                // at once without one slow client blocking the others.
                let log = log.clone();
                tokio::spawn(async move {
                    handle_client(socket, peer, log).await;
                });
            }
        }
    }
}

/// Echo lines for one client until it disconnects or an error occurs.
async fn handle_client(socket: tokio::net::TcpStream, peer: std::net::SocketAddr, log: LogSink) {
    // `into_split` separates the socket into a read half and a write half so we
    // can read from one and write to the other at the same time.
    let (reader, mut writer) = socket.into_split();

    // `BufReader::lines` produces one future per line; `next_line()` yields
    // `Ok(Some(line))` per line and `Ok(None)` when the client closes.
    let mut lines = BufReader::new(reader).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        log.log(format!("[{peer}] {line}"));

        // Echo the line back with a newline so the client can frame responses.
        // If the write fails the client has gone away, so stop serving it.
        if writer
            .write_all(format!("echo: {line}\n").as_bytes())
            .await
            .is_err()
        {
            break;
        }
    }

    log.log(format!("[tcp] client disconnected: {peer}"));
}
