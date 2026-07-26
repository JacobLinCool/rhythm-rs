use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use crossterm::cursor;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, KeyEvent, KeyEventKind,
    KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

pub type Frame<'a> = ratatui::Frame<'a>;

const MAX_INGRESS_EVENTS_PER_BATCH: usize = 64;
const MAX_SCHEDULER_RATE_HZ: u32 = crate::cli::MAX_TPS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEvent {
    Tick,
    Frame,
    Key {
        event: KeyEvent,
        observed_at: Instant,
    },
    Pointer {
        event: MouseEvent,
        observed_at: Instant,
    },
    Resize(u16, u16),
}

pub struct Tui {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    event_scheduler: EventScheduler,
    modes: TerminalModes,
    screen_ready: bool,
}

#[derive(Debug)]
struct EventScheduler {
    tick_interval: Duration,
    frame_interval: Duration,
    next_tick: Instant,
    next_frame: Instant,
    ready_events: VecDeque<UiEvent>,
    check_deadlines_after_batch: bool,
}

impl EventScheduler {
    fn new(tps: u32, fps: u32) -> Result<Self> {
        let now = Instant::now();
        Ok(Self {
            tick_interval: duration_per_rate(tps)
                .map_err(|error| anyhow!("invalid tick rate {tps}: {error}"))?,
            frame_interval: duration_per_rate(fps)
                .map_err(|error| anyhow!("invalid frame rate {fps}: {error}"))?,
            next_tick: now,
            next_frame: now,
            ready_events: VecDeque::with_capacity(MAX_INGRESS_EVENTS_PER_BATCH),
            check_deadlines_after_batch: false,
        })
    }

    fn next_event(&mut self, source: &mut impl EventSource) -> Result<UiEvent> {
        loop {
            if let Some(event) = self.take_ready_event() {
                return Ok(event);
            }

            if self.check_deadlines_after_batch {
                self.check_deadlines_after_batch = false;
                if let Some(event) = self.take_due_scheduled_event(Instant::now()) {
                    return Ok(event);
                }
            }

            // Stamp one bounded batch before running scheduled work. This preserves
            // the acquisition time of simultaneous P1/P2 hits even when dispatching
            // one hit makes a tick or frame overdue. Every semantic event already
            // acquired in that batch is delivered before the callback, while the
            // batch bound prevents input floods from starving scheduled work.
            self.ingest_ready_events(source, false)?;
            if let Some(event) = self.take_ready_event() {
                return Ok(event);
            }

            if self.check_deadlines_after_batch {
                continue;
            }

            let now = Instant::now();
            if let Some(event) = self.take_due_scheduled_event(now) {
                return Ok(event);
            }
            let next_deadline = self.next_tick.min(self.next_frame);
            let timeout = next_deadline.saturating_duration_since(now);

            if source.poll(timeout)? {
                self.ingest_ready_events(source, true)?;
            }
        }
    }

    fn take_ready_event(&mut self) -> Option<UiEvent> {
        self.ready_events.pop_front()
    }

    fn ingest_ready_events(
        &mut self,
        source: &mut impl EventSource,
        first_event_is_ready: bool,
    ) -> Result<()> {
        debug_assert!(self.ready_events.is_empty());

        let mut raw_events_read = 0;
        if first_event_is_ready {
            self.ingest_one_event(source)?;
            raw_events_read = 1;
        }
        while raw_events_read < MAX_INGRESS_EVENTS_PER_BATCH && source.poll(Duration::ZERO)? {
            self.ingest_one_event(source)?;
            raw_events_read += 1;
        }
        self.check_deadlines_after_batch = raw_events_read > 0;
        Ok(())
    }

    fn ingest_one_event(&mut self, source: &mut impl EventSource) -> Result<()> {
        let event = source.read()?;
        let observed_at = Instant::now();
        if let Some(event) = classify_event(event, observed_at) {
            // At most one semantic event can be emitted per raw event, and each
            // batch reads no more than this capacity.
            debug_assert!(self.ready_events.len() < MAX_INGRESS_EVENTS_PER_BATCH);
            self.ready_events.push_back(event);
        }
        Ok(())
    }

