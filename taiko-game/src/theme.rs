use ratatui::style::{Color, Modifier, Style};
use rhythm_mode_taiko::{TaikoAction, TaikoJudge, TaikoZone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerfMetricKind {
    TickP95Ms,
    FrameP95Ms,
    Tps,
    Fps,
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub text_primary: Style,
    pub text_secondary: Style,
    pub border: Style,
    pub title: Style,
    pub selection: Style,
    pub label: Style,
    pub value: Style,
    pub metadata: Style,
    pub warning: Style,
    pub error: Style,
    pub success: Style,
    pub judge_great: Style,
    pub judge_ok: Style,
    pub judge_miss: Style,
    pub judge_roll: Style,
    pub lane_track: Style,
    pub lane_gogo_edge: Style,
    pub lane_bar_line: Style,
    pub lane_note_don: Style,
    pub lane_note_kat: Style,
    pub lane_note_roll: Style,
    pub hit_zone_base: Style,
    pub judge_base_great: Style,
    pub judge_base_ok: Style,
    pub judge_base_miss: Style,
    pub marker_flash_don: Style,
    pub marker_flash_kat: Style,
    pub balloon: Style,
    pub gauge_low: Style,
    pub gauge_mid: Style,
    pub gauge_high: Style,
    pub gauge_full: Style,
}

impl Theme {
    pub fn detect() -> ColorMode {
        Self::detect_from_no_color(std::env::var_os("NO_COLOR"))
    }

    pub fn taiko_vivid(mode: ColorMode) -> Self {
        match mode {
            ColorMode::Enabled => Self {
                text_primary: Style::default().fg(Color::White),
                text_secondary: Style::default().fg(Color::DarkGray),
                border: Style::default().fg(Color::Blue),
                title: Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                selection: Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                label: Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                value: Style::default().fg(Color::White),
                metadata: Style::default().fg(Color::Gray),
                warning: Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                error: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                success: Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                judge_great: Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                judge_ok: Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
                judge_miss: Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
                judge_roll: Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                lane_track: Style::default().bg(Color::DarkGray),
                lane_gogo_edge: Style::default().bg(Color::Rgb(148, 72, 48)),
                lane_bar_line: Style::default().bg(Color::Rgb(86, 98, 120)),
                lane_note_don: Style::default()
                    .fg(Color::White)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
                lane_note_kat: Style::default()
                    .fg(Color::White)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                lane_note_roll: Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                hit_zone_base: Style::default()
                    .fg(Color::White)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
                judge_base_great: Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                judge_base_ok: Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD),
                judge_base_miss: Style::default()
                    .fg(Color::White)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
                marker_flash_don: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                marker_flash_kat: Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                balloon: Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
                gauge_low: Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
                gauge_mid: Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
                gauge_high: Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                gauge_full: Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            },
            ColorMode::Disabled => Self {
                text_primary: Style::default().fg(Color::Reset),
                text_secondary: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::DIM),
                border: Style::default().fg(Color::Reset),
                title: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
                selection: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                label: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
                value: Style::default().fg(Color::Reset),
                metadata: Style::default().fg(Color::Reset),
                warning: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
                error: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
                success: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
                judge_great: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
                judge_ok: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
                judge_miss: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
                judge_roll: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
                lane_track: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::DIM),
                lane_gogo_edge: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::DIM | Modifier::REVERSED),
                lane_bar_line: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::DIM | Modifier::REVERSED),
                lane_note_don: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                lane_note_kat: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                lane_note_roll: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                hit_zone_base: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
                judge_base_great: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                judge_base_ok: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                judge_base_miss: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                marker_flash_don: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                marker_flash_kat: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                balloon: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
                gauge_low: Style::default().fg(Color::Reset),
                gauge_mid: Style::default().fg(Color::Reset),
                gauge_high: Style::default().fg(Color::Reset),
                gauge_full: Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
            },
        }
    }

    pub fn judge_style(&self, judge: TaikoJudge) -> Style {
        match judge {
            TaikoJudge::Great { .. } => self.judge_great,
            TaikoJudge::Ok { .. } => self.judge_ok,
            TaikoJudge::Miss { .. } | TaikoJudge::MissExpired => self.judge_miss,
            TaikoJudge::RollHit => self.judge_roll,
            TaikoJudge::Ignored => self.text_secondary,
        }
    }

    pub fn judge_base_style(&self, judge: TaikoJudge) -> Style {
        match judge {
            TaikoJudge::Great { .. } => self.judge_base_great,
            TaikoJudge::Ok { .. } => self.judge_base_ok,
            TaikoJudge::Miss { .. } | TaikoJudge::MissExpired => self.judge_base_miss,
            TaikoJudge::RollHit | TaikoJudge::Ignored => self.hit_zone_base,
        }
    }

    pub fn marker_flash_style(&self, action: TaikoAction) -> Style {
        match action.zone {
            TaikoZone::Don => self.marker_flash_don,
            TaikoZone::Kat => self.marker_flash_kat,
        }
    }

    pub fn hit_zone_base_style(&self) -> Style {
        self.hit_zone_base
    }

    pub fn gauge_style(&self, gauge: f32, pass_threshold: f32) -> Style {
        let pass_threshold = pass_threshold.clamp(0.0, 1.0);
        let mid_threshold = (pass_threshold * 0.5).max(0.35);
        if gauge >= 1.0 {
            self.gauge_full
        } else if gauge >= pass_threshold {
            self.gauge_high
        } else if gauge >= mid_threshold {
            self.gauge_mid
        } else {
            self.gauge_low
        }
    }

    pub fn perf_style(&self, metric_kind: PerfMetricKind, value: f64) -> Style {
        match metric_kind {
            PerfMetricKind::TickP95Ms => {
                if value <= 2.0 {
                    self.success
                } else if value <= 3.0 {
                    self.warning
                } else {
                    self.error
                }
            }
            PerfMetricKind::FrameP95Ms => {
                if value <= 8.3 {
                    self.success
                } else if value <= 12.0 {
                    self.warning
                } else {
                    self.error
                }
            }
            PerfMetricKind::Tps => {
                if value >= 500.0 {
                    self.success
                } else if value >= 350.0 {
                    self.warning
                } else {
                    self.error
                }
            }
            PerfMetricKind::Fps => {
                if value >= 120.0 {
                    self.success
                } else if value >= 90.0 {
                    self.warning
                } else {
                    self.error
                }
            }
        }
    }

    fn detect_from_no_color(value: Option<std::ffi::OsString>) -> ColorMode {
        match value {
            Some(v) if !v.is_empty() => ColorMode::Disabled,
            _ => ColorMode::Enabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{
        backend::TestBackend,
        style::{Color, Modifier},
        widgets::{List, ListItem, ListState},
        Terminal,
    };

    use super::{ColorMode, PerfMetricKind, Theme};

    #[test]
    fn detect_no_color_behavior() {
        assert_eq!(
            Theme::detect_from_no_color(None),
            ColorMode::Enabled,
            "unset NO_COLOR should enable colors"
        );
        assert_eq!(
            Theme::detect_from_no_color(Some("".into())),
            ColorMode::Enabled,
            "empty NO_COLOR should enable colors"
        );
        assert_eq!(
            Theme::detect_from_no_color(Some("1".into())),
            ColorMode::Disabled,
            "non-empty NO_COLOR should disable colors"
        );
    }

    #[test]
    fn disabled_theme_keeps_readability_modifiers() {
        let theme = Theme::taiko_vivid(ColorMode::Disabled);

        assert_eq!(theme.selection.fg, Some(Color::Reset));
        assert!(theme.selection.add_modifier.contains(Modifier::BOLD));
        assert!(theme.selection.add_modifier.contains(Modifier::REVERSED));

        assert!(theme.title.add_modifier.contains(Modifier::BOLD));
        assert!(theme.error.add_modifier.contains(Modifier::BOLD));
        assert!(
            theme
                .lane_gogo_edge
                .add_modifier
                .contains(Modifier::REVERSED),
            "the Go-Go edge must remain visible without color"
        );
        assert!(
            !theme.lane_track.add_modifier.contains(Modifier::REVERSED),
            "the main lane must not adopt the Go-Go edge treatment"
        );
    }

    #[test]
    fn colored_gogo_edge_is_a_fixed_muted_orange_distinct_from_the_track() {
        let theme = Theme::taiko_vivid(ColorMode::Enabled);

        assert_eq!(theme.lane_gogo_edge.bg, Some(Color::Rgb(148, 72, 48)));
        assert_ne!(theme.lane_gogo_edge.bg, theme.lane_track.bg);
        assert_ne!(theme.lane_bar_line.bg, theme.lane_track.bg);
    }

    #[test]
    fn judge_gauge_and_perf_styles_are_mapped() {
        let theme = Theme::taiko_vivid(ColorMode::Enabled);

        assert_eq!(
            theme.judge_style(rhythm_mode_taiko::TaikoJudge::Great { delta_tick: 0 }),
            theme.judge_great
        );
        assert_eq!(
            theme.judge_style(rhythm_mode_taiko::TaikoJudge::Ok { delta_tick: 0 }),
            theme.judge_ok
        );
        assert_eq!(
            theme.judge_style(rhythm_mode_taiko::TaikoJudge::Miss { delta_tick: 0 }),
            theme.judge_miss
        );
        assert_eq!(
            theme.judge_style(rhythm_mode_taiko::TaikoJudge::MissExpired),
            theme.judge_miss
        );
        assert_eq!(theme.judge_great.fg, Some(Color::Yellow));
        assert_eq!(theme.judge_ok.fg, Some(Color::White));
        assert_eq!(theme.judge_miss.fg, Some(Color::Blue));

        assert_eq!(
            theme.judge_base_style(rhythm_mode_taiko::TaikoJudge::Great { delta_tick: 0 }),
            theme.judge_base_great
        );
        assert_eq!(
            theme.judge_base_style(rhythm_mode_taiko::TaikoJudge::Ok { delta_tick: 0 }),
            theme.judge_base_ok
        );
        assert_eq!(
            theme.judge_base_style(rhythm_mode_taiko::TaikoJudge::Miss { delta_tick: 0 }),
            theme.judge_base_miss
        );
        assert_eq!(
            theme.judge_base_style(rhythm_mode_taiko::TaikoJudge::MissExpired),
            theme.judge_base_miss
        );

        assert_eq!(theme.gauge_style(0.2, 0.8), theme.gauge_low);
        assert_eq!(theme.gauge_style(0.7, 0.8), theme.gauge_mid);
        assert_eq!(theme.gauge_style(0.9, 0.8), theme.gauge_high);
        assert_eq!(theme.gauge_style(1.0, 0.8), theme.gauge_full);

        assert_eq!(
            theme.perf_style(PerfMetricKind::TickP95Ms, 1.8),
            theme.success
        );
        assert_eq!(
            theme.perf_style(PerfMetricKind::TickP95Ms, 2.5),
            theme.warning
        );
        assert_eq!(
            theme.perf_style(PerfMetricKind::TickP95Ms, 4.0),
            theme.error
        );
    }

    #[test]
    fn selection_style_renders_in_buffer() {
        let theme = Theme::taiko_vivid(ColorMode::Enabled);
        let backend = TestBackend::new(20, 3);
        let mut terminal = Terminal::new(backend).expect("terminal");

        let mut state = ListState::default();
        state.select(Some(0));

        terminal
            .draw(|frame| {
                let items = vec![ListItem::new("song")];
                let list = List::new(items).highlight_style(theme.selection);
                frame.render_stateful_widget(list, frame.area(), &mut state);
            })
            .expect("draw");

        let has_selection_fg = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::style)
            .any(|style| style.fg == theme.selection.fg);
        assert!(has_selection_fg);
    }
}
