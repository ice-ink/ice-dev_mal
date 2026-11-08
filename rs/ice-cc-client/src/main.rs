//! The client entry point: parse arguments and run the beacon loop.
//!
//! A *beacon* is a program that periodically reports back to a server. This one
//! sends a single text message every `--interval` seconds to the TCP echo
//! endpoint and/or the HTTP `POST /data` endpoint of the control-center server,
//! prints each result, and repeats until `--count` is reached or the process is
//! interrupted (Ctrl+C).
//!
//! Because the client is standard-library-only, there is no portable
//! Ctrl+C handler: pressing Ctrl+C terminates the process through the OS's
//! default signal handling. Use `--count` when you need a bounded, scriptable
//! number of sends instead of running forever.

mod config;
mod send;

use std::io::{self, Write};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use config::{Config, Transport, USAGE, parse_args};
use send::{send_http, send_tcp};

fn main() -> ExitCode {
    // `args().skip(1)` drops the program name; the rest are the user's flags.
    let args: Vec<String> = std::env::args().skip(1).collect();

    match parse_args(args) {
        // `--help` was requested: print the usage and exit 0.
        Ok(None) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Some(cfg)) => {
            print_startup(&cfg);
            run_beacon(&cfg);
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            eprint!("{USAGE}");
            // Exit code 2 is the conventional "usage error" code.
            ExitCode::from(2)
        }
    }
}

/// Print one line describing what the beacon is about to do.
fn print_startup(cfg: &Config) {
    println!(
        "beacon: sending {:?} to {} ({}) every {}s, over {}",
        cfg.message,
        cfg.host,
        endpoints(cfg),
        cfg.interval_secs,
        cfg.transport.name(),
    );
}

/// A human-readable list of the endpoint(s) currently enabled.
fn endpoints(cfg: &Config) -> String {
    match cfg.transport {
        Transport::Tcp => format!("tcp:{}", cfg.tcp_port),
        Transport::Http => format!("http:{}", cfg.http_port),
        Transport::Both => format!("tcp:{}, http:{}", cfg.tcp_port, cfg.http_port),
    }
}

/// The main loop: send, report, sleep, repeat.
fn run_beacon(cfg: &Config) {
    let mut tick: u64 = 0;

    loop {
        if cfg.count != 0 && tick >= cfg.count {
            break;
        }
        tick += 1;

        // Build one report per enabled transport. A failed send becomes an
        // `error: ...` entry rather than stopping the beacon, so a temporarily
        // unreachable server doesn't end the run.
        let mut parts: Vec<String> = Vec::new();
        if cfg.transport.uses_tcp() {
            parts.push(format!(
                "tcp -> {}",
                report(send_tcp(&cfg.host, cfg.tcp_port, &cfg.message))
            ));
        }
        if cfg.transport.uses_http() {
            parts.push(format!(
                "http -> {}",
                report(send_http(&cfg.host, cfg.http_port, &cfg.message))
            ));
        }

        println!("[{}] #{tick}  {}", timestamp_utc(), parts.join("   "));
        // Flush so the line appears immediately even when output is piped.
        let _ = io::stdout().flush();

        if cfg.count != 0 && tick >= cfg.count {
            break;
        }
        thread::sleep(Duration::from_secs(cfg.interval_secs));
    }
}

/// Format a send result: a quoted reply on success, or an `error:` note.
fn report(result: Result<String, String>) -> String {
    match result {
        Ok(reply) => format!("\"{reply}\""),
        Err(e) => format!("error: {e}"),
    }
}

/// The current time as `HH:MM:SS UTC`, computed from the system clock without a
/// date library. `SystemTime` measures time since the Unix epoch (Jan 1 1970),
/// so hours/minutes/seconds fall out of simple modulo arithmetic.
fn timestamp_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02} UTC")
}
