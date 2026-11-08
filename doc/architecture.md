# Architecture

This document walks through how `ice-cc-server` (the control center) is put
together. It assumes you know what a thread is, what a function is, and roughly
what networking means, but not necessarily how Rust's `async`/`await` or
ownership model work. For the other half of the system — the remote
`ice-cc-client` beacon — see [`client.md`](client.md).

## The big picture

```
                ┌─────────────────────────────────────────────────────┐
                │                   one process                        │
                │                                                      │
  terminal ◄────┤  main thread ── UI (ui.rs, synchronous)              │
  (keyboard)    │     │   calls start/stop/status                      │
                │     ▼                                                │
                │  ServerManager (server/mod.rs, synchronous)          │
                │     │   handle.spawn(future)                         │
                │     │   cancel.cancel()                              │
                │     ▼                                                │
                │  tokio runtime ── worker threads                     │
                │     ├── TCP server task  (server/tcp.rs, async)      │
                │     └── HTTP server task (server/http.rs, async)     │
                │          │  log messages                             │
                │          ▼                                           │
                │  log channel ──► UI drains and draws (ui.rs)         │
                └─────────────────────────────────────────────────────┘
```

There are two kinds of work in the program, and they live on different threads:

1. **The UI** is *synchronous* and runs on the main thread. It reads keystrokes,
   draws the screen, and calls plain functions.
2. **The servers** are *asynchronous* and run on tokio's worker threads. They
   `await` network operations instead of blocking a thread while waiting.

The reason for this split is that a terminal UI needs a blocking loop on the
main thread, while network servers are easiest to write as `async fn`s. The two
are connected by a few well-understood bridges, described below.

## Threading: one runtime, two roles

`main` (in `src/main.rs`) does the wiring:

```rust
let runtime = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()?;                       // worker threads start now, in the background
let handle = runtime.handle().clone();
let mut manager = ServerManager::new(handle, log);
ui::run(&mut manager, log_rx)?;      // blocks the main thread
manager.stop_all();
runtime.shutdown_timeout(Duration::from_secs(2));
```

