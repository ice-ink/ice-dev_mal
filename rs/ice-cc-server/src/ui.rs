//! The terminal user interface (TUI).
//!
//! This module is the "view" of the application. It is responsible for three
//! things, repeated forever until the user quits:
//!
//! 1. **drawing** the screen (title, server status, scrollable log, input line);
//! 2. **reading** the user's keystrokes (via [crossterm](https://docs.rs/crossterm));
//! 3. **dispatching** the commands the user typed to the [`ServerManager`].
//!
//! Keeping the UI in its own module (and the server logic in `server`) means
//! each half can be read and tested in isolation. In particular, `parse_command`
//! below is a pure function — a string in, a `Command` or an error out — which
//! makes it trivial to unit test without a terminal.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::log::LogLine;
use crate::server::{ServerKind, ServerManager, ServerStatus};

/// Maximum number of lines kept in the on-screen log before the oldest drop off.
const LOG_CAPACITY: usize = 1000;
/// Maximum length of a single typed command.
const MAX_INPUT_LEN: usize = 256;
/// How long to wait (idle) for a keystroke before redrawing anyway. This keeps
/// the status panel responsive (e.g. reflects a crashed server) without busy
/// spinning the CPU.
const EVENT_POLL: Duration = Duration::from_millis(100);

/// Run the TUI until the user quits.
pub fn run(manager: &mut ServerManager, mut log_rx: UnboundedReceiver<LogLine>) -> io::Result<()> {
    // `try_init` puts the terminal into *raw mode* (keystrokes are delivered one
    // at a time instead of being line-buffered) and enters the *alternate
    // screen* (so our UI replaces the shell instead of scrolling under it). It
    // also installs a panic hook that restores the terminal if we panic.
    let mut terminal = ratatui::try_init()?;

    // RAII guard: `Drop::drop` runs both when `run` returns normally *and* when
    // it unwinds due to a `?` error, so the terminal is always restored. This is
    // a standard Rust pattern for "always run this cleanup".
    struct RestoreTerminal;
    impl Drop for RestoreTerminal {
        fn drop(&mut self) {
            ratatui::restore();
        }
    }
    let _restore = RestoreTerminal;

    // All of the UI's mutable state lives in one struct so the draw and
    // event-handling functions can share it without passing ten arguments.
    let mut ui = UiState::default();

    while ui.running {
        // 1) Pull in any log lines the servers produced since the last frame.
        //    `try_recv` never blocks: if the queue is empty it just returns Err.
        while let Ok(line) = log_rx.try_recv() {
            ui.push_log(line);
        }

        // 2) Draw the current frame.
        terminal.draw(|frame| ui.draw(frame, manager))?;

        // 3) Wait for and handle one user event.
        ui.handle_events(manager)?;
    }

    Ok(())
}

/// All of the UI state that changes as the program runs.
struct UiState {
    /// False once the user asks to quit; the main loop checks this.
    running: bool,
    /// The command the user is currently typing.
    input: String,
    /// Previously submitted commands, for Up/Down recall.
    history: Vec<String>,
    /// Current position in `history` (`history.len()` means "fresh line").
    history_pos: usize,
    /// The scrollable log pane, oldest first. Stored as rendered strings so the
    /// draw step only has to pick a window of them.
    logs: VecDeque<String>,
    /// How far the user has scrolled up; 0 means "follow the newest lines".
    scroll: usize,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            running: true,
            input: String::new(),
            history: Vec::new(),
            history_pos: 0,
            logs: VecDeque::new(),
            scroll: 0,
        }
    }
}

impl UiState {
    // -- drawing -------------------------------------------------------------

