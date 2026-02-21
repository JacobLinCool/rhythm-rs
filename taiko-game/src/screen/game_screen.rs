use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use rhythm_mode_taiko::{TaikoDisplayKind, TaikoJudge, LANE_BOTH, LANE_DON, LANE_KAT};

use super::render_gauge_bar_line;
use crate::app::App;
use crate::tui::Frame;

const LOOKAHEAD_TICKS: i64 = 2_000_000;
const LOOKBACK_TICKS: i64 = 300_000;
const HIT_X_DIVISOR: usize = 6;
const HIT_X_LEFT_SHIFT: usize = 1;

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
                "Don keys/Space, Kat keys, Esc=back, Ctrl+C=quit",
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

    let view = &game.last_output.frame_view;
    let width = usize::from(area.width.saturating_sub(2).max(1));
    let hit_x = hit_x_for_width(width);
    let mut label = vec![Span::styled(" ", app.theme.text_primary); width];
    let mut top = vec![Span::styled(" ", app.theme.text_primary); width];
    let mut middle = vec![Span::styled(" ", app.theme.lane_track); width];
    let mut bottom = vec![Span::styled(" ", app.theme.text_primary); width];

    let base_style = judge_base_style(app, view.now);
    paint_hit_zone_base(&mut top, &mut middle, &mut bottom, hit_x, width, base_style);

    paint_notes(
        app,
        &mut label,
        &mut top,
        &mut middle,
        &mut bottom,
        width,
        hit_x,
        view,
    );

    let marker_style = input_marker_style(app, view.now);
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
    ])
    .block(themed_block_plain(app));
    frame.render_widget(lane, area);
}

fn paint_notes(
    app: &App,
    label: &mut [Span<'static>],
    top: &mut [Span<'static>],
    middle: &mut [Span<'static>],
    bottom: &mut [Span<'static>],
    width: usize,
    hit_x: usize,
    view: &rhythm_mode_taiko::TaikoFrameView,
) {
    let global_scroll_speed = app.effective_scroll_speed();
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
                    top,
                    middle,
                    bottom,
                    start_x,
                    width,
                    lane_style(app, note.lane),
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

                for cell in top.iter_mut().take(r.min(width - 1) + 1).skip(l) {
                    *cell = Span::styled(" ", app.theme.lane_note_roll);
                }
                for cell in middle.iter_mut().take(r.min(width - 1) + 1).skip(l) {
                    *cell = Span::styled("=", app.theme.lane_note_roll);
                }
                for cell in bottom.iter_mut().take(r.min(width - 1) + 1).skip(l) {
                    *cell = Span::styled(" ", app.theme.lane_note_roll);
                }

                if matches!(note.kind, TaikoDisplayKind::Balloon) {
                    label[l] = Span::styled(format!("{}", note.remaining_hits), app.theme.balloon);
                }
            }
        }
    }
}

pub(crate) fn projection_span_for_viewport_width(viewport_width: u16) -> usize {
    let width = usize::from(viewport_width.saturating_sub(2).max(1));
    let hit_x = hit_x_for_width(width);
    width.saturating_sub(hit_x + 1).max(1)
}

fn lane_style(app: &App, lane: u16) -> Style {
    match lane {
        LANE_DON => app.theme.lane_note_don,
        LANE_KAT => app.theme.lane_note_kat,
        LANE_BOTH => app.theme.lane_note_roll,
        _ => app.theme.value,
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

fn judge_base_style(app: &App, now: i64) -> Style {
    let Some(game) = app.game.as_ref() else {
        return app.theme.hit_zone_base_style();
    };

    match game.judge_flash {
        Some(flash) if now <= flash.until_tick => app.theme.judge_base_style(flash.judge),
        _ => app.theme.hit_zone_base_style(),
    }
}

fn input_marker_style(app: &App, now: i64) -> Style {
    let Some(game) = app.game.as_ref() else {
        return app.theme.value;
    };

    match game.input_flash {
        Some(flash) if now <= flash.until_tick => app.theme.marker_flash_style(flash.action),
        _ => app.theme.value,
    }
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}

fn themed_block_plain(app: &App) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
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
        hit_x_for_width, marker_base_style_for_hit_zone, paint_hit_markers_overlay,
        paint_hit_zone_base, paint_note_blob, projected_span_x, to_x,
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

    fn cell(buffer: &ratatui::buffer::Buffer, x: u16, y: u16) -> &Cell {
        buffer
            .cell((x, y))
            .expect("expected cell inside rendered buffer")
    }
}
