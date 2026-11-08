//! Network sends to the control-center servers, using only the standard library.
//!
//! There is no `reqwest` or `tokio` here. TCP is handled by `std::net::TcpStream`
//! and the HTTP request is written by hand as a plain `POST` over that same
//! socket. Hand-rolling HTTP is educational: the request is just a few lines of
//! text with a specific format, which is exactly what `build_http_request`
//! produces.
//!
//! The two pure helpers — `json_escape`, `build_http_request`, and
//! `parse_http_response` — do no I/O, so they are unit-tested without any
//! network. The socket functions are exercised by integration tests that spin
//! up a local listener in a thread.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// How long to wait for a reply before giving up. Prevents the beacon from
/// hanging forever if a server accepts a connection but never responds.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Send one line to the TCP echo server and return its reply.
///
/// The server is *line-oriented*: it echoes once per newline-terminated line.
/// So we append a `\n` and read back a single line (`echo: <message>`).
pub fn send_tcp(host: &str, port: u16, message: &str) -> Result<String, String> {
    let addr = format!("{host}:{port}");

    // `connect` opens the socket; if the server is down this fails immediately
    // with a "connection refused" error that we wrap in context.
    let mut stream = TcpStream::connect(&addr).map_err(|e| format!("tcp connect {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|e| format!("tcp set timeout: {e}"))?;

    stream
        .write_all(format!("{message}\n").as_bytes())
        .map_err(|e| format!("tcp write {addr}: {e}"))?;

    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| format!("tcp read {addr}: {e}"))?;
    Ok(reply.trim_end().to_string())
}

/// Send a JSON message to the HTTP `POST /data` endpoint and return the status
/// line plus body (e.g. `"200 OK"`).
pub fn send_http(host: &str, port: u16, message: &str) -> Result<String, String> {
    let addr = format!("{host}:{port}");

    // Build the JSON body and the full request text (see `build_http_request`).
    let body = format!("{{\"message\":\"{}\"}}", json_escape(message));
    let request = build_http_request(host, port, &body);

    let mut stream = TcpStream::connect(&addr).map_err(|e| format!("http connect {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|e| format!("http set timeout: {e}"))?;
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("http write {addr}: {e}"))?;

    // The server answers and closes (we send `Connection: close`), so reading
    // to EOF gets us the whole response.
    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .map_err(|e| format!("http read {addr}: {e}"))?;

    let (status, body) = parse_http_response(&raw);
    Ok(format!("{status} {body}"))
}

/// Escape a string so it is safe to embed inside a JSON `"..."` value.
///
/// JSON uses `\` as an escape character, so a literal `\` or `"` in the text
/// must be escaped, as must control characters such as newlines.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Any other control character is written as a \uXXXX escape.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Build the exact HTTP/1.1 `POST /data` request text.
///
/// HTTP requests are lines separated by `\r\n`, with a blank line (`\r\n\r\n`)
/// marking the end of the headers. `Content-Length` must be the *byte* length
/// of the body so the server knows when the request ends.
pub fn build_http_request(host: &str, port: u16, body: &str) -> String {
    format!(
        "POST /data HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    )
}

/// Split an HTTP response into its status code and body.
///
/// The status line looks like `HTTP/1.1 200 OK`; the second token is the
/// numeric code. The body follows the blank line.
pub fn parse_http_response(raw: &str) -> (String, String) {
    let (head, body) = match raw.split_once("\r\n\r\n") {
        Some((headers, body)) => (headers, body),
        None => (raw, ""),
    };
    let status = head
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();
    (status, body.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn json_escape_handles_special_characters() {
        assert_eq!(json_escape("plain"), "plain");
        assert_eq!(json_escape("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(json_escape("a\\b"), "a\\\\b");
        assert_eq!(json_escape("line\nbreak"), "line\\nbreak");
        assert_eq!(json_escape("tab\there"), "tab\\there");
    }

    #[test]
    fn build_http_request_has_the_expected_shape() {
        let body = r#"{"message":"hi"}"#;
        let request = build_http_request("127.0.0.1", 3000, body);

        assert!(
            request.starts_with("POST /data HTTP/1.1\r\n"),
            "got: {request}"
        );
        assert!(
            request.contains("Host: 127.0.0.1:3000\r\n"),
            "got: {request}"
        );
        assert!(request.contains("Content-Type: application/json\r\n"));
        assert!(request.contains(&format!("Content-Length: {}\r\n", body.len())));
        assert!(request.ends_with("\r\n\r\n{\"message\":\"hi\"}"));
    }

    #[test]
    fn parse_http_response_extracts_status_and_body() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK";
        let (status, body) = parse_http_response(raw);
        assert_eq!(status, "200");
        assert_eq!(body, "OK");
    }

    /// Read a full HTTP/1.1 request off a socket, using `Content-Length` to know
    /// when the body ends. This mirrors what a real web server does.
    fn read_http_request(sock: &mut std::net::TcpStream) -> String {
        let mut data: Vec<u8> = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            let n = sock.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);

            if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                let header_end = pos + 4;
                let header_text = String::from_utf8_lossy(&data[..header_end]);
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if data.len() >= header_end + content_length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&data).to_string()
    }

    #[test]
    fn send_tcp_echoes_against_a_local_listener() {
        // Bind to port 0 so the OS picks a free port; read it back.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = std::thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            // Read the line the client sends, then reply with the echo.
            let mut line = String::new();
            let mut reader = BufReader::new(sock.try_clone().unwrap());
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "hello\n");
            let mut sock = sock;
            sock.write_all(b"echo: hello\n").unwrap();
        });

        let reply = send_tcp("127.0.0.1", port, "hello").unwrap();
        assert_eq!(reply, "echo: hello");
        server.join().unwrap();
    }

    #[test]
    fn send_http_posts_against_a_local_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let request = read_http_request(&mut sock);
            assert!(
                request.starts_with("POST /data HTTP/1.1\r\n"),
                "got: {request}"
            );
            assert!(request.contains("\"message\":\"hi\""), "got: {request}");
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                .unwrap();
            // `sock` is dropped here, closing the connection so the client's
            // read-to-EOF returns.
        });

        let reply = send_http("127.0.0.1", port, "hi").unwrap();
        assert_eq!(reply, "200 OK");
        server.join().unwrap();
    }
}