    fn take_due_scheduled_event(&mut self, now: Instant) -> Option<UiEvent> {
        let tick_due = now >= self.next_tick;
        let frame_due = now >= self.next_frame;
        match (tick_due, frame_due) {
            (false, false) => None,
            (true, false) => {
                self.next_tick = advance_deadline(self.next_tick, self.tick_interval, now);
                Some(UiEvent::Tick)
            }
            (false, true) => {
                self.next_frame = advance_deadline(self.next_frame, self.frame_interval, now);
                Some(UiEvent::Frame)
            }
            (true, true) if self.next_tick <= self.next_frame => {
                self.next_tick = advance_deadline(self.next_tick, self.tick_interval, now);
                Some(UiEvent::Tick)
            }
            (true, true) => {
                self.next_frame = advance_deadline(self.next_frame, self.frame_interval, now);
                Some(UiEvent::Frame)
            }
        }
    }
}

trait EventSource {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool>;
    fn read(&mut self) -> io::Result<CrosstermEvent>;
}

struct CrosstermEventSource;

impl EventSource for CrosstermEventSource {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        event::poll(timeout)
    }

    fn read(&mut self) -> io::Result<CrosstermEvent> {
        event::read()
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TerminalModes {
    raw_mode: bool,
    keyboard_enhancement: bool,
    alternate_screen: bool,
    cursor_hidden: bool,
    mouse_capture: bool,
}

impl TerminalModes {
    fn any_active(self) -> bool {
        self.raw_mode
            || self.keyboard_enhancement
            || self.alternate_screen
            || self.cursor_hidden
            || self.mouse_capture
    }

    fn is_entered(self) -> bool {
        self.raw_mode && self.alternate_screen && self.cursor_hidden
    }

    fn enter(&mut self, commands: &mut impl TerminalModeCommands) -> Result<()> {
        if self.is_entered() {
            return Ok(());
        }

        let enter_result: Result<()> = (|| {
            if !self.raw_mode {
                self.raw_mode = true;
                commands.enable_raw_mode()?;
            }
            if !self.keyboard_enhancement && commands.supports_reliable_keyboard_event_types()? {
                self.keyboard_enhancement = true;
                commands.push_keyboard_enhancement()?;
            }
            if !self.alternate_screen {
                self.alternate_screen = true;
                commands.enter_alternate_screen()?;
            }
            if !self.cursor_hidden {
                self.cursor_hidden = true;
                commands.hide_cursor()?;
            }
            Ok(())
        })();

        match enter_result {
            Ok(()) => Ok(()),
            Err(error) => merge_results(Err(error), self.exit(commands), "terminal rollback"),
        }
    }

    fn set_mouse_capture(
        &mut self,
        commands: &mut impl TerminalModeCommands,
        enabled: bool,
    ) -> Result<()> {
        if enabled == self.mouse_capture {
            return Ok(());
        }
        if enabled && !self.is_entered() {
            bail!("mouse capture requires an entered terminal");
        }

        if enabled {
            self.mouse_capture = true;
            if let Err(enable_error) = commands.enable_mouse_capture() {
                let rollback_result = match commands.disable_mouse_capture() {
                    Ok(()) => {
                        self.mouse_capture = false;
                        Ok(())
                    }
                    Err(disable_error) => Err(disable_error.into()),
                };
                return merge_results(
                    Err(enable_error.into()),
                    rollback_result,
                    "mouse capture rollback",
                );
            }
        } else {
            commands.disable_mouse_capture()?;
            self.mouse_capture = false;
        }
        Ok(())
    }

    fn exit(&mut self, commands: &mut impl TerminalModeCommands) -> Result<()> {
        let mut error = None;

        if self.mouse_capture {
            match commands.disable_mouse_capture() {
                Ok(()) => self.mouse_capture = false,
                Err(command_error) => {
                    append_error(&mut error, "disable mouse capture", command_error)
                }
            }
        }
        if self.cursor_hidden {
            match commands.show_cursor() {
                Ok(()) => self.cursor_hidden = false,
                Err(command_error) => append_error(&mut error, "show cursor", command_error),
            }
        }
        if self.alternate_screen {
            match commands.leave_alternate_screen() {
                Ok(()) => self.alternate_screen = false,
                Err(command_error) => {
                    append_error(&mut error, "leave alternate screen", command_error)
                }
            }
        }
        if self.keyboard_enhancement {
            match commands.pop_keyboard_enhancement() {
                Ok(()) => self.keyboard_enhancement = false,
                Err(command_error) => {
                    append_error(&mut error, "pop keyboard enhancement", command_error)
                }
            }
        }
        if self.raw_mode {
            match commands.disable_raw_mode() {
                Ok(()) => self.raw_mode = false,
                Err(command_error) => append_error(&mut error, "disable raw mode", command_error),
            }
        }

        error.map_or(Ok(()), Err)
    }
}

trait TerminalModeCommands {
    fn enable_raw_mode(&mut self) -> io::Result<()>;
    fn disable_raw_mode(&mut self) -> io::Result<()>;
    fn supports_reliable_keyboard_event_types(&mut self) -> io::Result<bool>;
    fn push_keyboard_enhancement(&mut self) -> io::Result<()>;
    fn pop_keyboard_enhancement(&mut self) -> io::Result<()>;
    fn enter_alternate_screen(&mut self) -> io::Result<()>;
    fn leave_alternate_screen(&mut self) -> io::Result<()>;
    fn hide_cursor(&mut self) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
    fn enable_mouse_capture(&mut self) -> io::Result<()>;
    fn disable_mouse_capture(&mut self) -> io::Result<()>;
}

struct CrosstermModeCommands;

impl TerminalModeCommands for CrosstermModeCommands {
    fn enable_raw_mode(&mut self) -> io::Result<()> {
        enable_raw_mode()
    }

    fn disable_raw_mode(&mut self) -> io::Result<()> {
        disable_raw_mode()
    }

    fn supports_reliable_keyboard_event_types(&mut self) -> io::Result<bool> {
        supports_keyboard_enhancement()
    }

    fn push_keyboard_enhancement(&mut self) -> io::Result<()> {
        execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            )
        )
    }

    fn pop_keyboard_enhancement(&mut self) -> io::Result<()> {
        execute!(io::stdout(), PopKeyboardEnhancementFlags)
    }

    fn enter_alternate_screen(&mut self) -> io::Result<()> {
        execute!(io::stdout(), EnterAlternateScreen)
    }

    fn leave_alternate_screen(&mut self) -> io::Result<()> {
        execute!(io::stdout(), LeaveAlternateScreen)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        execute!(io::stdout(), cursor::Hide)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        execute!(io::stdout(), cursor::Show)
    }

    fn enable_mouse_capture(&mut self) -> io::Result<()> {
        execute!(io::stdout(), EnableMouseCapture)
    }

    fn disable_mouse_capture(&mut self) -> io::Result<()> {
        execute!(io::stdout(), DisableMouseCapture)
    }
}

