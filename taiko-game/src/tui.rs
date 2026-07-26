use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::cursor;
use crossterm::event::{self, Event as CrosstermEvent, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

pub type Frame<'a> = ratatui::Frame<'a>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEvent {
    Tick,
    Frame,
    Key {
        event: KeyEvent,
        observed_at: Instant,
    },
    Resize(u16, u16),
}

pub struct Tui {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    tick_interval: Duration,
    frame_interval: Duration,
    next_tick: Instant,
    next_frame: Instant,
    entered: bool,
}

impl Tui {
    pub fn new(tps: u32, fps: u32) -> Result<Self> {
        let stdout = io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        let now = Instant::now();
        Ok(Self {
            terminal,
            tick_interval: duration_per_rate(tps),
            frame_interval: duration_per_rate(fps),
            next_tick: now,
            next_frame: now,
            entered: false,
        })
    }

    pub fn enter(&mut self) -> Result<()> {
        if self.entered {
            return Ok(());
        }

        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
        self.terminal.clear()?;
        self.entered = true;
        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        if !self.entered {
            return Ok(());
        }

        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen, cursor::Show)?;
        self.entered = false;
        Ok(())
    }

    pub fn draw<F>(&mut self, f: F) -> Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.terminal.draw(f)?;
        Ok(())
    }

    pub fn resize(&mut self, area: Rect) -> Result<()> {
        self.terminal.resize(area)?;
        Ok(())
    }

    pub fn next_event(&mut self) -> Result<UiEvent> {
        loop {
            // Gameplay input takes priority over scheduled work. Otherwise an input
            // already waiting in the terminal queue can sit behind one or more
            // overdue tick/frame events and acquire a systematically late timestamp.
            if event::poll(Duration::ZERO)? {
                match event::read()? {
                    CrosstermEvent::Key(event) if is_physical_key_press(event) => {
                        return Ok(UiEvent::Key {
                            event,
                            observed_at: Instant::now(),
                        });
                    }
                    CrosstermEvent::Resize(w, h) => return Ok(UiEvent::Resize(w, h)),
                    _ => {}
                }
            }

            let now = Instant::now();
            if now >= self.next_tick {
                self.next_tick = advance_deadline(self.next_tick, self.tick_interval, now);
                return Ok(UiEvent::Tick);
            }

            if now >= self.next_frame {
                self.next_frame = advance_deadline(self.next_frame, self.frame_interval, now);
                return Ok(UiEvent::Frame);
            }

            let next_deadline = self.next_tick.min(self.next_frame);
            let timeout = next_deadline.saturating_duration_since(now);

            if event::poll(timeout)? {
                match event::read()? {
                    CrosstermEvent::Key(event) if is_physical_key_press(event) => {
                        return Ok(UiEvent::Key {
                            event,
                            observed_at: Instant::now(),
                        });
                    }
                    CrosstermEvent::Resize(w, h) => return Ok(UiEvent::Resize(w, h)),
                    _ => {}
                }
            }
        }
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.exit();
    }
}

fn duration_per_rate(rate: u32) -> Duration {
    let rate = rate.max(1);
    Duration::from_secs_f64(1.0 / f64::from(rate))
}

fn is_physical_key_press(event: KeyEvent) -> bool {
    event.kind == KeyEventKind::Press
}

fn advance_deadline(deadline: Instant, interval: Duration, now: Instant) -> Instant {
    let next = deadline + interval;
    if next > now {
        next
    } else {
        // The audio/server clock is authoritative, so missed UI callbacks must
        // be dropped instead of replayed.  Re-anchoring also keeps resume after
        // a suspended terminal O(1), rather than looping once per missed frame.
        now + interval
    }
}

#[cfg(test)]
mod tests {
    use super::{advance_deadline, is_physical_key_press};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use std::time::{Duration, Instant};

    #[test]
    fn deadline_keeps_phase_when_only_one_interval_is_due() {
        let origin = Instant::now();
        let interval = Duration::from_millis(4);
        assert_eq!(
            advance_deadline(origin, interval, origin + Duration::from_millis(2)),
            origin + interval
        );
    }

    #[test]
    fn deadline_drops_a_large_suspended_backlog_in_constant_work() {
        let origin = Instant::now();
        let now = origin + Duration::from_secs(24 * 60 * 60);
        let interval = Duration::from_millis(4);
        assert_eq!(advance_deadline(origin, interval, now), now + interval);
    }

    #[test]
    fn repeated_or_released_keys_cannot_starve_scheduled_work() {
        let key = |kind| KeyEvent {
            code: KeyCode::Char('f'),
            modifiers: KeyModifiers::NONE,
            kind,
            state: KeyEventState::NONE,
        };
        assert!(is_physical_key_press(key(KeyEventKind::Press)));
        assert!(!is_physical_key_press(key(KeyEventKind::Repeat)));
        assert!(!is_physical_key_press(key(KeyEventKind::Release)));
    }
}
