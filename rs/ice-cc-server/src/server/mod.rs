//! Server management: starting, stopping, and checking the status of the TCP
//! and HTTP servers.
//!
//! There is an important design choice here that keeps the rest of the program
//! simple:
//!
//! * The **servers themselves are asynchronous** — they use tokio and `await`
//!   their network operations so they never block a thread while waiting.
//! * The **[`ServerManager`] is synchronous** — the UI can call `start`, `stop`,
//!   and `status` without writing any `async` code at all.
//!
//! The trick is that `ServerManager` holds a [`tokio::runtime::Handle`]. Calling
//! `handle.spawn(future)` is a *synchronous* function: it just schedules the
//! future to run on the runtime's worker threads (which were started when the
//! runtime was built in `main`) and immediately returns a [`JoinHandle`]. So the
//! UI thread can start and stop servers with plain function calls while the
//! runtime does the actual asynchronous work in the background.

mod http;
mod tcp;

use std::time::Instant;

use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::log::LogSink;

/// The two kinds of server this control center can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerKind {
    /// The TCP echo server.
    Tcp,
    /// The HTTP server exposing `POST /data`.
    Http,
}

impl ServerKind {
    /// Short lowercase name, used in status output and log prefixes.
    pub fn name(self) -> &'static str {
        match self {
            ServerKind::Tcp => "tcp",
            ServerKind::Http => "http",
        }
    }

    /// The port each server listens on unless the user overrides it.
    pub fn default_port(self) -> u16 {
        match self {
            ServerKind::Tcp => 7878,
            ServerKind::Http => 3000,
        }
    }
}

/// The state of a server as far as the UI is concerned.
#[derive(Debug, Clone, Copy)]
pub enum ServerStatus {
    /// Not currently running.
    Stopped,
    /// Running and accepting connections on `port` since `since`.
    Running { port: u16, since: Instant },
}

/// A server that is currently running.
///
/// The two fields are what make control possible:
///
/// * `join` is the async task itself. `JoinHandle::is_finished` lets us detect
///   when a server has crashed unexpectedly.
/// * `cancel` is a shutdown signal. Calling `cancel.cancel()` makes the server's
///   accept loop notice and wind down gracefully instead of being killed
///   mid-request.
struct RunningServer {
    /// The actual bound port (differs from the requested port when 0 is used).
    port: u16,
    /// When the server was started, for the "+12s" uptime in the UI.
    since: Instant,
    /// Handle to the spawned server task.
    join: JoinHandle<()>,
    /// Shutdown signal handed to the server task.
    cancel: CancellationToken,
}

/// Owns the runtime handle and the currently running servers, and exposes a
/// synchronous control API to the UI.
pub struct ServerManager {
    /// A clone of the tokio runtime handle, used to spawn server tasks.
    handle: Handle,
    /// The running TCP server, if any.
    tcp: Option<RunningServer>,
    /// The running HTTP server, if any.
    http: Option<RunningServer>,
    /// Shared log channel; a clone is handed to each spawned server.
    log: LogSink,
}

impl ServerManager {
    /// Create a manager bound to a runtime handle.
    pub fn new(handle: Handle, log: LogSink) -> Self {
        Self {
            handle,
            tcp: None,
            http: None,
            log,
        }
    }

    /// Start a server of the given kind, optionally on a specific port.
    ///
    /// Binding happens *here*, synchronously, so that errors like "port already
    /// in use" are reported to the caller immediately rather than surfacing as
    /// an asynchronous log line after the UI has already said "started".
    pub fn start(&mut self, kind: ServerKind, port: Option<u16>) -> Result<(), String> {
        // If the slot is already occupied, either refuse (still running) or
        // clear it (previous task already ended, e.g. it crashed).
        if let Some(running) = self.get(kind) {
            if running.join.is_finished() {
                self.slot_mut(kind).take();
            } else {
                return Err(format!("{} is already running", kind.name()));
            }
        }

        let requested = port.unwrap_or_else(|| kind.default_port());

        // Bind with the standard library first so we can fail fast. The OS
        // begins listening immediately, so clients can connect even before the
        // tokio task below calls `accept`.
        let std_listener = std::net::TcpListener::bind(("0.0.0.0", requested))
            .map_err(|e| format!("cannot bind {} to 0.0.0.0:{requested}: {e}", kind.name()))?;

        // tokio requires the socket to be in non-blocking mode before it takes
        // ownership of it.
        std_listener
            .set_nonblocking(true)
            .map_err(|e| format!("cannot set non-blocking mode: {e}"))?;

        // Read back the port the OS actually bound. This matters when `requested`
        // is 0 ("give me any free port"), which the tests rely on. We read it
        // from the standard-library listener before handing it to tokio.
        let bound_port = std_listener
            .local_addr()
            .map_err(|e| format!("cannot read bound address: {e}"))?
            .port();

        // A fresh cancellation token per server: stopping one must never affect
        // the other. The server receives a clone; we keep the original.
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let log = self.log.clone();

        // Schedule the server future onto the runtime. This returns immediately.
        // `async move` means the block takes ownership of the variables it uses.
        let join = self.handle.spawn(async move {
            // Converting the socket to a tokio listener must happen *here*,
            // inside the runtime: tokio needs a running reactor to register the
            // socket with its I/O driver. (This is why we could not do it back
            // in `start`, which runs on the UI thread outside any reactor.)
            let listener = match tokio::net::TcpListener::from_std(std_listener) {
                Ok(listener) => listener,
                Err(e) => {
                    log.log(format!(
                        "[{}] cannot hand listener to tokio: {e}",
                        kind.name()
                    ));
                    return;
                }
            };

            // Each arm fully awaits its server, so both arms produce the same
            // type `()`. (The two futures have different concrete types, which
            // is why we cannot write a single `match { a, b }.await`.)
            match kind {
                ServerKind::Tcp => {
                    // `log.clone()` hands the server its own handle while leaving
                    // ours intact for the error message below.
                    if let Err(e) = tcp::run(listener, task_cancel, log.clone()).await {
                        log.log(format!("[tcp] server error: {e}"));
                    }
                }
                ServerKind::Http => {
                    if let Err(e) = http::run(listener, task_cancel, log.clone()).await {
                        log.log(format!("[http] server error: {e}"));
                    }
                }
            }
        });

        *self.slot_mut(kind) = Some(RunningServer {
            port: bound_port,
            since: Instant::now(),
            join,
            cancel,
        });
        Ok(())
    }

