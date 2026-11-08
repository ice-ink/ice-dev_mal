# ice-cc — a small client/server pair in Rust

Two programs that talk to each other over the network:

* **`ice-cc-server`** — a terminal control center. A single binary that opens a
  full-screen text UI (a "TUI") from which you start, stop, and inspect two
  servers that run *inside the same process*.
* **`ice-cc-client`** — a remote **beacon** that periodically sends a message to
  those servers. It is dependency-free (standard library only).

The servers the control center runs, and that the client reports to:

| Server | Default port | What it does |
| ------ | ------------ | ------------ |
| `tcp`  | `7878`       | Echo server — sends back a copy of every line it receives. |
| `http` | `3000`       | HTTP server with a single `POST /data` endpoint that logs the JSON body and answers `OK`. |

The two programs live side by side in one [Cargo workspace](rs/Cargo.toml) under
`rs/`, so they share a repo and a lockfile but build independently — important
because the client is meant to run on a *remote* machine and should stay tiny.

---

## Prerequisites

* Rust **1.88** or newer (the crates use edition 2024 and are developed on 1.99).
  Install it with [rustup](https://rustup.rs/) if you don't have it.
* Network access on first build, so Cargo can download the dependencies.

> **Run the server in a real terminal.** Its UI needs an interactive terminal
> (raw mode + an alternate screen). Use Windows Terminal, cmd, PowerShell, or any
> Unix terminal. It will *not* render correctly through a pipe or a
> headless/CI environment. (The client has no such requirement.)

## Build & run

All commands below are run from the workspace root, `rs/`.

```sh
cd rs

cargo build                      # build both programs
cargo run -p ice-cc-server       # run the control center
cargo run -p ice-cc-client -- --count 3   # run the client (3 sends)
```

Test and lint everything:

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

---

## The server (control center)

When it starts you'll see a screen with four regions:

1. a **title/help bar** at the top,
2. a **servers** panel showing whether each server is `RUNNING` or `STOPPED`
   (plus its port and uptime),
3. a scrolling **log** pane (server events and command output), and
4. an **input line** with a `>` prompt at the bottom.

Type a command and press **Enter**. All commands are case-insensitive.

### Server commands

```
tcp start [port]   start the TCP echo server   (default 7878)
tcp stop           stop  the TCP echo server
tcp status         show  the TCP server status
http start [port]  start the HTTP server       (default 3000)
http stop          stop  the HTTP server
http status        show  the HTTP server status
servers | list     show  the status of both servers
clear              clear the log pane
help               show this message
quit | exit        stop all servers and exit   (Ctrl+C works too)
```

### Server key bindings

| Key | Action |
| --- | ------ |
| `Enter` | submit the current command |
| `Backspace` | delete the last character |
| `Up` / `Down` | recall previous commands (history) |
| `PageUp` / `PageDown` | scroll the log pane |
| `Esc` | clear the current input |
| `Ctrl+C` (or `Ctrl+D`) | quit |

### Trying the server out by hand

From a second terminal, after `tcp start`:

```sh
# Windows PowerShell
$c = New-Object System.Net.Sockets.TcpClient("127.0.0.1", 7878)
$s = $c.GetStream(); $w = New-Object System.IO.StreamWriter($s)
$w.WriteLine("hello"); $w.Flush()
(New-Object System.IO.StreamReader($s)).ReadLine()   # -> echo: hello
```

After `http start`:

```sh
# Windows PowerShell
Invoke-RestMethod -Method Post -Uri http://localhost:3000/data `
  -ContentType 'application/json' -Body '{"message":"hi"}'   # -> OK

# or with curl
curl -X POST http://localhost:3000/data -H "Content-Type: application/json" -d '{"message":"hi"}'
```

---

## The client (beacon)

The client sends a message to the enabled endpoint(s) every `--interval`
seconds, prints each result, and repeats until `--count` is reached or it is
interrupted (Ctrl+C). Start the server, run `tcp start` and `http start`, then
from another terminal:

```sh
cd rs
cargo run -p ice-cc-client -- --count 3 --interval 2 --message "hello from a remote host"
```

Output looks like:

```
beacon: sending "hello from a remote host" to 127.0.0.1 (tcp:7878, http:3000) every 2s, over both
[16:11:38 UTC] #1  tcp -> "echo: hello from a remote host"   http -> "200 OK"
[16:11:40 UTC] #2  tcp -> "echo: hello from a remote host"   http -> "200 OK"
[16:11:42 UTC] #3  tcp -> "echo: hello from a remote host"   http -> "200 OK"
```

For a remote server, point `--host` at its address (the server already binds
`0.0.0.0`).

### Client options

| Option | Meaning | Default |
| ------ | ------- | ------- |
| `--host <ADDR>` | server host | `127.0.0.1` |
| `--tcp-port <PORT>` | TCP echo port | `7878` |
| `--http-port <PORT>` | HTTP port | `3000` |
| `--interval <SECS>` | seconds between sends | `5` |
| `--message <TEXT>` | text to send | `"hello"` |
| `--transport <MODE>` | `tcp`, `http`, or `both` | `both` |
| `--count <N>` | send N times then exit; `0` = forever | `0` |
| `-h`, `--help` | show help and exit | — |

---

## How it's organized

```
rs/
  Cargo.toml            the Cargo workspace (two member crates)
  ice-cc-server/        the TUI control center
    src/
      main.rs           entry point: builds the runtime, wires everything up
      log.rs            the shared log channel (servers -> UI)
      server/mod.rs     ServerManager: start/stop/status of the servers
      server/tcp.rs     the async TCP echo server
      server/http.rs    the async axum HTTP server
      ui.rs             the terminal UI (ratatui + crossterm) and command parser
  ice-cc-client/        the remote beacon (std-only)
    src/
      main.rs           entry point: parse args, run the beacon loop
      config.rs         Config + Transport + argument parsing
      send.rs           send_tcp / send_http + the hand-written HTTP request
```

Two different worlds, deliberately kept apart:

* The **server** is `async` (tokio + axum) behind a *synchronous* TUI front end.
  The bridge is a `tokio::runtime::Handle` held by the `ServerManager`:
  `handle.spawn(future)` is a synchronous call that schedules a server onto the
  runtime.
* The **client** is fully synchronous and dependency-free: plain `TcpStream`
  sockets and a hand-written HTTP/1.1 `POST`.

Docs:

* [`doc/architecture.md`](doc/architecture.md) — the server's threading model,
  graceful shutdown, and log pipeline.
* [`doc/client.md`](doc/client.md) — the client's beacon loop, hand-written HTTP
  framing, and JSON escaping.

## Extending it

**Server — add a command:** extend the `Command` enum and `parse_command` in
`ui.rs`, then handle the new variant in `UiState::dispatch`.

**Server — add a server:** add a variant to `ServerKind`, give it a
`default_port`, write a `run(...)` modeled on `server/tcp.rs`, and extend
`ServerManager`.

**Client — add an option:** add a field to `Config`, parse it in `config.rs`,
and use it in `main.rs`.

---

*The inline comments in the source are written for someone who knows basic
programming but is new to Rust — they explain ownership, `async`/`await`,
channels, and the borrow checker as they come up.*
