use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use rhythm_mode_taiko::{
    TaikoAction, TaikoDisplayKind, TaikoFrameView, TaikoJudge, LANE_BOTH, LANE_DON, LANE_KAT,
};

use super::render_gauge_bar_line;
use crate::app::App;
use crate::theme::Theme;
use crate::tui::Frame;

const LOOKAHEAD_TICKS: i64 = 2_000_000;
const LOOKBACK_TICKS: i64 = 300_000;
const HIT_X_DIVISOR: usize = 6;
const HIT_X_LEFT_SHIFT: usize = 1;

#[derive(Debug, Clone, Copy)]
pub struct LaneRenderOptions {
    pub scroll_speed: f32,
    pub paused: bool,
    pub judge_flash: Option<TaikoJudge>,
    pub input_flash: Option<TaikoAction>,
}

impl LaneRenderOptions {
    pub fn standard(scroll_speed: f32) -> Self {
        Self {
            scroll_speed,
            paused: false,
            judge_flash: None,
            input_flash: None,
        }
    }
}

struct LaneRows<'a> {
    label: &'a mut [Span<'static>],
    top: &'a mut [Span<'static>],
    middle: &'a mut [Span<'static>],
    bottom: &'a mut [Span<'static>],
}

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(game) = app.game.as_ref() else {
        let empty = Paragraph::new(Span::styled("Game session missing", app.theme.error))
            .block(themed_block(app, "Game"));
        frame.render_widget(empty, area);
        return;
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Min(3),
        ])
        .split(area);

    let score = &game.last_output.score;
    let hud = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Score ", app.theme.label),
            Span::styled(format!("{:>9}", score.score), app.theme.value),
            Span::styled(" | Combo ", app.theme.label),
            Span::styled(format!("{:>4}", score.combo), app.theme.value),
            Span::styled(" | Max ", app.theme.label),
            Span::styled(format!("{:>4}", score.max_combo), app.theme.value),
        ]),
        render_gauge_bar_line(
            app,
            score.gauge,
            score.pass_threshold,
            layout[0].width.saturating_sub(2),
            false,
        ),
        Line::from(vec![
            Span::styled("GREAT ", app.theme.label),
            Span::styled(
                format!("{:>4}", score.great),
                app.theme.judge_style(TaikoJudge::Great { delta_tick: 0 }),
            ),
            Span::styled(" | OK ", app.theme.label),
            Span::styled(
                format!("{:>4}", score.ok),
                app.theme.judge_style(TaikoJudge::Ok { delta_tick: 0 }),
            ),
            Span::styled(" | MISS ", app.theme.label),
            Span::styled(
                format!("{:>4}", score.miss),
                app.theme.judge_style(TaikoJudge::Miss { delta_tick: 0 }),
            ),
            Span::styled(" | Roll Hits ", app.theme.label),
            Span::styled(
                format!("{:>4}", score.roll_hits),
                app.theme.judge_style(TaikoJudge::RollHit),
            ),
        ]),
        Line::from(vec![
            Span::styled("Auto: ", app.theme.label),
            Span::styled(app.auto_play.to_string(), app.theme.value),
            Span::styled(" | Paused: ", app.theme.label),
            Span::styled(
                game.paused.to_string(),
                if game.paused {
                    app.theme.warning
                } else {
                    app.theme.value
                },
            ),
            Span::styled(" | Replay: ", app.theme.label),
            Span::styled(
                format!("{:016x}", game.last_output.replay_hash),
                app.theme.metadata,
            ),
        ]),
    ])
    .block(themed_block_plain(app));
    frame.render_widget(hud, layout[0]);

    render_lane(app, frame, layout[1]);

    let help = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Controls: ", app.theme.label),
            Span::styled(
                "Don keys/Space, Kat keys, P=pause, Esc=back, Ctrl+C=quit",
                app.theme.metadata,
            ),
        ]),
        Line::from(vec![
            Span::styled("Note offset: ", app.theme.label),
            Span::styled(app.note_offset_label(), app.theme.value),
            Span::styled(" | Music offset: ", app.theme.label),
            Span::styled(app.music_offset_label(), app.theme.value),
            Span::styled(" | Total: ", app.theme.label),
            Span::styled(app.total_offset_label(), app.theme.value),
            Span::styled(" | Scroll: ", app.theme.label),
            Span::styled(app.scroll_speed_label(), app.theme.value),
            Span::styled(" | Tick now: ", app.theme.label),
            Span::styled(
                format!("{:.3}s", game.last_output.now as f64 / 1_000_000.0),
                app.theme.value,
            ),
        ]),
    ])
    .block(themed_block(app, "Input"));
    frame.render_widget(help, layout[2]);
}

