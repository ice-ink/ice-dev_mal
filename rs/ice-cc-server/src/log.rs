//! Shared logging for the control center.
//!
//! The servers run as asynchronous tasks on tokio's *background* worker
//! threads, while the terminal UI runs on the *main* thread. Those two worlds
//! need a safe way to pass messages in one direction (servers -> UI) so the UI
//! can display what is happening.
//!
//! The standard tool for that in Rust is a **channel**. We use an *unbounded*
//! multi-producer, single-consumer (mpsc) channel:
//!
//! * **multi-producer** — every server holds its own clone of the sender
//!   ([`LogSink`]) and can push messages onto the channel independently.
//! * **single-consumer** — the UI holds the one receiver and drains it on every
//!   redraw.
//!
//! "Unbounded" means `send` never has to wait for the receiver to catch up; it
//! simply enqueues the message. That is the right trade-off here because we do
//! not want a busy server to block on logging, and the UI drains quickly
//! anyway.

use std::time::{Duration, Instant};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// One line of log output travelling from a server to the UI.
#[derive(Debug, Clone)]
pub struct LogLine {
    /// How long after program start this line was produced.
    ///
    /// We record *elapsed time* rather than a wall-clock date so we can show
    /// something like `+12.3s` without pulling in a date-handling crate.
    pub elapsed: Duration,
    /// The text of the line. The server is responsible for prefixing it with
    /// its source tag, e.g. `[tcp]` or `[http]`.
    pub message: String,
}

/// The "sender half" of the log channel.
///
/// This type is deliberately cheap to clone: cloning just creates another
/// handle that points at the *same* underlying channel. That is what lets us
/// hand one copy to the TCP server, one to the HTTP server, and keep another
/// for the manager itself, all writing to the same queue.
#[derive(Clone)]
pub struct LogSink {
    /// The actual channel sender.
    tx: UnboundedSender<LogLine>,
    /// Captured once when the program starts, so every line can report its age.
    start: Instant,
}

impl LogSink {
    /// Create a new sink plus the receiver that the UI will drain.
    ///
    /// Both halves are returned together because they are two ends of the same
    /// channel: keep the receiver for the UI, and clone the sink for each
    /// server.
    pub fn new() -> (Self, UnboundedReceiver<LogLine>) {
        let (tx, rx) = unbounded_channel();
        let sink = Self {
            tx,
            start: Instant::now(),
        };
        (sink, rx)
    }

    /// Push a message onto the channel for the UI to display.
    ///
    /// `send` can only fail if the receiver has already been dropped (that is,
    /// the UI is gone — for example during shutdown). We deliberately ignore
    /// that result (`let _ = ...`) so a server can never panic merely because
    /// it tried to log at the wrong moment.
    pub fn log(&self, message: impl Into<String>) {
        let line = LogLine {
            elapsed: self.start.elapsed(),
            message: message.into(),
        };
        let _ = self.tx.send(line);
    }
}

impl Default for LogSink {
    /// A default sink whose receiver is thrown away. Useful for tests that do
    /// not care about the log output.
    fn default() -> Self {
        Self::new().0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_round_trip() {
        let (sink, mut rx) = LogSink::new();
        sink.log("hello from a server");

        let line = rx.try_recv().expect("a message should be queued");
        assert_eq!(line.message, "hello from a server");
        assert!(line.elapsed < Duration::from_secs(1));
    }

    #[test]
    fn multiple_producers_share_one_queue() {
        let (sink, mut rx) = LogSink::new();
        // Cloning the sink gives independent handles to the same channel.
        let a = sink.clone();
        let b = sink;

        a.log("first");
        b.log("second");

        assert_eq!(rx.try_recv().unwrap().message, "first");
        assert_eq!(rx.try_recv().unwrap().message, "second");
        assert!(rx.try_recv().is_err(), "no more messages expected");
    }

    #[test]
    fn send_after_receiver_dropped_does_not_panic() {
        let (sink, rx) = LogSink::new();
        drop(rx); // simulate the UI shutting down
        sink.log("nobody is listening"); // must be a harmless no-op
    }
}