    fn draw(&mut self, frame: &mut Frame, manager: &ServerManager) {
        let area = frame.area();

        // Split the screen into four horizontal bands. `Length(n)` is a fixed
        // number of rows; `Min(n)` takes whatever space is left (at least n).
        let chunks = Layout::vertical([
            Constraint::Length(1), // title / help bar
            Constraint::Length(4), // status panel (border + two rows)
            Constraint::Min(3),    // log pane
            Constraint::Length(1), // input line
        ])
        .split(area);

        // 1) Title / help bar.
        let title = Line::from(vec![
            Span::styled(
                " ice control center ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "   type 'help' for commands · 'quit' or Ctrl+C to exit ",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(title), chunks[0]);

        // 2) Status panel: one colored line per server.
        let status_lines: Vec<Line> = manager
            .list()
            .into_iter()
            .map(|(kind, status)| status_line(kind, status))
            .collect();
        frame.render_widget(
            Paragraph::new(status_lines)
                .block(Block::default().borders(Borders::ALL).title(" servers ")),
            chunks[1],
        );

        // 3) Log pane: a scrollable window over the newest lines.
        let log_block = Block::default().borders(Borders::ALL).title(" log ");
        frame.render_widget(
            Paragraph::new(self.visible_log_lines(chunks[2].height as usize))
                .block(log_block)
                .wrap(Wrap { trim: false }),
            chunks[2],
        );

        // 4) Input line, with the terminal cursor placed just after the text.
        let input = Line::from(vec![
            Span::styled(
                "> ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(self.input.as_str()),
        ]);
        frame.render_widget(Paragraph::new(input), chunks[3]);
        let cursor_x = chunks[3].x + 2 + self.input.chars().count() as u16;
        frame.set_cursor_position((cursor_x, chunks[3].y));
    }

    /// The slice of log lines currently visible in a pane `height` rows tall.
    fn visible_log_lines(&self, height: usize) -> Vec<Line<'static>> {
        let total = self.logs.len();
        // `scroll` counts from the newest line: scroll = 0 -> show the tail.
        let end = total.saturating_sub(self.scroll);
        let start = end.saturating_sub(height);
        self.logs
            .iter()
            .skip(start)
            .take(end - start)
            // `s.clone()` makes an owned String so each `Line` owns its text
            // (the returned `Line`s must be `'static`, not borrow from `self`).
            .map(|s| Line::styled(s.clone(), log_line_style(s)))
            .collect()
    }

    // -- event handling ------------------------------------------------------

    fn handle_events(&mut self, manager: &mut ServerManager) -> io::Result<()> {
        // Wait up to EVENT_POLL for an event. If none arrives, return and let
        // the outer loop redraw (which refreshes the status panel).
        if !event::poll(EVENT_POLL)? {
            return Ok(());
        }
        match event::read()? {
            Event::Key(key) => self.handle_key(key, manager),
            // A resize needs no action; the next `draw` re-renders at the new size.
            Event::Resize(_, _) => {}
            // Mouse, focus, paste, etc. are ignored for now.
            _ => {}
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent, manager: &mut ServerManager) {
        // On Windows (and on terminals with keyboard-enhancement reporting), a
        // single physical keypress can arrive as Press/Repeat/Release events. We
        // only care about the initial press, otherwise every keystroke would be
        // processed twice.
        if key.kind != KeyEventKind::Press {
            return;
        }

        match (key.code, key.modifiers) {
            // Ctrl+C and Ctrl+D both quit.
            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => self.running = false,
            (KeyCode::Char('d'), m) if m.contains(KeyModifiers::CONTROL) => self.running = false,
            (KeyCode::Enter, _) => self.submit(manager),
            (KeyCode::Backspace, _) => {
                self.input.pop();
            }
            (KeyCode::Esc, _) => self.input.clear(),
            (KeyCode::Up, _) => self.history_back(),
            (KeyCode::Down, _) => self.history_forward(),
            (KeyCode::PageUp, _) => self.scroll = (self.scroll + 1).min(self.logs.len()),
            (KeyCode::PageDown, _) => self.scroll = self.scroll.saturating_sub(1),
            (KeyCode::Char(c), _) if self.input.chars().count() < MAX_INPUT_LEN => {
                self.input.push(c);
            }
            _ => {}
        }
    }

    /// The user pressed Enter: interpret `self.input` as a command.
    fn submit(&mut self, manager: &mut ServerManager) {
        let line = self.input.trim().to_string();

        // Echo the command so the log pane reads like a transcript.
        self.push_text(format!("> {line}"));

        if !line.is_empty() {
            self.history.push(line.clone());
            self.history_pos = self.history.len();
        }
        self.input.clear();
        self.scroll = 0; // jump back to the newest output

        if line.is_empty() {
            return;
        }

        match parse_command(&line) {
            Ok(command) => self.dispatch(command, manager),
            Err(message) => self.push_text(format!("error: {message}")),
        }
    }

    /// Carry out a parsed command, writing results into the log pane.
    fn dispatch(&mut self, command: Command, manager: &mut ServerManager) {
        match command {
            Command::Help => self.push_text(HELP_TEXT),
            Command::Quit => self.running = false,
            Command::Clear => {
                self.logs.clear();
                self.scroll = 0;
            }
            Command::List => {
                for (kind, status) in manager.list() {
                    self.push_text(format_status(kind, status));
                }
            }
            Command::Start(kind, port) => {
                if let Err(e) = manager.start(kind, port) {
                    self.push_text(format!("error: {e}"));
                }
            }
            Command::Stop(kind) => {
                if let Err(e) = manager.stop(kind) {
                    self.push_text(format!("error: {e}"));
                }
            }
            Command::Status(kind) => {
                self.push_text(format_status(kind, manager.status(kind)));
            }
        }
    }

    // -- history & scrolling -------------------------------------------------

    fn history_back(&mut self) {
        if self.history.is_empty() {
            return;
        }
        if self.history_pos > 0 {
            self.history_pos -= 1;
            self.input = self.history[self.history_pos].clone();
        }
    }

    fn history_forward(&mut self) {
        if self.history_pos < self.history.len() {
            self.history_pos += 1;
            self.input = if self.history_pos < self.history.len() {
                self.history[self.history_pos].clone()
            } else {
                String::new()
            };
        }
    }

    // -- log pane ------------------------------------------------------------

    fn push_log(&mut self, line: LogLine) {
        let text = format!("{:>7.1}s  {}", line.elapsed.as_secs_f64(), line.message);
        self.push_line(text);
    }

    fn push_text(&mut self, text: impl Into<String>) {
        self.push_line(text.into());
    }

    fn push_line(&mut self, text: String) {
        if self.logs.len() >= LOG_CAPACITY {
            self.logs.pop_front();
        }
        self.logs.push_back(text);
    }
}

/// A command the user can type, produced by [`parse_command`].
enum Command {
    /// `help` or `?`
    Help,
    /// `quit` or `exit`
    Quit,
    /// `clear`
    Clear,
    /// `servers`, `list`, or `status` (show everything)
    List,
    /// `tcp start [port]` / `http start [port]`
    Start(ServerKind, Option<u16>),
    /// `tcp stop` / `http stop`
    Stop(ServerKind),
    /// `tcp status` / `http status`
    Status(ServerKind),
}

/// Turn a raw line of input into a [`Command`].
///
/// The grammar is deliberately tiny and forgiving: whitespace is split on
/// spaces, and all keywords are case-insensitive.
fn parse_command(input: &str) -> Result<Command, String> {
    // `split_whitespace` also collapses leading/trailing and repeated spaces.
    let mut words = input.split_whitespace();
    let first = words.next().unwrap_or("").to_ascii_lowercase();

    match first.as_str() {
        "help" | "?" => {
            expect_no_more(words.next(), "help")?;
            Ok(Command::Help)
        }
        "quit" | "exit" => {
            expect_no_more(words.next(), "quit")?;
            Ok(Command::Quit)
        }
        "clear" => {
            expect_no_more(words.next(), "clear")?;
            Ok(Command::Clear)
        }
        "servers" | "list" | "status" => {
            expect_no_more(words.next(), "servers")?;
            Ok(Command::List)
        }
        "tcp" | "http" => {
            let kind = if first == "tcp" {
                ServerKind::Tcp
            } else {
                ServerKind::Http
            };
            let sub = words.next().ok_or_else(|| {
                format!("{} needs a subcommand: start, stop, or status", kind.name())
            })?;
            match sub.to_ascii_lowercase().as_str() {
                "start" => {
                    let port = match words.next() {
                        None => None,
                        Some(token) => {
                            let parsed = token
                                .parse::<u16>()
                                .map_err(|_| format!("invalid port '{token}'"))?;
                            if parsed == 0 {
                                return Err("port must be between 1 and 65535".to_string());
                            }
                            Some(parsed)
                        }
                    };
                    expect_no_more(words.next(), "start")?;
                    Ok(Command::Start(kind, port))
                }
                "stop" => {
                    expect_no_more(words.next(), "stop")?;
                    Ok(Command::Stop(kind))
                }
                "status" => {
                    expect_no_more(words.next(), "status")?;
                    Ok(Command::Status(kind))
                }
                other => Err(format!("unknown subcommand '{other}' for {}", kind.name())),
            }
        }
        other => Err(format!("unknown command '{other}' (type 'help')")),
    }
}

/// Shared check that a command took no further arguments.
fn expect_no_more(extra: Option<&str>, command: &str) -> Result<(), String> {
    match extra {
        Some(word) => Err(format!(
            "'{}' takes no arguments (unexpected '{word}')",
            command
        )),
        None => Ok(()),
    }
}

/// Format a server's status as a plain-text line for the log pane.
fn format_status(kind: ServerKind, status: ServerStatus) -> String {
    match status {
        ServerStatus::Running { port, since } => format!(
            "{:<4} RUNNING  0.0.0.0:{}  (+{}s)",
            kind.name(),
            port,
            since.elapsed().as_secs()
        ),
        ServerStatus::Stopped => format!("{:<4} STOPPED", kind.name()),
    }
}

/// A colored status line for the status panel.
fn status_line(kind: ServerKind, status: ServerStatus) -> Line<'static> {
    let (text, style) = match status {
        ServerStatus::Running { port, since } => (
            format!(
                "RUNNING  0.0.0.0:{:<5}  +{}s",
                port,
                since.elapsed().as_secs()
            ),
            Style::default().fg(Color::Green),
        ),
        ServerStatus::Stopped => ("STOPPED".to_string(), Style::default().fg(Color::Red)),
    };
    Line::from(vec![
        Span::styled(
            format!(" {:<5} ", kind.name()),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(text, style),
    ])
}

/// Pick a color for a log line based on its content.
fn log_line_style(text: &str) -> Style {
    if text.starts_with("error:") {
        Style::default().fg(Color::Red)
    } else if text.contains("[tcp]") {
        Style::default().fg(Color::Green)
    } else if text.contains("[http]") {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}

/// The text printed by the `help` command.
const HELP_TEXT: &str = "\
commands:
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

keys:
  Up / Down    command history
  PageUp/Down  scroll the log
  Esc          clear the current input";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_command_form() {
        assert!(matches!(parse_command("help"), Ok(Command::Help)));
        assert!(matches!(parse_command("?"), Ok(Command::Help)));
        assert!(matches!(parse_command("quit"), Ok(Command::Quit)));
        assert!(matches!(parse_command("EXIT"), Ok(Command::Quit)));
        assert!(matches!(parse_command("clear"), Ok(Command::Clear)));
        assert!(matches!(parse_command("servers"), Ok(Command::List)));
        assert!(matches!(parse_command("list"), Ok(Command::List)));
        assert!(matches!(parse_command("status"), Ok(Command::List)));
        assert!(matches!(
            parse_command("tcp start"),
            Ok(Command::Start(ServerKind::Tcp, None))
        ));
        assert!(matches!(
            parse_command("TCP START 9000"),
            Ok(Command::Start(ServerKind::Tcp, Some(9000)))
        ));
        assert!(matches!(
            parse_command("tcp stop"),
            Ok(Command::Stop(ServerKind::Tcp))
        ));
        assert!(matches!(
            parse_command("tcp status"),
            Ok(Command::Status(ServerKind::Tcp))
        ));
        assert!(matches!(
            parse_command("http start 8080"),
            Ok(Command::Start(ServerKind::Http, Some(8080)))
        ));
        assert!(matches!(
            parse_command("http stop"),
            Ok(Command::Stop(ServerKind::Http))
        ));
    }

    #[test]
    fn rejects_invalid_input() {
        assert!(parse_command("").is_err());
        assert!(parse_command("frobnicate").is_err());
        assert!(parse_command("tcp").is_err()); // missing subcommand
        assert!(parse_command("tcp start abc").is_err()); // bad port
        assert!(parse_command("tcp start 0").is_err()); // port 0 not allowed from UI
        assert!(parse_command("tcp start 70000").is_err()); // out of u16 range
        assert!(parse_command("tcp stop extra").is_err()); // trailing token
        assert!(parse_command("help me").is_err()); // help takes no args
    }

    #[test]
    fn whitespace_and_case_are_tolerated() {
        assert!(matches!(
            parse_command("   tcp   START   1234   "),
            Ok(Command::Start(ServerKind::Tcp, Some(1234)))
        ));
    }
}