    /// Stop a running server by signalling its cancellation token.
    pub fn stop(&mut self, kind: ServerKind) -> Result<(), String> {
        let running = self
            .slot_mut(kind)
            .take()
            .ok_or_else(|| format!("{} is not running", kind.name()))?;
        // `cancel` wakes the server's select loop so it shuts down gracefully.
        running.cancel.cancel();
        Ok(())
    }

    /// Current status of one server.
    pub fn status(&self, kind: ServerKind) -> ServerStatus {
        match self.get(kind) {
            Some(running) => {
                if running.join.is_finished() {
                    // The task ended on its own (e.g. an accept error): report
                    // it as stopped rather than claiming it is still up.
                    ServerStatus::Stopped
                } else {
                    ServerStatus::Running {
                        port: running.port,
                        since: running.since,
                    }
                }
            }
            None => ServerStatus::Stopped,
        }
    }

    /// Status of both servers, in a fixed order for the UI's status panel.
    pub fn list(&self) -> [(ServerKind, ServerStatus); 2] {
        [
            (ServerKind::Tcp, self.status(ServerKind::Tcp)),
            (ServerKind::Http, self.status(ServerKind::Http)),
        ]
    }

    /// Stop everything. Called once on program exit.
    pub fn stop_all(&mut self) {
        if let Some(running) = self.tcp.take() {
            running.cancel.cancel();
        }
        if let Some(running) = self.http.take() {
            running.cancel.cancel();
        }
    }

    // -- internal helpers -----------------------------------------------------

    /// Mutable access to whichever slot holds `kind`'s server.
    fn slot_mut(&mut self, kind: ServerKind) -> &mut Option<RunningServer> {
        match kind {
            ServerKind::Tcp => &mut self.tcp,
            ServerKind::Http => &mut self.http,
        }
    }

    /// Immutable access to whichever slot holds `kind`'s server.
    fn get(&self, kind: ServerKind) -> Option<&RunningServer> {
        match kind {
            ServerKind::Tcp => self.tcp.as_ref(),
            ServerKind::Http => self.http.as_ref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::time::Duration;

    /// Build a manager plus a live runtime, mirroring what `main` does.
    fn manager_with_runtime() -> (tokio::runtime::Runtime, ServerManager) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let handle = runtime.handle().clone();
        let (log, _rx) = LogSink::new();
        (runtime, ServerManager::new(handle, log))
    }

    /// Extract the bound port from a running status, or panic.
    fn running_port(status: ServerStatus) -> u16 {
        match status {
            ServerStatus::Running { port, .. } => port,
            ServerStatus::Stopped => panic!("expected server to be running"),
        }
    }

    #[test]
    fn default_ports_are_the_documented_values() {
        assert_eq!(ServerKind::Tcp.default_port(), 7878);
        assert_eq!(ServerKind::Http.default_port(), 3000);
    }

    #[test]
    fn start_and_stop_tcp_echoes() {
        let (runtime, mut manager) = manager_with_runtime();

        // Port 0 asks the OS for any free ephemeral port.
        manager.start(ServerKind::Tcp, Some(0)).unwrap();
        let port = running_port(manager.status(ServerKind::Tcp));
        assert!(port > 0, "the OS should have assigned a real port");

        // Connect from the test thread and verify the echo.
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(b"hello\n").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        assert_eq!(line, "echo: hello\n");

        // Stopping flips the reported status back to Stopped.
        manager.stop(ServerKind::Tcp).unwrap();
        assert!(matches!(
            manager.status(ServerKind::Tcp),
            ServerStatus::Stopped
        ));

        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn start_and_stop_http_serves_post() {
        let (runtime, mut manager) = manager_with_runtime();

        manager.start(ServerKind::Http, Some(0)).unwrap();
        let port = running_port(manager.status(ServerKind::Http));

        let body = r#"{"message":"hi"}"#;
        let request = format!(
            "POST /data HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );

        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"), "got: {response}");
        assert!(
            response.ends_with("OK"),
            "body should be OK, got: {response}"
        );

        manager.stop(ServerKind::Http).unwrap();
        assert!(matches!(
            manager.status(ServerKind::Http),
            ServerStatus::Stopped
        ));

        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn double_start_is_refused() {
        let (runtime, mut manager) = manager_with_runtime();
        manager.start(ServerKind::Tcp, Some(0)).unwrap();
        let err = manager.start(ServerKind::Tcp, Some(0)).unwrap_err();
        assert!(err.contains("already running"), "got: {err}");
        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn stop_when_not_running_is_an_error() {
        let (runtime, mut manager) = manager_with_runtime();
        let err = manager.stop(ServerKind::Http).unwrap_err();
        assert!(err.contains("not running"), "got: {err}");
        runtime.shutdown_timeout(Duration::from_secs(1));
    }
}