impl Tui {
    pub fn new(tps: u32, fps: u32) -> Result<Self> {
        let stdout = io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        Ok(Self {
            terminal,
            event_scheduler: EventScheduler::new(tps, fps)?,
            modes: TerminalModes::default(),
            screen_ready: false,
        })
    }

    pub fn enter(&mut self) -> Result<()> {
        if self.screen_ready {
            return Ok(());
        }

        let mut commands = CrosstermModeCommands;
        if self.modes.any_active() {
            self.modes.exit(&mut commands)?;
        }
        self.modes.enter(&mut commands)?;
        match self.terminal.clear() {
            Ok(()) => {
                self.screen_ready = true;
                Ok(())
            }
            Err(error) => merge_results(
                Err(error.into()),
                self.modes.exit(&mut commands),
                "terminal rollback",
            ),
        }
    }

    pub fn exit(&mut self) -> Result<()> {
        self.screen_ready = false;
        self.modes.exit(&mut CrosstermModeCommands)
    }

    /// Requests pointer reporting exactly once per successful state transition.
    ///
    /// Disabling remains available during partial cleanup; enabling requires a
    /// successfully entered and cleared alternate screen.
    pub fn set_mouse_capture(&mut self, enabled: bool) -> Result<()> {
        if enabled && !self.screen_ready {
            bail!("mouse capture requires a ready terminal");
        }
        self.modes
            .set_mouse_capture(&mut CrosstermModeCommands, enabled)
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
        self.event_scheduler.next_event(&mut CrosstermEventSource)
    }

    /// Whether this terminal reports physical press, auto-repeat, and release as
    /// distinct keyboard event kinds.
    ///
    /// Unsupported terminals remain fully usable with pointer and LAN
    /// controllers, but keyboard auto-repeat cannot be identified reliably.
    pub fn keyboard_repeat_is_distinguishable(&self) -> bool {
        self.screen_ready && self.modes.keyboard_enhancement
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.exit();
    }
}

fn duration_per_rate(rate: u32) -> Result<Duration> {
    if rate == 0 {
        bail!("rate must be greater than zero");
    }
    if rate > MAX_SCHEDULER_RATE_HZ {
        bail!("rate exceeds the practical scheduler limit of {MAX_SCHEDULER_RATE_HZ} Hz");
    }
    let interval = Duration::from_secs_f64(1.0 / f64::from(rate));
    if interval.is_zero() {
        bail!("rate rounds to a zero-duration scheduler interval");
    }
    Ok(interval)
}

