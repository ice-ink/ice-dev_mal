//! Command-line configuration for the client.
//!
//! The client is deliberately dependency-free, so it cannot use a crate such as
//! `clap` to parse its arguments. Instead it walks the command-line arguments
//! by hand. The grammar is intentionally tiny: every option is written as
//! `--name value`, and anything else is an error.

/// Which server(s) the client should talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// The TCP echo endpoint only.
    Tcp,
    /// The HTTP `POST /data` endpoint only.
    Http,
    /// Both endpoints (the default).
    Both,
}

impl Transport {
    /// Whether the TCP endpoint should be contacted this run.
    pub fn uses_tcp(self) -> bool {
        matches!(self, Transport::Tcp | Transport::Both)
    }

    /// Whether the HTTP endpoint should be contacted this run.
    pub fn uses_http(self) -> bool {
        matches!(self, Transport::Http | Transport::Both)
    }

    /// A short name for log/usage output.
    pub fn name(self) -> &'static str {
        match self {
            Transport::Tcp => "tcp",
            Transport::Http => "http",
            Transport::Both => "both",
        }
    }
}

/// Everything the client needs to know to run, filled from the command line
/// (with sensible defaults when an option is omitted).
#[derive(Debug, Clone)]
pub struct Config {
    /// Server host address.
    pub host: String,
    /// TCP echo port.
    pub tcp_port: u16,
    /// HTTP port.
    pub http_port: u16,
    /// Seconds to sleep between sends.
    pub interval_secs: u64,
    /// The text message to send.
    pub message: String,
    /// Which endpoint(s) to contact.
    pub transport: Transport,
    /// How many sends to perform; `0` means "run forever".
    pub count: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            tcp_port: 7878,
            http_port: 3000,
            interval_secs: 5,
            message: "hello".to_string(),
            transport: Transport::Both,
            count: 0,
        }
    }
}

/// Parse command-line arguments into a [`Config`].
///
/// Returns `Ok(None)` when `--help` (or `-h`) was requested, so the caller can
/// print the usage and exit successfully. Any other problem is an `Err` with a
/// readable message.
///
/// The function is generic over the input so it accepts both owned strings
/// (`Vec<String>`, from `std::env::args`) and borrowed ones (`&[&str]`, which
/// the unit tests use). `S: AsRef<str>` means "anything we can view as `&str`".
pub fn parse_args<I, S>(args: I) -> Result<Option<Config>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut cfg = Config::default();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_ref() {
            "-h" | "--help" => return Ok(None),
            "--host" => cfg.host = next_value(&mut iter, "--host")?,
            "--tcp-port" => {
                cfg.tcp_port = parse_port(&next_value(&mut iter, "--tcp-port")?, "--tcp-port")?
            }
            "--http-port" => {
                cfg.http_port = parse_port(&next_value(&mut iter, "--http-port")?, "--http-port")?
            }
            "--interval" => {
                cfg.interval_secs =
                    parse_positive(&next_value(&mut iter, "--interval")?, "--interval")?
            }
            "--message" => cfg.message = next_value(&mut iter, "--message")?,
            "--transport" => {
                cfg.transport = parse_transport(&next_value(&mut iter, "--transport")?)?
            }
            "--count" => cfg.count = parse_u64(&next_value(&mut iter, "--count")?, "--count")?,
            other => return Err(format!("unknown option '{other}' (try --help)")),
        }
    }

    Ok(Some(cfg))
}

/// Pull the value that must follow the flag just consumed.
fn next_value<I, S>(iter: &mut I, flag: &str) -> Result<String, String>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    iter.next()
        .map(|s| s.as_ref().to_string())
        .ok_or_else(|| format!("option '{flag}' needs a value"))
}

/// Parse a TCP/HTTP port: a number from 1 to 65535.
fn parse_port(value: &str, name: &str) -> Result<u16, String> {
    let port: u16 = value
        .parse()
        .map_err(|_| format!("invalid {name} '{value}'"))?;
    if port == 0 {
        return Err(format!("{name} must be between 1 and 65535"));
    }
    Ok(port)
}