fn render_lane(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(game) = app.game.as_ref() else {
        return;
    };

    render_lane_view(
        &app.theme,
        frame,
        area,
        &game.last_output.frame_view,
        LaneRenderOptions {
            scroll_speed: app.effective_scroll_speed(),
            paused: game.paused,
            judge_flash: game.judge_flash.map(|flash| flash.judge),
            input_flash: game.input_flash.map(|flash| flash.action),
        },
    );
}

pub fn render_lane_view(
    theme: &Theme,
    frame: &mut Frame<'_>,
    area: Rect,
    view: &TaikoFrameView,
    options: LaneRenderOptions,
) {
    let width = usize::from(area.width.saturating_sub(2).max(1));
    let hit_x = hit_x_for_width(width);
    // Shared row: balloon labels and top-side bar lines.
    // Draw bar lines first, then labels so labels stay visually on top.
    let mut label = vec![Span::styled(" ", theme.text_primary); width];
    let mut top = vec![Span::styled(" ", theme.text_primary); width];
    let mut middle = vec![Span::styled(" ", theme.lane_track); width];
    let mut bottom = vec![Span::styled(" ", theme.text_primary); width];
    let mut bar_bottom = vec![Span::styled(" ", theme.text_primary); width];

    let base_style = judge_base_style(theme, options.judge_flash);
    paint_hit_zone_base(&mut top, &mut middle, &mut bottom, hit_x, width, base_style);
    paint_bar_lines(
        &mut label,
        &mut bar_bottom,
        width,
        hit_x,
        view,
        options.scroll_speed,
        theme.lane_bar_line,
    );

    let mut rows = LaneRows {
        label: &mut label,
        top: &mut top,
        middle: &mut middle,
        bottom: &mut bottom,
    };
    paint_notes(theme, &mut rows, width, hit_x, view, options.scroll_speed);
    if options.paused {
        paint_centered_label(&mut label, "PAUSED", theme.warning);
    }

    let marker_style = input_marker_style(theme, options.input_flash);
    let marker_base_style = marker_base_style_for_hit_zone(&middle, hit_x, base_style);
    paint_hit_markers_overlay(
        &mut top,
        &mut middle,
        &mut bottom,
        hit_x,
        marker_base_style,
        marker_style,
    );

    let lane = Paragraph::new(vec![
        Line::from(label),
        Line::from(top),
        Line::from(middle),
        Line::from(bottom),
        Line::from(bar_bottom),
    ])
    .block(themed_block_plain_theme(theme));
    frame.render_widget(lane, area);
}