fn is_physical_key_press(event: KeyEvent) -> bool {
    event.kind == KeyEventKind::Press
}

fn is_primary_pointer_down(event: MouseEvent) -> bool {
    event.kind == MouseEventKind::Down(MouseButton::Left)
}

fn classify_event(event: CrosstermEvent, observed_at: Instant) -> Option<UiEvent> {
    match event {
        CrosstermEvent::Key(event) if is_physical_key_press(event) => {
            Some(UiEvent::Key { event, observed_at })
        }
        CrosstermEvent::Mouse(event) if is_primary_pointer_down(event) => {
            Some(UiEvent::Pointer { event, observed_at })
        }
        CrosstermEvent::Resize(width, height) => Some(UiEvent::Resize(width, height)),
        _ => None,
    }
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

fn append_error(combined: &mut Option<anyhow::Error>, label: &str, error: io::Error) {
    *combined = Some(match combined.take() {
        Some(previous) => anyhow!("{previous}; {label} also failed: {error}"),
        None => anyhow!("{label} failed: {error}"),
    });
}

fn merge_results(first: Result<()>, second: Result<()>, second_label: &str) -> Result<()> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(anyhow!("{first}; {second_label} also failed: {second}")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashSet, VecDeque};
    use std::io;
    use std::time::{Duration, Instant};

    use crossterm::event::{
        Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    };

    use super::{
        advance_deadline, classify_event, duration_per_rate, is_physical_key_press, EventScheduler,
        EventSource, TerminalModeCommands, TerminalModes, UiEvent, MAX_INGRESS_EVENTS_PER_BATCH,
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum ModeCommand {
        EnableRaw,
        DisableRaw,
        QueryKeyboardEnhancement,
        PushKeyboardEnhancement,
        PopKeyboardEnhancement,
        EnterAlternate,
        LeaveAlternate,
        HideCursor,
        ShowCursor,
        EnableMouse,
        DisableMouse,
    }

    #[derive(Debug, Default)]
    struct FakeModeCommands {
        calls: Vec<ModeCommand>,
        failures: HashSet<ModeCommand>,
        keyboard_enhancement_unsupported: bool,
    }

    impl FakeModeCommands {
        fn call(&mut self, command: ModeCommand) -> io::Result<()> {
            self.calls.push(command);
            if self.failures.contains(&command) {
                Err(io::Error::other(format!("{command:?} rejected")))
            } else {
                Ok(())
            }
        }
    }

    impl TerminalModeCommands for FakeModeCommands {
        fn enable_raw_mode(&mut self) -> io::Result<()> {
            self.call(ModeCommand::EnableRaw)
        }

        fn disable_raw_mode(&mut self) -> io::Result<()> {
            self.call(ModeCommand::DisableRaw)
        }

        fn supports_reliable_keyboard_event_types(&mut self) -> io::Result<bool> {
            self.call(ModeCommand::QueryKeyboardEnhancement)?;
            Ok(!self.keyboard_enhancement_unsupported)
        }

        fn push_keyboard_enhancement(&mut self) -> io::Result<()> {
            self.call(ModeCommand::PushKeyboardEnhancement)
        }

        fn pop_keyboard_enhancement(&mut self) -> io::Result<()> {
            self.call(ModeCommand::PopKeyboardEnhancement)
        }

        fn enter_alternate_screen(&mut self) -> io::Result<()> {
            self.call(ModeCommand::EnterAlternate)
        }

        fn leave_alternate_screen(&mut self) -> io::Result<()> {
            self.call(ModeCommand::LeaveAlternate)
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            self.call(ModeCommand::HideCursor)
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            self.call(ModeCommand::ShowCursor)
        }

        fn enable_mouse_capture(&mut self) -> io::Result<()> {
            self.call(ModeCommand::EnableMouse)
        }

        fn disable_mouse_capture(&mut self) -> io::Result<()> {
            self.call(ModeCommand::DisableMouse)
        }
    }

    #[derive(Debug)]
    struct FakeEventSource {
        events: VecDeque<CrosstermEvent>,
    }

    impl FakeEventSource {
        fn new(events: impl IntoIterator<Item = CrosstermEvent>) -> Self {
            Self {
                events: events.into_iter().collect(),
            }
        }
    }

    impl EventSource for FakeEventSource {
        fn poll(&mut self, _timeout: Duration) -> io::Result<bool> {
            Ok(!self.events.is_empty())
        }

        fn read(&mut self) -> io::Result<CrosstermEvent> {
            self.events
                .pop_front()
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "event queue is empty"))
        }
    }

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 12,
            row: 7,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn key(kind: KeyEventKind, character: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(character),
            modifiers: KeyModifiers::NONE,
            kind,
            state: KeyEventState::NONE,
        }
    }

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
    fn scheduler_rate_must_produce_a_positive_representable_interval() {
        assert!(duration_per_rate(0).is_err());
        assert!(duration_per_rate(u32::MAX).is_err());
        assert_eq!(
            duration_per_rate(250).expect("250 Hz"),
            Duration::from_millis(4)
        );
        assert_eq!(
            duration_per_rate(crate::cli::MAX_TPS).expect("maximum scheduler rate"),
            Duration::from_millis(1)
        );
        assert!(duration_per_rate(crate::cli::MAX_TPS + 1).is_err());
    }

    #[test]
    fn repeated_or_released_keys_cannot_starve_scheduled_work() {
        assert!(is_physical_key_press(key(KeyEventKind::Press, 'f')));
        assert!(!is_physical_key_press(key(KeyEventKind::Repeat, 'f')));
        assert!(!is_physical_key_press(key(KeyEventKind::Release, 'f')));
    }

    #[test]
    fn pointer_event_carries_the_mouse_event_and_ingress_timestamp() {
        let event = mouse(MouseEventKind::Down(MouseButton::Left));
        let observed_at = Instant::now();
        let classified = classify_event(CrosstermEvent::Mouse(event), observed_at);

        let Some(UiEvent::Pointer {
            event: classified_event,
            observed_at: classified_at,
        }) = classified
        else {
            panic!("primary pointer down must be emitted");
        };
        assert_eq!(classified_event, event);
        assert_eq!(classified_at, observed_at);
    }

    #[test]
    fn pointer_classifier_ignores_every_event_except_primary_down() {
        for kind in [
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Moved,
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ] {
            assert_eq!(
                classify_event(CrosstermEvent::Mouse(mouse(kind)), Instant::now()),
                None
            );
        }
    }

    #[test]
    fn simultaneous_inputs_are_timestamped_in_one_bounded_batch_before_due_work() {
        let mut source = FakeEventSource::new([
            CrosstermEvent::Key(key(KeyEventKind::Press, 'f')),
            CrosstermEvent::Key(key(KeyEventKind::Press, 'j')),
        ]);
        let mut scheduler = EventScheduler::new(250, 120).expect("scheduler");
        scheduler.next_tick = Instant::now();
        scheduler.next_frame = Instant::now() + Duration::from_secs(1);

        let UiEvent::Key {
            event: first,
            observed_at: first_observed_at,
        } = scheduler.next_event(&mut source).expect("first input")
        else {
            panic!("first ready key must keep input priority");
        };
        let batch_completed_at = Instant::now();
        assert_eq!(first.code, KeyCode::Char('f'));
        assert!(source.events.is_empty());
        assert_eq!(scheduler.ready_events.len(), 1);

        let UiEvent::Key {
            event: second,
            observed_at: second_observed_at,
        } = scheduler.next_event(&mut source).expect("second input")
        else {
            panic!("second key must remain queued");
        };
        assert_eq!(second.code, KeyCode::Char('j'));
        assert!(first_observed_at <= second_observed_at);
        assert!(second_observed_at <= batch_completed_at);
        assert_eq!(
            scheduler.next_event(&mut source).expect("due tick"),
            UiEvent::Tick
        );
    }

    #[test]
    fn mouse_motion_backlog_cannot_starve_a_due_tick() {
        let moved = CrosstermEvent::Mouse(mouse(MouseEventKind::Moved));
        let mut source = FakeEventSource::new(std::iter::repeat_n(moved, 10_000));
        let mut scheduler = EventScheduler::new(250, 120).expect("scheduler");
        scheduler.next_tick = Instant::now();
        scheduler.next_frame = Instant::now() + Duration::from_secs(1);

        assert_eq!(
            scheduler.next_event(&mut source).expect("scheduled event"),
            UiEvent::Tick
        );
        assert_eq!(source.events.len(), 10_000 - MAX_INGRESS_EVENTS_PER_BATCH);
    }

    #[test]
    fn mouse_motion_backlog_cannot_starve_a_due_frame() {
        let moved = CrosstermEvent::Mouse(mouse(MouseEventKind::Moved));
        let mut source = FakeEventSource::new(std::iter::repeat_n(moved, 10_000));
        let mut scheduler = EventScheduler::new(250, 120).expect("scheduler");
        scheduler.next_tick = Instant::now() + Duration::from_secs(1);
        scheduler.next_frame = Instant::now();

        assert_eq!(
            scheduler.next_event(&mut source).expect("scheduled event"),
            UiEvent::Frame
        );
        assert_eq!(source.events.len(), 10_000 - MAX_INGRESS_EVENTS_PER_BATCH);
    }

    #[test]
    fn ready_pointer_down_behind_motion_keeps_gameplay_input_priority() {
        let moved = CrosstermEvent::Mouse(mouse(MouseEventKind::Moved));
        let pointer = CrosstermEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Left)));
        let preceding_motion = MAX_INGRESS_EVENTS_PER_BATCH - 1;
        let mut source =
            FakeEventSource::new(std::iter::repeat_n(moved, preceding_motion).chain([pointer]));
        let mut scheduler = EventScheduler::new(250, 120).expect("scheduler");
        scheduler.next_tick = Instant::now();
        scheduler.next_frame = Instant::now() + Duration::from_secs(1);

        assert!(matches!(
            scheduler.next_event(&mut source).expect("pointer event"),
            UiEvent::Pointer { .. }
        ));
        assert!(source.events.is_empty());
        assert_eq!(
            scheduler.next_event(&mut source).expect("due callback"),
            UiEvent::Tick
        );
    }

    #[test]
    fn relevant_input_flood_cannot_starve_scheduled_work_across_calls() {
        let pointer = CrosstermEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Left)));
        let mut source = FakeEventSource::new(std::iter::repeat_n(pointer, 10_000));
        let mut scheduler = EventScheduler::new(250, 120).expect("scheduler");
        scheduler.next_tick = Instant::now();
        scheduler.next_frame = Instant::now() + Duration::from_secs(1);

        assert!(matches!(
            scheduler.next_event(&mut source).expect("first input"),
            UiEvent::Pointer { .. }
        ));
        for _ in 1..MAX_INGRESS_EVENTS_PER_BATCH {
            assert!(matches!(
                scheduler
                    .next_event(&mut source)
                    .expect("bounded input batch"),
                UiEvent::Pointer { .. }
            ));
        }
        assert_eq!(
            scheduler.next_event(&mut source).expect("due callback"),
            UiEvent::Tick
        );
        assert_eq!(source.events.len(), 10_000 - MAX_INGRESS_EVENTS_PER_BATCH);
        assert!(scheduler.ready_events.is_empty());
    }

    #[test]
    fn simultaneous_overdue_work_runs_the_earliest_deadline_first() {
        let mut scheduler = EventScheduler::new(250, 120).expect("scheduler");
        let now = Instant::now();
        scheduler.next_tick = now - Duration::from_millis(1);
        scheduler.next_frame = now - Duration::from_secs(1);

        assert_eq!(
            scheduler.take_due_scheduled_event(now),
            Some(UiEvent::Frame)
        );

        scheduler.next_tick = now - Duration::from_secs(2);
        scheduler.next_frame = now - Duration::from_millis(1);
        assert_eq!(scheduler.take_due_scheduled_event(now), Some(UiEvent::Tick));
    }

    #[test]
    fn terminal_entry_and_mouse_capture_are_idempotent() {
        let mut modes = TerminalModes::default();
        let mut commands = FakeModeCommands::default();

        modes.enter(&mut commands).expect("enter");
        modes.enter(&mut commands).expect("re-enter");
        modes
            .set_mouse_capture(&mut commands, true)
            .expect("enable mouse");
        modes
            .set_mouse_capture(&mut commands, true)
            .expect("re-enable mouse");
        modes
            .set_mouse_capture(&mut commands, false)
            .expect("disable mouse");
        modes
            .set_mouse_capture(&mut commands, false)
            .expect("re-disable mouse");

        assert_eq!(
            commands.calls,
            [
                ModeCommand::EnableRaw,
                ModeCommand::QueryKeyboardEnhancement,
                ModeCommand::PushKeyboardEnhancement,
                ModeCommand::EnterAlternate,
                ModeCommand::HideCursor,
                ModeCommand::EnableMouse,
                ModeCommand::DisableMouse,
            ]
        );
        assert!(modes.is_entered());
        assert!(!modes.mouse_capture);
    }

    #[test]
    fn unsupported_keyboard_event_types_enter_without_claiming_repeat_detection() {
        let mut modes = TerminalModes::default();
        let mut commands = FakeModeCommands {
            keyboard_enhancement_unsupported: true,
            ..FakeModeCommands::default()
        };

        modes
            .enter(&mut commands)
            .expect("pointer and LAN controllers remain usable");

        assert_eq!(
            commands.calls,
            [
                ModeCommand::EnableRaw,
                ModeCommand::QueryKeyboardEnhancement,
                ModeCommand::EnterAlternate,
                ModeCommand::HideCursor,
            ]
        );
        assert!(modes.is_entered());
        assert!(!modes.keyboard_enhancement);

        modes.exit(&mut commands).expect("exit");
        assert_eq!(
            &commands.calls[4..],
            [
                ModeCommand::ShowCursor,
                ModeCommand::LeaveAlternate,
                ModeCommand::DisableRaw,
            ]
        );
        assert_eq!(modes, TerminalModes::default());
    }

    #[test]
    fn keyboard_capability_query_failure_restores_raw_mode() {
        let mut modes = TerminalModes::default();
        let mut commands = FakeModeCommands::default();
        commands
            .failures
            .insert(ModeCommand::QueryKeyboardEnhancement);

        let error = modes
            .enter(&mut commands)
            .expect_err("keyboard capability query must fail");

        assert!(error
            .to_string()
            .contains("QueryKeyboardEnhancement rejected"));
        assert_eq!(
            commands.calls,
            [
                ModeCommand::EnableRaw,
                ModeCommand::QueryKeyboardEnhancement,
                ModeCommand::DisableRaw,
            ]
        );
        assert_eq!(modes, TerminalModes::default());
    }

    #[test]
    fn failed_keyboard_enhancement_push_is_popped_during_entry_rollback() {
        let mut modes = TerminalModes::default();
        let mut commands = FakeModeCommands::default();
        commands
            .failures
            .insert(ModeCommand::PushKeyboardEnhancement);

        let error = modes
            .enter(&mut commands)
            .expect_err("keyboard enhancement push must fail");

        assert!(error
            .to_string()
            .contains("PushKeyboardEnhancement rejected"));
        assert_eq!(
            commands.calls,
            [
                ModeCommand::EnableRaw,
                ModeCommand::QueryKeyboardEnhancement,
                ModeCommand::PushKeyboardEnhancement,
                ModeCommand::PopKeyboardEnhancement,
                ModeCommand::DisableRaw,
            ]
        );
        assert_eq!(modes, TerminalModes::default());
    }

    #[test]
    fn mouse_capture_cannot_be_enabled_before_terminal_entry() {
        let mut modes = TerminalModes::default();
        let mut commands = FakeModeCommands::default();

        let error = modes
            .set_mouse_capture(&mut commands, true)
            .expect_err("capture requires terminal");

        assert!(error.to_string().contains("entered terminal"));
        assert!(commands.calls.is_empty());
        assert_eq!(modes, TerminalModes::default());
    }

    #[test]
    fn failed_mouse_capture_commands_preserve_retryable_state() {
        let mut modes = TerminalModes::default();
        let mut commands = FakeModeCommands::default();
        modes.enter(&mut commands).expect("enter");
        commands.failures.insert(ModeCommand::EnableMouse);

        modes
            .set_mouse_capture(&mut commands, true)
            .expect_err("enable failure");
        assert!(!modes.mouse_capture);
        assert_eq!(
            &commands.calls[5..],
            [ModeCommand::EnableMouse, ModeCommand::DisableMouse]
        );

        commands.failures.clear();
        modes
            .set_mouse_capture(&mut commands, true)
            .expect("retry enable");
        commands.failures.insert(ModeCommand::DisableMouse);
        modes
            .set_mouse_capture(&mut commands, false)
            .expect_err("disable failure");
        assert!(modes.mouse_capture);

        commands.failures.clear();
        modes
            .set_mouse_capture(&mut commands, false)
            .expect("retry disable");
        assert!(!modes.mouse_capture);
    }

    #[test]
    fn failed_mouse_enable_and_rollback_remains_marked_for_exit_retry() {
        let mut modes = TerminalModes::default();
        let mut commands = FakeModeCommands::default();
        modes.enter(&mut commands).expect("enter");
        commands
            .failures
            .extend([ModeCommand::EnableMouse, ModeCommand::DisableMouse]);

        let error = modes
            .set_mouse_capture(&mut commands, true)
            .expect_err("enable and rollback fail");

        assert!(error.to_string().contains("EnableMouse rejected"));
        assert!(error
            .to_string()
            .contains("mouse capture rollback also failed"));
        assert!(modes.mouse_capture);

        commands.failures.clear();
        modes
            .set_mouse_capture(&mut commands, false)
            .expect("cleanup retry");
        assert!(!modes.mouse_capture);
    }

    #[test]
    fn terminal_exit_runs_every_cleanup_in_reverse_order() {
        let mut modes = TerminalModes::default();
        let mut commands = FakeModeCommands::default();
        modes.enter(&mut commands).expect("enter");
        modes
            .set_mouse_capture(&mut commands, true)
            .expect("capture");

        modes.exit(&mut commands).expect("exit");

        assert_eq!(
            &commands.calls[6..],
            [
                ModeCommand::DisableMouse,
                ModeCommand::ShowCursor,
                ModeCommand::LeaveAlternate,
                ModeCommand::PopKeyboardEnhancement,
                ModeCommand::DisableRaw,
            ]
        );
        assert_eq!(modes, TerminalModes::default());
    }

    #[test]
    fn terminal_exit_aggregates_failures_and_retries_only_unrestored_modes() {
        let mut modes = TerminalModes::default();
        let mut commands = FakeModeCommands::default();
        modes.enter(&mut commands).expect("enter");
        modes
            .set_mouse_capture(&mut commands, true)
            .expect("capture");
        commands
            .failures
            .extend([ModeCommand::ShowCursor, ModeCommand::LeaveAlternate]);

        let error = modes.exit(&mut commands).expect_err("two cleanup failures");

        assert!(error.to_string().contains("show cursor failed"));
        assert!(error
            .to_string()
            .contains("leave alternate screen also failed"));
        assert!(!modes.mouse_capture);
        assert!(modes.cursor_hidden);
        assert!(modes.alternate_screen);
        assert!(!modes.keyboard_enhancement);
        assert!(!modes.raw_mode);
        assert_eq!(
            &commands.calls[6..],
            [
                ModeCommand::DisableMouse,
                ModeCommand::ShowCursor,
                ModeCommand::LeaveAlternate,
                ModeCommand::PopKeyboardEnhancement,
                ModeCommand::DisableRaw,
            ]
        );

        commands.failures.clear();
        let before_retry = commands.calls.len();
        modes.exit(&mut commands).expect("retry cleanup");
        assert_eq!(
            &commands.calls[before_retry..],
            [ModeCommand::ShowCursor, ModeCommand::LeaveAlternate]
        );
        assert_eq!(modes, TerminalModes::default());
    }

    #[test]
    fn failed_terminal_entry_rolls_back_every_applied_mode() {
        let mut modes = TerminalModes::default();
        let mut commands = FakeModeCommands::default();
        commands.failures.insert(ModeCommand::HideCursor);

        let error = modes.enter(&mut commands).expect_err("hide failure");

        assert!(error.to_string().contains("HideCursor rejected"));
        assert_eq!(
            commands.calls,
            [
                ModeCommand::EnableRaw,
                ModeCommand::QueryKeyboardEnhancement,
                ModeCommand::PushKeyboardEnhancement,
                ModeCommand::EnterAlternate,
                ModeCommand::HideCursor,
                ModeCommand::ShowCursor,
                ModeCommand::LeaveAlternate,
                ModeCommand::PopKeyboardEnhancement,
                ModeCommand::DisableRaw,
            ]
        );
        assert_eq!(modes, TerminalModes::default());
    }

    #[test]
    fn failed_keyboard_enhancement_pop_remains_marked_for_exit_retry() {
        let mut modes = TerminalModes::default();
        let mut commands = FakeModeCommands::default();
        modes.enter(&mut commands).expect("enter");
        commands
            .failures
            .insert(ModeCommand::PopKeyboardEnhancement);

        let error = modes.exit(&mut commands).expect_err("pop failure");

        assert!(error
            .to_string()
            .contains("pop keyboard enhancement failed"));
        assert!(modes.keyboard_enhancement);
        assert!(!modes.raw_mode);
        assert!(!modes.alternate_screen);
        assert!(!modes.cursor_hidden);

        commands.failures.clear();
        let before_retry = commands.calls.len();
        modes.exit(&mut commands).expect("retry pop");
        assert_eq!(
            &commands.calls[before_retry..],
            [ModeCommand::PopKeyboardEnhancement]
        );
        assert_eq!(modes, TerminalModes::default());
    }
}
