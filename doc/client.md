# The client (ice-cc-client)

This document walks through the remote client — the "beacon" that periodically
sends a message to the control-center servers. It assumes the same background as
[`architecture.md`](architecture.md): basic programming knowledge, but not
necessarily deep Rust.

The client's defining constraint is that it is **standard-library-only**. It
links against nothing but the Rust standard library, so the resulting binary is
small and needs no runtime installed on the target machine.

## What it does

```
run → parse arguments → loop:
        send to TCP (optional) ──► print result
        send to HTTP (optional) ─► print result
        sleep --interval seconds
        repeat until --count reached (or Ctrl+C kills the process)
```

Each send is independent and synchronous: open a socket, write the request,
read the reply, close. A failed send prints `error: ...` for that transport and
does **not** stop the loop — a temporarily unreachable server should not end a
long-running beacon.

## Three small modules

* `main.rs` — entry point and the beacon loop.
* `config.rs` — the `Config` struct and the hand-written argument parser.
* `send.rs` — the two socket functions plus the pure HTTP/JSON helpers.

The split mirrors the server's philosophy: keep pure logic (parsing, string
building) separate from I/O so it can be unit-tested without a network.

## Parsing arguments without a library

`config.rs` cannot use `clap` (that would break the zero-dependency goal), so it
walks `std::env::args()` by hand. The grammar is a single pattern repeated for
every option:

```
--name value
```

`parse_args` is generic over the input — it accepts `Vec<String>` (what
`env::args` gives) and `&[&str]` (what the tests pass) — via the bound
`S: AsRef<str>`, which just means "anything we can view as a `&str`". This is a
small example of how Rust generics can make one function serve both owned and
borrowed data.

## Hand-writing HTTP/1.1

Rather than pull in `reqwest`, the client writes the HTTP request as text and
sends it over a plain `TcpStream`. An HTTP/1.1 request is just:

```
POST /data HTTP/1.1\r\n
Host: 127.0.0.1:3000\r\n
Content-Type: application/json\r\n
Content-Length: 16\r\n
Connection: close\r\n
\r\n
{"message":"hi"}
```

`build_http_request` produces exactly that. Three details matter:

1. Lines end with `\r\n`, and a **blank line** (`\r\n\r\n`) separates headers
   from the body.
2. `Content-Length` is the **byte** length of the body (not the character
   count), so the server knows where the body ends.
3. `Connection: close` tells the server to close after replying, so the client
   can read the whole response with `read_to_string` (it returns at EOF).

The response is parsed by `parse_http_response`, which splits headers from body
at the blank line and extracts the numeric status code from the status line.

## JSON escaping

The HTTP body is `{"message":"..."}`. If the message itself contains a `"` or a
`\`, it would break the JSON. `json_escape` fixes that by turning special
characters into their JSON escape sequences (`"` → `\"`, `\` → `\\`, newline →
`\n`, and so on). This is the same idea as escaping quotes inside a string in
almost any language.

## TCP is just a socket

`send_tcp` connects a `TcpStream`, writes the message followed by `\n` (the
server is line-oriented), and reads one line back. A read timeout (`5s`) stops
the beacon from hanging forever if a server accepts a connection but never
replies.

## Stopping the beacon

Because the client is std-only, it has no portable signal handler. Pressing
**Ctrl+C** terminates the process through the OS's default handling (SIGINT on
Unix, a console control event on Windows). For a bounded, scriptable run, use
`--count N` instead.

## Error handling philosophy

Every socket function returns `Result<_, String>` and wraps the underlying OS
error with human-readable context (for example `tcp connect 127.0.0.1:7878:
connection refused`). The beacon loop converts any `Err` into an `error: ...`
line and keeps going, so one bad tick never kills the run.
