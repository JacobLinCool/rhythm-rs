use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::tui::UiEvent;

pub enum HeadlessCommand {
    Key(KeyEvent),
    Quit,
}

pub struct HeadlessEventSource {
    tick_interval: Duration,
    next_tick: Instant,
    command_rx: mpsc::Receiver<HeadlessCommand>,
}

impl HeadlessEventSource {
    pub fn new(tps: u32, command_rx: mpsc::Receiver<HeadlessCommand>) -> Self {
        let now = Instant::now();
        Self {
            tick_interval: Duration::from_secs_f64(1.0 / f64::from(tps.max(1))),
            next_tick: now,
            command_rx,
        }
    }

    pub fn next_event(&mut self) -> Result<UiEvent> {
        loop {
            match self.command_rx.try_recv() {
                Ok(cmd) => {
                    if let Some(event) = command_to_event(cmd) {
                        return Ok(event);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Ok(UiEvent::Key(ctrl_c()));
                }
            }

            let now = Instant::now();
            if now >= self.next_tick {
                let mut next = self.next_tick + self.tick_interval;
                while next <= now {
                    next += self.tick_interval;
                }
                self.next_tick = next;
                return Ok(UiEvent::Tick);
            }

            let timeout = self.next_tick.saturating_duration_since(now);
            match self.command_rx.recv_timeout(timeout) {
                Ok(cmd) => {
                    if let Some(event) = command_to_event(cmd) {
                        return Ok(event);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Ok(UiEvent::Key(ctrl_c()));
                }
            }
        }
    }
}

fn command_to_event(cmd: HeadlessCommand) -> Option<UiEvent> {
    match cmd {
        HeadlessCommand::Key(key) => Some(UiEvent::Key(key)),
        HeadlessCommand::Quit => Some(UiEvent::Key(ctrl_c())),
    }
}

fn ctrl_c() -> KeyEvent {
    KeyEvent::new_with_kind(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        KeyEventKind::Press,
    )
}

pub fn synthetic_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
}

pub fn synthetic_char(c: char) -> KeyEvent {
    synthetic_key(KeyCode::Char(c))
}

/// Parse a line from stdin into a HeadlessCommand.
///
/// Supported commands:
///   ready / r          — toggle ready
///   select / enter     — confirm selection (Enter key)
///   up                 — move selection up
///   down               — move selection down
///   left               — previous course
///   right              — next course
///   quit / q           — quit
///   key <char>         — press a single character key
///   esc                — press Escape
pub fn parse_stdin_command(line: &str) -> Option<HeadlessCommand> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (cmd, arg) = trimmed
        .split_once(char::is_whitespace)
        .map(|(c, a)| (c, a.trim()))
        .unwrap_or((trimmed, ""));

    match cmd.to_ascii_lowercase().as_str() {
        "quit" | "q" => Some(HeadlessCommand::Quit),
        "ready" | "r" => Some(HeadlessCommand::Key(synthetic_char('r'))),
        "select" | "enter" => Some(HeadlessCommand::Key(synthetic_key(KeyCode::Enter))),
        "up" => Some(HeadlessCommand::Key(synthetic_key(KeyCode::Up))),
        "down" => Some(HeadlessCommand::Key(synthetic_key(KeyCode::Down))),
        "left" => Some(HeadlessCommand::Key(synthetic_key(KeyCode::Left))),
        "right" => Some(HeadlessCommand::Key(synthetic_key(KeyCode::Right))),
        "esc" => Some(HeadlessCommand::Key(synthetic_key(KeyCode::Esc))),
        "key" => {
            let c = arg.chars().next()?;
            Some(HeadlessCommand::Key(synthetic_char(c)))
        }
        _ => None,
    }
}

/// Spawn a thread that reads stdin lines and sends parsed commands to the channel.
pub fn spawn_stdin_reader(tx: mpsc::Sender<HeadlessCommand>) {
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            match stdin.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    // EOF or error — signal quit
                    let _ = tx.send(HeadlessCommand::Quit);
                    break;
                }
                Ok(_) => {
                    if let Some(cmd) = parse_stdin_command(&line) {
                        if tx.send(cmd).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });
}