A tokio **runtime** is an executor: a pool of threads that poll *futures*
(Rust's name for "a computation that may not be done yet"). Building it starts
the worker threads immediately, so they run in the background while `main`
moves on to drive the UI.

`runtime.handle()` returns a `Handle`: a cheap, clone-able pointer that can
spawn work onto that runtime from any thread. This is the crucial detail that
lets the UI stay synchronous:

* `handle.spawn(future)` is a **synchronous function**. It schedules the future
  onto the runtime and returns a `JoinHandle` immediately. It does not wait, and
  it does not require the caller to be inside an `async fn`.
* So the UI can say "start the TCP server" with a plain function call, and the
  server runs `async` on the runtime's threads.

## The synchronous bind, the async serve

`ServerManager::start` wants two things that pull in opposite directions:

1. **Fail fast on errors.** If the port is already in use, the user should hear
   about it *now*, not see "started" followed by a log line a moment later.
2. **Hand a tokio-usable socket to the async task.**

It satisfies both by doing the bind in two steps:

```rust
// 1. Synchronous bind (fails fast on "address in use"):
let std_listener = std::net::TcpListener::bind(("0.0.0.0", port))?;
std_listener.set_nonblocking(true)?;

// 2. Inside the spawned task (where a reactor exists):
let listener = tokio::net::TcpListener::from_std(std_listener)?;
```

The standard-library `bind` performs `bind` **and** `listen` at the OS level, so
the socket is already accepting connections before the tokio task ever calls
`accept` — clients just queue up in the OS backlog.

The conversion in step 2 has to happen *inside* the runtime because tokio needs
a running **reactor** (its I/O event loop) to register the socket. That is why
the code does not call `from_std` on the UI thread.

## Starting and stopping a server

Each running server is described by a small struct:

```rust
struct RunningServer {
    port: u16,
    since: Instant,
    join: JoinHandle<()>,        // the spawned task
    cancel: CancellationToken,   // the shutdown signal
}
```

**Starting** creates a fresh `CancellationToken` and spawns the server future:

```rust
let cancel = CancellationToken::new();
let join = handle.spawn(async move { /* ... run the server ... */ });
```

**Stopping** is a one-liner:

```rust
running.cancel.cancel();
```

`CancellationToken` is a shared "please stop" flag (from `tokio-util`). The
server's accept loop waits on it at the same time as it waits for connections:

```rust
tokio::select! {
    _ = cancel.cancelled() => return Ok(()),     // asked to stop
    accepted = listener.accept() => { /* ... */ } // a client connected
}
```

`tokio::select!` races two futures and runs whichever completes first, so the
server reacts to either "shut down" or "a new connection" without blocking on
one or the other. The HTTP server uses the same token with axum's
`with_graceful_shutdown`, which stops accepting new requests and lets in-flight
requests finish.

Because `cancel()` only *signals*, the server winds down cleanly (it is not
killed mid-request). The manager drops its `RunningServer` record immediately,
so the UI reports `STOPPED` right away; the task itself exits a moment later.

## The log pipeline: a channel

The servers run on background threads, the UI on the main thread. They must not
touch each other's memory directly, so they communicate through a **channel**
(`src/log.rs`):

```
TCP task  ─┐
HTTP task ─┼─► LogSink (UnboundedSender<LogLine>) ──► UI (UnboundedReceiver)
manager   ─┘                                          │  try_recv() each frame
                                                       ▼
                                                    log pane
```

* `LogSink` is the **sender** half. It is cheap to `Clone` — every clone is just
  another handle to the same queue — so each server gets its own.
* The UI holds the **receiver** half and drains it once per frame with
  `try_recv()`, which never blocks: if the queue is empty it just returns an
  error that the loop ignores.
* The channel is *unbounded*, so a busy server never waits to log; the UI is
  fast enough to keep up.

This is a *multi-producer, single-consumer* channel: many writers, one reader.
The single reader is the whole point — log ordering stays simple because the UI
is the only thing consuming messages.

## The UI loop

`ui::run` repeats three steps forever (until the user quits):

```
while running {
    1. drain log_rx.try_recv() into the log buffer;
    2. terminal.draw(|frame| draw the screen);
    3. wait for one keystroke (up to 100 ms) and handle it;
}
```

* `ratatui::try_init()` puts the terminal into **raw mode** (keystrokes arrive
  one at a time instead of line-buffered) and enters the **alternate screen**
  (our UI replaces the shell rather than scrolling under it). It also installs a
  panic hook that restores the terminal if the program panics.
* A small RAII guard (`RestoreTerminal`) calls `ratatui::restore()` in its
  `Drop` impl, so the terminal is restored whether `run` returns normally *or*
  unwinds through a `?` error. `Drop` is Rust's "always run this cleanup"
  mechanism.
* The `100 ms` poll timeout means the status panel refreshes even when the user
  is idle — for example, it flips to `STOPPED` shortly after a server crashes.

## Parsing commands

`ui.rs` keeps command parsing as a **pure function** — `String` in, `Command` or
error out — with no terminal or network involved:

```rust
fn parse_command(input: &str) -> Result<Command, String> { ... }
```

`Command` is an enum (e.g. `Start(ServerKind, Option<u16>)`). Keeping this
function pure makes it trivial to unit test, and keeps "what did the user type"
separate from "what should happen as a result".

## Data flow, end to end

1. User types `tcp start` and presses Enter.
2. `ui.rs` parses it into `Command::Start(ServerKind::Tcp, None)`.
3. `dispatch` calls `manager.start(Tcp, None)`.
4. `start` binds `0.0.0.0:7878` synchronously (failing fast if it's taken), then
   `handle.spawn`s the TCP server task.
5. The task logs `[tcp] listening on ...` onto the channel and enters its accept
   loop.
6. The UI's next frame drains that message and shows it in the log pane; the
   status panel shows `tcp RUNNING`.
7. User types `tcp stop`. `manager.stop` cancels the token; the server's select
   loop notices and shuts down gracefully.
