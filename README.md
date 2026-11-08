# ice-cc-server

A small **terminal control center** written in Rust. It is a single program that
opens a full-screen text UI (a "TUI") from which you start, stop, and inspect
two network servers that run *inside the same process*:

| Server | Default port | What it does |
| ------ | ------------ | ------------ |
| `tcp`  | `7878`       | Echo server — sends back a copy of every line it receives. |
| `http` | `3000`       | HTTP server with a single `POST /data` endpoint that logs the JSON body and answers `OK`. |

This started as three separate pieces (an arithmetic REPL, a TCP server binary,
and an HTTP server binary). It has been merged into one binary where the
command line is a full terminal UI that controls both servers.

---

## Prerequisites

* Rust **1.88** or newer (the crate uses edition 2024 and is developed on 1.99).
  Install it with [rustup](https://rustup.rs/) if you don't have it.
* Network access on first build, so Cargo can download the dependencies.

> **Run it in a real terminal.** The UI needs an interactive terminal (raw
> mode + an alternate screen). Use Windows Terminal, cmd, PowerShell, or any
> Unix terminal. It will *not* render correctly when run through a pipe or a
> headless/CI environment.

## Build & run

```sh
cd rs/ice-cc-server
cargo run            # build and launch the control center
```

Run the test suite and lints with:

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

## Using it

When it starts you'll see a screen with four regions:

1. a **title/help bar** at the top,
2. a **servers** panel showing whether each server is `RUNNING` or `STOPPED`
   (plus its port and uptime),
3. a scrolling **log** pane (server events and command output), and
4. an **input line** with a `>` prompt at the bottom.

Type a command and press **Enter**. All commands are case-insensitive.

### Commands

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

### Key bindings

| Key | Action |
| --- | ------ |
| `Enter` | submit the current command |
| `Backspace` | delete the last character |
| `Up` / `Down` | recall previous commands (history) |
| `PageUp` / `PageDown` | scroll the log pane |
| `Esc` | clear the current input |
| `Ctrl+C` (or `Ctrl+D`) | quit |

### Trying the servers out

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

The message appears in the log pane, and the server's status flips to
`RUNNING`/`STOPPED` as you start and stop it.

## How it's organized

```
rs/ice-cc-server/
  Cargo.toml
  src/
    main.rs          entry point: builds the runtime, wires everything up
    log.rs           the shared log channel (servers -> UI)
    server/mod.rs    ServerManager: start/stop/status of the servers
    server/tcp.rs    the async TCP echo server
    server/http.rs   the async axum HTTP server
    ui.rs            the terminal UI (ratatui + crossterm) and command parser
```

The key idea is a clean split between two worlds:

* **Asynchronous servers** — `server/tcp.rs` and `server/http.rs` are `async fn`s
  that `await` their network I/O on tokio's worker threads.
* **Synchronous UI** — `ui.rs` runs on the main thread and never writes `async`.
  It starts and stops servers with plain function calls.

The bridge is a `tokio::runtime::Handle` held by the `ServerManager`:
`handle.spawn(future)` is a *synchronous* call that schedules a server onto the
runtime. Servers report events back to the UI over a channel.

Read [`doc/architecture.md`](doc/architecture.md) for a fuller walkthrough of
the threading model, graceful shutdown, and the log pipeline.

## Extending it

**Add a command** — extend the `Command` enum and `parse_command` in `ui.rs`,
then handle the new variant in `UiState::dispatch`.

**Add a server** — add a variant to `ServerKind`, give it a `default_port`, write
a `run(...)` function modeled on `server/tcp.rs`, and extend `ServerManager`
(its `start`/`stop`/`status`/`list` all switch on `ServerKind`).

---

*The inline comments in the source are written for someone who knows basic
programming but is new to Rust — they explain ownership, `async`/`await`,
channels, and the borrow checker as they come up.*