/// Parse a strictly-positive integer (used for `--interval`).
fn parse_positive(value: &str, name: &str) -> Result<u64, String> {
    let n: u64 = value
        .parse()
        .map_err(|_| format!("invalid {name} '{value}'"))?;
    if n == 0 {
        return Err(format!("{name} must be at least 1"));
    }
    Ok(n)
}

/// Parse a non-negative integer (used for `--count`, where 0 means "forever").
fn parse_u64(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {name} '{value}'"))
}

/// Parse the `--transport` value.
fn parse_transport(value: &str) -> Result<Transport, String> {
    match value.to_ascii_lowercase().as_str() {
        "tcp" => Ok(Transport::Tcp),
        "http" => Ok(Transport::Http),
        "both" => Ok(Transport::Both),
        other => Err(format!(
            "invalid transport '{other}' (expected tcp, http, or both)"
        )),
    }
}

/// The help text printed by `--help` and after a parse error.
pub const USAGE: &str = "\
ice-cc-client — periodically send a message to the control-center servers

USAGE:
    ice-cc-client [OPTIONS]

OPTIONS:
    --host <ADDR>         server host                 [default: 127.0.0.1]
    --tcp-port <PORT>     TCP echo port               [default: 7878]
    --http-port <PORT>    HTTP port                   [default: 3000]
    --interval <SECS>     seconds between sends       [default: 5]
    --message <TEXT>      text to send                [default: \"hello\"]
    --transport <MODE>    tcp | http | both           [default: both]
    --count <N>           send N times then exit; 0 = run forever  [default: 0]
    -h, --help            show this help and exit
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_no_args() {
        let cfg = parse_args(std::iter::empty::<&str>()).unwrap().unwrap();
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.tcp_port, 7878);
        assert_eq!(cfg.http_port, 3000);
        assert_eq!(cfg.interval_secs, 5);
        assert_eq!(cfg.message, "hello");
        assert_eq!(cfg.transport, Transport::Both);
        assert_eq!(cfg.count, 0);
    }

    #[test]
    fn every_option_can_be_overridden() {
        let cfg = parse_args([
            "--host",
            "10.0.0.5",
            "--tcp-port",
            "9000",
            "--http-port",
            "9001",
            "--interval",
            "10",
            "--message",
            "ping",
            "--transport",
            "http",
            "--count",
            "3",
        ])
        .unwrap()
        .unwrap();

        assert_eq!(cfg.host, "10.0.0.5");
        assert_eq!(cfg.tcp_port, 9000);
        assert_eq!(cfg.http_port, 9001);
        assert_eq!(cfg.interval_secs, 10);
        assert_eq!(cfg.message, "ping");
        assert_eq!(cfg.transport, Transport::Http);
        assert_eq!(cfg.count, 3);
    }

    #[test]
    fn help_returns_none() {
        assert!(parse_args(["--help"]).unwrap().is_none());
        assert!(parse_args(["-h"]).unwrap().is_none());
    }

    #[test]
    fn transport_values_are_case_insensitive() {
        assert_eq!(
            parse_args(["--transport", "TCP"])
                .unwrap()
                .unwrap()
                .transport,
            Transport::Tcp
        );
        assert_eq!(
            parse_args(["--transport", "Both"])
                .unwrap()
                .unwrap()
                .transport,
            Transport::Both
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_args(["--frobnicate"]).is_err());
        assert!(parse_args(["--host"]).is_err()); // missing value
        assert!(parse_args(["--tcp-port", "abc"]).is_err());
        assert!(parse_args(["--tcp-port", "70000"]).is_err()); // > u16::MAX
        assert!(parse_args(["--tcp-port", "0"]).is_err());
        assert!(parse_args(["--interval", "0"]).is_err());
        assert!(parse_args(["--interval", "abc"]).is_err());
        assert!(parse_args(["--count", "abc"]).is_err());
        assert!(parse_args(["--transport", "ftp"]).is_err());
    }
}