fn paint_bar_lines(
    above: &mut [Span<'static>],
    below: &mut [Span<'static>],
    width: usize,
    hit_x: usize,
    view: &rhythm_mode_taiko::TaikoFrameView,
    scroll_speed: f32,
    style: Style,
) {
    for bar_line in &view.bar_lines {
        let bar_line_scroll_speed = scroll_speed * bar_line.scroll_multiplier();
        let Some(x) = to_x(
            bar_line.tick - view.now,
            width,
            hit_x,
            bar_line_scroll_speed,
        ) else {
            continue;
        };
        above[x] = Span::styled(" ", style);
        below[x] = Span::styled(" ", style);
    }
}

fn paint_notes(
    theme: &Theme,
    rows: &mut LaneRows<'_>,
    width: usize,
    hit_x: usize,
    view: &rhythm_mode_taiko::TaikoFrameView,
    global_scroll_speed: f32,
) {
    for note in &view.notes {
        let scroll_speed = global_scroll_speed * note.scroll_multiplier();
        match note.kind {
            TaikoDisplayKind::Tap => {
                let start_x = match to_x(note.start_tick - view.now, width, hit_x, scroll_speed) {
                    Some(v) => v,
                    None => continue,
                };
                let symbol = if note.is_big { "O" } else { "o" };
                paint_note_blob(
                    rows.top,
                    rows.middle,
                    rows.bottom,
                    start_x,
                    width,
                    lane_style(theme, note.lane),
                    symbol,
                );
            }
            TaikoDisplayKind::Roll | TaikoDisplayKind::Balloon => {
                let Some((l, r)) = projected_span_x(
                    note.start_tick,
                    note.end_tick,
                    view.now,
                    width,
                    hit_x,
                    scroll_speed,
                ) else {
                    continue;
                };

                for cell in rows.top.iter_mut().take(r.min(width - 1) + 1).skip(l) {
                    *cell = Span::styled(" ", theme.lane_note_roll);
                }
                for cell in rows.middle.iter_mut().take(r.min(width - 1) + 1).skip(l) {
                    *cell = Span::styled("=", theme.lane_note_roll);
                }
                for cell in rows.bottom.iter_mut().take(r.min(width - 1) + 1).skip(l) {
                    *cell = Span::styled(" ", theme.lane_note_roll);
                }

                if matches!(note.kind, TaikoDisplayKind::Balloon) {
                    rows.label[l] = Span::styled(format!("{}", note.remaining_hits), theme.balloon);
                }
            }
        }
    }
}

fn paint_centered_label(label: &mut [Span<'static>], text: &str, style: Style) {
    if label.is_empty() || text.is_empty() {
        return;
    }

    let text_chars = text.chars().collect::<Vec<_>>();
    let start = label.len().saturating_sub(text_chars.len()) / 2;
    for (idx, ch) in text_chars.into_iter().enumerate() {
        let col = start + idx;
        if col >= label.len() {
            break;
        }
        label[col] = Span::styled(ch.to_string(), style);
    }
}

pub(crate) fn projection_span_for_viewport_width(viewport_width: u16) -> usize {
    let width = usize::from(viewport_width.saturating_sub(2).max(1));
    let hit_x = hit_x_for_width(width);
    width.saturating_sub(hit_x + 1).max(1)
}

fn lane_style(theme: &Theme, lane: u16) -> Style {
    match lane {
        LANE_DON => theme.lane_note_don,
        LANE_KAT => theme.lane_note_kat,
        LANE_BOTH => theme.lane_note_roll,
        _ => theme.value,
    }
}

fn paint_note_blob(
    top: &mut [Span<'static>],
    middle: &mut [Span<'static>],
    bottom: &mut [Span<'static>],
    x: usize,
    width: usize,
    style: Style,
    symbol: &'static str,
) {
    let right = (x + 1).min(width.saturating_sub(1));
    for col in x..=right {
        top[col] = Span::styled(" ", style);
        middle[col] = Span::styled(" ", style);
        bottom[col] = Span::styled(" ", style);
    }
    middle[x] = Span::styled(symbol, style);
}

fn paint_hit_zone_base(
    top: &mut [Span<'static>],
    middle: &mut [Span<'static>],
    bottom: &mut [Span<'static>],
    hit_x: usize,
    width: usize,
    style: Style,
) {
    let left = hit_x.saturating_sub(1);
    let right = (hit_x + 1).min(width.saturating_sub(1));
    for col in left..=right {
        top[col] = Span::styled(" ", style);
        middle[col] = Span::styled(" ", style);
        bottom[col] = Span::styled(" ", style);
    }
}

fn paint_hit_markers_overlay(
    top: &mut [Span<'static>],
    middle: &mut [Span<'static>],
    bottom: &mut [Span<'static>],
    hit_x: usize,
    base_style: Style,
    marker_style: Style,
) {
    let style = base_style.patch(marker_style);
    top[hit_x] = Span::styled("|", style);
    middle[hit_x] = Span::styled("◎", style);
    bottom[hit_x] = Span::styled("|", style);
}

fn marker_base_style_for_hit_zone(
    middle: &[Span<'static>],
    hit_x: usize,
    default_style: Style,
) -> Style {
    let Some(cell) = middle.get(hit_x) else {
        return default_style;
    };

    if cell.content.as_ref() == "=" {
        cell.style
    } else {
        default_style
    }
}

fn judge_base_style(theme: &Theme, judge_flash: Option<TaikoJudge>) -> Style {
    judge_flash
        .map(|judge| theme.judge_base_style(judge))
        .unwrap_or_else(|| theme.hit_zone_base_style())
}

fn input_marker_style(theme: &Theme, input_flash: Option<TaikoAction>) -> Style {
    input_flash
        .map(|action| theme.marker_flash_style(action))
        .unwrap_or(theme.value)
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}

fn themed_block_plain(app: &App) -> Block<'_> {
    themed_block_plain_theme(&app.theme)
}

fn themed_block_plain_theme(theme: &Theme) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
}

fn to_x(delta_tick: i64, width: usize, hit_x: usize, scroll_speed: f32) -> Option<usize> {
    let (lookback_ticks, lookahead_ticks) = projection_window_ticks(scroll_speed);
    let forward_is_right = normalized_scroll_speed(scroll_speed).is_sign_positive();

    if !(-lookback_ticks..=lookahead_ticks).contains(&delta_tick) {
        return None;
    }

    if delta_tick >= 0 {
        let span = if forward_is_right {
            (width.saturating_sub(hit_x + 1)).max(1)
        } else {
            hit_x.max(1)
        };
        let ratio = delta_tick as f64 / lookahead_ticks.max(1) as f64;
        let offset = (ratio * span as f64).round() as isize;
        let x = if forward_is_right {
            hit_x as isize + offset
        } else {
            hit_x as isize - offset
        };
        Some(x.clamp(0, (width - 1) as isize) as usize)
    } else {
        let span = if forward_is_right {
            hit_x.max(1)
        } else {
            (width.saturating_sub(hit_x + 1)).max(1)
        };
        let ratio = (-delta_tick) as f64 / lookback_ticks.max(1) as f64;
        let offset = (ratio * span as f64).round() as isize;
        let x = if forward_is_right {
            hit_x as isize - offset
        } else {
            hit_x as isize + offset
        };
        Some(x.clamp(0, (width - 1) as isize) as usize)
    }
}

fn projection_window_ticks(scroll_speed: f32) -> (i64, i64) {
    let speed = normalized_scroll_speed(scroll_speed).abs();
    let lookahead_ticks = ((LOOKAHEAD_TICKS as f64) / speed).round() as i64;
    let lookback_ticks = ((LOOKBACK_TICKS as f64) / speed).round() as i64;
    (lookback_ticks.max(1), lookahead_ticks.max(1))
}

fn normalized_scroll_speed(scroll_speed: f32) -> f64 {
    let speed = if scroll_speed.is_finite() {
        f64::from(scroll_speed)
    } else {
        1.0
    };
    if speed.abs() < 0.01 {
        0.01_f64.copysign(speed)
    } else {
        speed
    }
}

fn projected_span_x(
    start_tick: i64,
    end_tick: i64,
    now: i64,
    width: usize,
    hit_x: usize,
    scroll_speed: f32,
) -> Option<(usize, usize)> {
    let (lookback_ticks, lookahead_ticks) = projection_window_ticks(scroll_speed);
    let forward_is_right = normalized_scroll_speed(scroll_speed).is_sign_positive();
    let start_delta = start_tick - now;
    let end_delta = end_tick - now;
    let roll_active_at_hit_zone = start_delta <= 0 && end_delta >= 0;

    if end_delta < -lookback_ticks || start_delta > lookahead_ticks {
        return None;
    }

    let clipped_start = start_delta.clamp(-lookback_ticks, lookahead_ticks);
    let clipped_end = end_delta.clamp(-lookback_ticks, lookahead_ticks);
    // Keep active-roll head inside the hit zone and avoid 1-cell visual bias from marker overlay.
    let start_x = if roll_active_at_hit_zone {
        if forward_is_right {
            hit_x.saturating_sub(1)
        } else {
            (hit_x + 1).min(width.saturating_sub(1))
        }
    } else {
        to_x(clipped_start, width, hit_x, scroll_speed)?
    };
    let end_x = to_x(clipped_end, width, hit_x, scroll_speed)?;

    if start_x <= end_x {
        Some((start_x, end_x))
    } else {
        Some((end_x, start_x))
    }
}

fn hit_x_for_width(width: usize) -> usize {
    if width <= 1 {
        return 0;
    }

    let base = width / HIT_X_DIVISOR;
    base.saturating_sub(HIT_X_LEFT_SHIFT).clamp(1, width - 1)
}

#[cfg(test)]
mod tests {
    use ratatui::{
        backend::TestBackend,
        buffer::Cell,
        style::{Color, Style},
        text::Line,
        widgets::Paragraph,
        Terminal,
    };

    use super::{
        hit_x_for_width, marker_base_style_for_hit_zone, paint_bar_lines,
        paint_hit_markers_overlay, paint_hit_zone_base, paint_note_blob, projected_span_x, to_x,
    };

    #[test]
    fn layering_keeps_marker_on_top_of_note_and_base() {
        let width = 12usize;
        let hit_x = 4usize;
        let base_style = Style::default().bg(Color::Blue);
        let note_style = Style::default().bg(Color::Red).fg(Color::White);
        let marker_style = Style::default().fg(Color::Yellow);

        let mut top = vec![ratatui::text::Span::styled(" ", Style::default()); width];
        let mut middle = vec![ratatui::text::Span::styled(" ", Style::default()); width];
        let mut bottom = vec![ratatui::text::Span::styled(" ", Style::default()); width];

        paint_hit_zone_base(&mut top, &mut middle, &mut bottom, hit_x, width, base_style);
        paint_note_blob(
            &mut top,
            &mut middle,
            &mut bottom,
            hit_x,
            width,
            note_style,
            "o",
        );
        paint_hit_markers_overlay(
            &mut top,
            &mut middle,
            &mut bottom,
            hit_x,
            base_style,
            marker_style,
        );

        let backend = TestBackend::new(width as u16, 3);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::from(top),
                        Line::from(middle),
                        Line::from(bottom),
                    ]),
                    frame.area(),
                );
            })
            .expect("draw");

        let buffer = terminal.backend().buffer();
        let top_marker_cell = cell(buffer, hit_x as u16, 0);
        assert_eq!(top_marker_cell.symbol(), "|");
        assert_eq!(top_marker_cell.style().fg, Some(Color::Yellow));
        assert_eq!(top_marker_cell.style().bg, Some(Color::Blue));

        let middle_marker_cell = cell(buffer, hit_x as u16, 1);
        assert_eq!(middle_marker_cell.symbol(), "◎");
        assert_eq!(middle_marker_cell.style().fg, Some(Color::Yellow));
        assert_eq!(middle_marker_cell.style().bg, Some(Color::Blue));

        let bottom_marker_cell = cell(buffer, hit_x as u16, 2);
        assert_eq!(bottom_marker_cell.symbol(), "|");
        assert_eq!(bottom_marker_cell.style().fg, Some(Color::Yellow));
        assert_eq!(bottom_marker_cell.style().bg, Some(Color::Blue));
    }

    #[test]
    fn marker_uses_roll_note_background_at_hit_zone() {
        let width = 12usize;
        let hit_x = 4usize;
        let base_style = Style::default().bg(Color::Blue);
        let roll_style = Style::default().bg(Color::Yellow);
        let marker_style = Style::default().fg(Color::Red);

        let mut top = vec![ratatui::text::Span::styled(" ", Style::default()); width];
        let mut middle = vec![ratatui::text::Span::styled(" ", Style::default()); width];
        let mut bottom = vec![ratatui::text::Span::styled(" ", Style::default()); width];

        paint_hit_zone_base(&mut top, &mut middle, &mut bottom, hit_x, width, base_style);
        middle[hit_x] = ratatui::text::Span::styled("=", roll_style);

        let marker_base_style = marker_base_style_for_hit_zone(&middle, hit_x, base_style);
        paint_hit_markers_overlay(
            &mut top,
            &mut middle,
            &mut bottom,
            hit_x,
            marker_base_style,
            marker_style,
        );

        let backend = TestBackend::new(width as u16, 3);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::from(top),
                        Line::from(middle),
                        Line::from(bottom),
                    ]),
                    frame.area(),
                );
            })
            .expect("draw");

        let buffer = terminal.backend().buffer();
        let top_marker_cell = cell(buffer, hit_x as u16, 0);
        assert_eq!(top_marker_cell.style().bg, Some(Color::Yellow));
        let middle_marker_cell = cell(buffer, hit_x as u16, 1);
        assert_eq!(middle_marker_cell.style().bg, Some(Color::Yellow));
        let bottom_marker_cell = cell(buffer, hit_x as u16, 2);
        assert_eq!(bottom_marker_cell.style().bg, Some(Color::Yellow));
    }

    #[test]
    fn hit_x_position_is_left_shifted_and_bounded() {
        assert_eq!(hit_x_for_width(1), 0);
        assert_eq!(hit_x_for_width(2), 1);
        assert_eq!(hit_x_for_width(12), 1);
        assert_eq!(hit_x_for_width(60), 9);
    }

    #[test]
    fn roll_span_is_clipped_to_right_edge_when_tail_is_out_of_range() {
        let width = 40;
        let hit_x = hit_x_for_width(width);
        let span = projected_span_x(200_000, 8_000_000, 0, width, hit_x, 1.0).expect("span");
        assert_eq!(span.1, width - 1);
    }

    #[test]
    fn roll_span_anchors_head_on_hit_zone_when_roll_is_already_active() {
        let width = 40;
        let hit_x = hit_x_for_width(width);
        let span = projected_span_x(-2_000_000, 200_000, 0, width, hit_x, 1.0).expect("span");
        assert_eq!(span.0, hit_x.saturating_sub(1));
        assert!(span.1 >= hit_x);
    }

    #[test]
    fn roll_span_is_none_when_note_is_fully_outside_viewport() {
        let width = 40;
        let hit_x = hit_x_for_width(width);
        let span = projected_span_x(4_000_000, 6_000_000, 0, width, hit_x, 1.0);
        assert!(span.is_none());
    }

    #[test]
    fn faster_scroll_speed_projects_note_farther_from_hit_zone() {
        let width = 40;
        let hit_x = hit_x_for_width(width);
        let delta = 500_000;
        let base = to_x(delta, width, hit_x, 1.0).expect("base projection");
        let fast = to_x(delta, width, hit_x, 2.0).expect("fast projection");
        assert!(fast > base);
    }

    #[test]
    fn negative_scroll_projects_future_notes_to_left_side() {
        let width = 40;
        let hit_x = hit_x_for_width(width);
        let future = to_x(500_000, width, hit_x, -1.0).expect("reverse projection");
        assert!(future < hit_x);
    }

    #[test]
    fn bar_lines_render_as_background_blocks_on_outer_rows() {
        let width = 40usize;
        let hit_x = hit_x_for_width(width);
        let bar_line_style = Style::default().bg(Color::DarkGray);
        let mut above = vec![ratatui::text::Span::styled(" ", Style::default()); width];
        let mut below = vec![ratatui::text::Span::styled(" ", Style::default()); width];
        let view = rhythm_mode_taiko::TaikoFrameView {
            now: 0,
            notes: Vec::new(),
            bar_lines: vec![rhythm_mode_taiko::TaikoFrameBarLine {
                tick: 1_000_000,
                scroll_scaled: 1_000_000,
            }],
            score: 0,
            combo: 0,
            gauge: 0.0,
        };

        paint_bar_lines(
            &mut above,
            &mut below,
            width,
            hit_x,
            &view,
            1.0,
            bar_line_style,
        );

        let x = to_x(1_000_000, width, hit_x, 1.0).expect("bar line x");
        assert_eq!(above[x].content.as_ref(), " ");
        assert_eq!(below[x].content.as_ref(), " ");
        assert_eq!(above[x].style, bar_line_style);
        assert_eq!(below[x].style, bar_line_style);
    }

    #[test]
    fn label_overrides_top_bar_line_when_sharing_same_row() {
        let width = 40usize;
        let hit_x = hit_x_for_width(width);
        let bar_line_style = Style::default().bg(Color::DarkGray);
        let label_style = Style::default().fg(Color::Yellow);
        let mut shared_top = vec![ratatui::text::Span::styled(" ", Style::default()); width];
        let mut below = vec![ratatui::text::Span::styled(" ", Style::default()); width];
        let view = rhythm_mode_taiko::TaikoFrameView {
            now: 0,
            notes: Vec::new(),
            bar_lines: vec![rhythm_mode_taiko::TaikoFrameBarLine {
                tick: 1_000_000,
                scroll_scaled: 1_000_000,
            }],
            score: 0,
            combo: 0,
            gauge: 0.0,
        };

        paint_bar_lines(
            &mut shared_top,
            &mut below,
            width,
            hit_x,
            &view,
            1.0,
            bar_line_style,
        );

        let x = to_x(1_000_000, width, hit_x, 1.0).expect("bar line x");
        shared_top[x] = ratatui::text::Span::styled("7", label_style);

        assert_eq!(shared_top[x].content.as_ref(), "7");
        assert_eq!(shared_top[x].style, label_style);
        assert_eq!(below[x].style, bar_line_style);
    }

    #[test]
    fn bar_line_scroll_multiplier_affects_projection() {
        let width = 40usize;
        let hit_x = hit_x_for_width(width);
        let base_style = Style::default().bg(Color::DarkGray);
        let mut above = vec![ratatui::text::Span::styled(" ", Style::default()); width];
        let mut below = vec![ratatui::text::Span::styled(" ", Style::default()); width];
        let view = rhythm_mode_taiko::TaikoFrameView {
            now: 0,
            notes: Vec::new(),
            bar_lines: vec![
                rhythm_mode_taiko::TaikoFrameBarLine {
                    tick: 500_000,
                    scroll_scaled: 1_000_000,
                },
                rhythm_mode_taiko::TaikoFrameBarLine {
                    tick: 500_000,
                    scroll_scaled: 2_000_000,
                },
            ],
            score: 0,
            combo: 0,
            gauge: 0.0,
        };

        paint_bar_lines(&mut above, &mut below, width, hit_x, &view, 1.0, base_style);

        let base_x = to_x(500_000, width, hit_x, 1.0).expect("base x");
        let fast_x = to_x(500_000, width, hit_x, 2.0).expect("fast x");
        assert!(fast_x > base_x);
        assert_eq!(above[base_x].style, base_style);
        assert_eq!(above[fast_x].style, base_style);
        assert_eq!(below[fast_x].style, base_style);
    }

    fn cell(buffer: &ratatui::buffer::Buffer, x: u16, y: u16) -> &Cell {
        buffer
            .cell((x, y))
            .expect("expected cell inside rendered buffer")
    }
}
