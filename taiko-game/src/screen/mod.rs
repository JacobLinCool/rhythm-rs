pub mod course_menu;
pub mod error_screen;
pub mod game_screen;
pub mod load_warnings_screen;
pub mod result_screen;
pub mod song_menu;

use std::sync::OnceLock;
use std::time::Instant;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Page};
use crate::theme::ColorMode;
use crate::tui::Frame;

const GAUGE_BAR_MIN_WIDTH: usize = 12;
const GAUGE_BAR_MAX_WIDTH: usize = 60;
const GAUGE_FIXED_COLUMNS: usize = 24;

pub fn render_topbar(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let chunks =
        Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)]).split(area);

    let left = match app.page {
        Page::SongMenu => format!(
            "Taiko on Terminal | Song Menu | songs={}/{} | auto={} | tps={}",
            app.visible_song_count(),
            app.songs.len(),
            app.auto_play,
            app.args.tps
        ),
        Page::LoadWarnings => "Taiko on Terminal | Load Warnings".to_owned(),
        Page::CourseMenu => {
            let title = app
                .selected_song()
                .map_or("<none>", |song| song.title.as_str());
            format!("Taiko on Terminal | Course Menu | {title}")
        }
        Page::Game => {
            if let Some(game) = app.game.as_ref() {
                let now_sec = game.last_output.now as f64 / 1_000_000.0;
                if game.paused {
                    format!(
                        "Taiko on Terminal | {} | t={now_sec:.2}s | PAUSED",
                        game.course_name
                    )
                } else {
                    format!("Taiko on Terminal | {} | t={now_sec:.2}s", game.course_name)
                }
            } else {
                "Taiko on Terminal | Game".to_owned()
            }
        }
        Page::Result => "Taiko on Terminal | Result".to_owned(),
        Page::Error => "Taiko on Terminal | Error".to_owned(),
    };

    let right = {
        let snapshot = app.perf_meter.snapshot();
        let color_mode = match app.theme.mode {
            ColorMode::Enabled => "color:on",
            ColorMode::Disabled => "color:off",
        };
        if snapshot.tick.avg_ms <= f64::EPSILON {
            format!("branch=auto auto-play={} {}", app.auto_play, color_mode)
        } else {
            format!(
                "tick {:.2}/{:.2} ms frame {:.2}/{:.2} ms {}",
                snapshot.tick.avg_ms,
                snapshot.tick.p95_ms,
                snapshot.frame.avg_ms,
                snapshot.frame.p95_ms,
                color_mode
            )
        }
    };

    let left_widget = Paragraph::new(Line::from(Span::styled(left, app.theme.text_secondary)));
    frame.render_widget(left_widget, chunks[0]);

    let right_widget =
        Paragraph::new(Line::from(Span::styled(right, app.theme.text_secondary))).right_aligned();
    frame.render_widget(right_widget, chunks[1]);
}

pub(crate) fn render_gauge_bar_line(
    app: &App,
    gauge: f32,
    pass_threshold: f32,
    available_columns: u16,
    show_status: bool,
) -> Line<'static> {
    let gauge = gauge.clamp(0.0, 1.0);
    let pass_threshold = pass_threshold.clamp(0.0, 1.0);
    let full_blink_on = full_gauge_blink_on();
    let bar_width = gauge_bar_width(available_columns);
    let fill_count = (gauge * bar_width as f32).round() as usize;
    let pass_idx = threshold_index(bar_width, pass_threshold);
    let full_idx = bar_width.saturating_sub(1);

    let mut spans = Vec::with_capacity(bar_width + 10);
    spans.push(Span::styled("Gauge [", app.theme.label));

    for idx in 0..bar_width {
        let mut symbol = if idx < fill_count { "=" } else { "-" };
        let mut style = if idx < fill_count {
            gauge_fill_style(app, gauge, pass_threshold, full_blink_on)
        } else {
            app.theme.text_secondary
        };

        if idx == pass_idx {
            symbol = "|";
            style = if gauge >= pass_threshold {
                app.theme.warning
            } else {
                app.theme.text_secondary
            };
        }
        if idx == full_idx {
            symbol = "|";
            style = if gauge >= 1.0 {
                gauge_fill_style(app, gauge, pass_threshold, full_blink_on)
            } else {
                app.theme.metadata
            };
        }

        spans.push(Span::styled(symbol, style));
    }

    spans.push(Span::styled("] ", app.theme.label));
    spans.push(Span::styled(
        format!("{:>6.2}%", gauge * 100.0),
        gauge_fill_style(app, gauge, pass_threshold, full_blink_on),
    ));
    if show_status {
        spans.push(Span::styled(" ", app.theme.text_primary));

        let (status, status_style) = if gauge >= 1.0 {
            (
                "FULL",
                gauge_fill_style(app, gauge, pass_threshold, full_blink_on),
            )
        } else if gauge >= pass_threshold {
            ("PASS", app.theme.warning)
        } else {
            ("FAIL", app.theme.error)
        };
        spans.push(Span::styled(status, status_style));
    }

    Line::from(spans)
}

fn gauge_fill_style(app: &App, gauge: f32, pass_threshold: f32, full_blink_on: bool) -> Style {
    if gauge >= 1.0 {
        if full_blink_on {
            app.theme.gauge_full
        } else {
            app.theme.warning
        }
    } else {
        app.theme.gauge_style(gauge, pass_threshold)
    }
}

fn full_gauge_blink_on() -> bool {
    static START: OnceLock<Instant> = OnceLock::new();
    let elapsed = START.get_or_init(Instant::now).elapsed();
    (elapsed.as_millis() / 180).is_multiple_of(2)
}

pub(crate) fn gauge_bar_width(available_columns: u16) -> usize {
    let dynamic = usize::from(available_columns).saturating_sub(GAUGE_FIXED_COLUMNS);
    dynamic.clamp(GAUGE_BAR_MIN_WIDTH, GAUGE_BAR_MAX_WIDTH)
}

fn threshold_index(bar_width: usize, threshold: f32) -> usize {
    if bar_width == 0 {
        return 0;
    }
    let last = bar_width - 1;
    ((threshold * last as f32).round() as usize).min(last)
}

#[cfg(test)]
mod tests {
    use super::gauge_bar_width;

    #[test]
    fn gauge_bar_width_is_clamped() {
        assert_eq!(gauge_bar_width(10), 12);
        assert_eq!(gauge_bar_width(36), 12);
        assert_eq!(gauge_bar_width(54), 30);
        assert_eq!(gauge_bar_width(200), 60);
    }
}
