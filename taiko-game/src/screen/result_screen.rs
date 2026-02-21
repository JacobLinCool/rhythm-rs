use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use rhythm_mode_taiko::{TaikoJudgeKind, GREAT_WINDOW_TICKS, MISS_WINDOW_TICKS, OK_WINDOW_TICKS};

use super::render_gauge_bar_line;
use crate::app::{App, ResultState, TimingSample};
use crate::theme::PerfMetricKind;
use crate::tui::Frame;

const TIMING_VIOLIN_HALF_HEIGHT: usize = 3;
const TIMING_PLOT_MIN_WIDTH: usize = 24;
const TIMING_PLOT_RESERVED_COLUMNS: usize = 2;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(result) = app.result.as_ref() else {
        let empty = Paragraph::new(Span::styled("No result", app.theme.error))
            .block(themed_block(app, "Result"));
        frame.render_widget(empty, area);
        return;
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Min(3),
        ])
        .split(area);

    let pass_fail_style = if result.final_result.passed {
        app.theme.warning
    } else {
        app.theme.error
    };

    let summary = Paragraph::new(vec![
        kv_line(app, "Song", format!("{} {}", result.title, result.subtitle)),
        kv_line(app, "Course", result.course_name.clone()),
        kv_line(app, "Score", result.final_result.score.to_string()),
        kv_line(app, "Max Combo", result.final_result.max_combo.to_string()),
        render_gauge_bar_line(
            app,
            result.final_result.gauge,
            result.final_result.pass_threshold,
            layout[0].width.saturating_sub(2),
            true,
        ),
        Line::from(vec![
            Span::styled("Result: ", app.theme.label),
            Span::styled(
                if result.final_result.passed {
                    "PASS"
                } else {
                    "FAIL"
                },
                pass_fail_style,
            ),
        ]),
        Line::from(vec![
            Span::styled("Replay Hash: ", app.theme.label),
            Span::styled(format!("{:016x}", result.replay_hash), app.theme.metadata),
        ]),
    ])
    .block(themed_block(app, "Result Summary"))
    .wrap(Wrap { trim: true });
    frame.render_widget(summary, layout[0]);

    let judge = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("GREAT: ", app.theme.label),
            Span::styled(
                result.final_result.great.to_string(),
                app.theme
                    .judge_style(rhythm_mode_taiko::TaikoJudge::Great { delta_tick: 0 }),
            ),
            Span::styled(" | OK: ", app.theme.label),
            Span::styled(
                result.final_result.ok.to_string(),
                app.theme
                    .judge_style(rhythm_mode_taiko::TaikoJudge::Ok { delta_tick: 0 }),
            ),
            Span::styled(" | MISS: ", app.theme.label),
            Span::styled(
                result.final_result.miss.to_string(),
                app.theme
                    .judge_style(rhythm_mode_taiko::TaikoJudge::Miss { delta_tick: 0 }),
            ),
        ]),
        Line::from(vec![
            Span::styled("Roll Hits: ", app.theme.label),
            Span::styled(
                result.final_result.roll_hits.to_string(),
                app.theme
                    .judge_style(rhythm_mode_taiko::TaikoJudge::RollHit),
            ),
        ]),
        Line::from(vec![
            Span::styled("Branch Controls: ", app.theme.label),
            Span::styled(result.branch_controls.to_string(), app.theme.route_current),
        ]),
    ])
    .block(themed_block(app, "Judge Stats"))
    .wrap(Wrap { trim: true });
    frame.render_widget(judge, layout[1]);

    let timing = Paragraph::new(timing_lines(app, result, layout[2].width))
        .block(themed_block(app, "Timing Distribution"))
        .wrap(Wrap { trim: false });
    frame.render_widget(timing, layout[2]);

    let perf = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Tick avg ", app.theme.label),
            Span::styled(
                format!("{:.3} ms", result.perf.tick.avg_ms),
                app.theme.value,
            ),
            Span::styled(" | Tick p95 ", app.theme.label),
            Span::styled(
                format!("{:.3} ms", result.perf.tick.p95_ms),
                app.theme
                    .perf_style(PerfMetricKind::TickP95Ms, result.perf.tick.p95_ms),
            ),
            Span::styled(" | TPS ", app.theme.label),
            Span::styled(
                format!("{:.1}", result.perf.tps),
                app.theme.perf_style(PerfMetricKind::Tps, result.perf.tps),
            ),
        ]),
        Line::from(vec![
            Span::styled("Frame avg ", app.theme.label),
            Span::styled(
                format!("{:.3} ms", result.perf.frame.avg_ms),
                app.theme.value,
            ),
            Span::styled(" | Frame p95 ", app.theme.label),
            Span::styled(
                format!("{:.3} ms", result.perf.frame.p95_ms),
                app.theme
                    .perf_style(PerfMetricKind::FrameP95Ms, result.perf.frame.p95_ms),
            ),
            Span::styled(" | FPS ", app.theme.label),
            Span::styled(
                format!("{:.1}", result.perf.fps),
                app.theme.perf_style(PerfMetricKind::Fps, result.perf.fps),
            ),
        ]),
        Line::from(Span::styled(
            "Press Enter/Don/Esc to go back to Song Menu",
            app.theme.metadata,
        )),
    ])
    .block(themed_block(app, "Performance"))
    .wrap(Wrap { trim: true });
    frame.render_widget(perf, layout[3]);
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}

fn kv_line(app: &App, label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), app.theme.label),
        Span::styled(value, app.theme.value),
    ])
}

fn timing_lines(app: &App, result: &ResultState, area_width: u16) -> Vec<Line<'static>> {
    let samples = result
        .timing_samples
        .iter()
        .filter(|sample| {
            matches!(
                sample.judge,
                TaikoJudgeKind::Great | TaikoJudgeKind::Ok | TaikoJudgeKind::Miss
            )
        })
        .copied()
        .collect::<Vec<_>>();

    if samples.is_empty() {
        return vec![Line::from(vec![
            Span::styled("No tap timing samples.", app.theme.text_secondary),
            Span::styled(" (roll/expired miss are excluded)", app.theme.metadata),
        ])];
    }

    let stats = timing_stats(&samples);
    let plot_width = timing_plot_width(area_width);
    let rows = timing_violin_rows(plot_width, &samples);
    let range_ms = tick_to_ms(MISS_WINDOW_TICKS);

    let mut lines = Vec::with_capacity(rows.len() + 3);
    let stats_str = format!(
        "Samples={}  Avg={:+.2}ms  Median={:+.2}ms  P90|delta|={:.2}ms",
        stats.count,
        tick_to_ms(stats.avg_tick),
        tick_to_ms(stats.median_tick),
        tick_to_ms(stats.p90_abs_tick)
    );
    lines.push(Line::from(vec![Span::styled(
        format!("{:<width$}", stats_str, width = plot_width),
        app.theme.value,
    )]));
    lines.push(Line::from(vec![Span::styled(
        build_axis_label(plot_width, range_ms),
        app.theme.metadata,
    )]));

    let ok_style = app
        .theme
        .judge_style(rhythm_mode_taiko::TaikoJudge::Ok { delta_tick: 0 });
    let great_style = app
        .theme
        .judge_style(rhythm_mode_taiko::TaikoJudge::Great { delta_tick: 0 });
    let zone_boundaries = timing_zone_boundaries(plot_width);

    for row in rows {
        lines.push(colorize_violin_row(
            &row,
            &zone_boundaries,
            great_style,
            ok_style,
            app.theme.value,
        ));
    }

    lines
}

fn timing_plot_width(area_width: u16) -> usize {
    let dynamic = usize::from(area_width).saturating_sub(TIMING_PLOT_RESERVED_COLUMNS);
    dynamic.max(TIMING_PLOT_MIN_WIDTH)
}

fn timing_zero_index(width: usize) -> usize {
    if width <= 1 {
        0
    } else {
        (width - 1) / 2
    }
}

fn timing_violin_rows(width: usize, samples: &[TimingSample]) -> Vec<String> {
    let bins = timing_bins(width, samples);
    let max_count = bins
        .iter()
        .copied()
        .fold(0.0_f64, |acc, value| acc.max(value))
        .max(1.0);
    let height = TIMING_VIOLIN_HALF_HEIGHT;
    let zero_idx = timing_zero_index(width);
    let mut out = Vec::with_capacity(height + 1);

    // Only render top half + center axis (no mirrored bottom half)
    for row in 0..=height {
        let mut chars = vec![' '; width];
        let dist = height - row;
        for (x, count) in bins.iter().copied().enumerate() {
            let level = ((count / max_count) * (height as f64)).round() as usize;
            let fill = level > 0 && level >= dist;
            chars[x] = if row == height {
                if fill {
                    '='
                } else {
                    '-'
                }
            } else if fill {
                '#'
            } else {
                ' '
            };
        }
        if zero_idx < chars.len() {
            chars[zero_idx] = if row == height { '+' } else { '|' };
        }
        out.push(chars.into_iter().collect::<String>());
    }

    out
}

fn timing_bins(width: usize, samples: &[TimingSample]) -> Vec<f64> {
    let mut bins = vec![0.0_f64; width.max(1)];
    let range = MISS_WINDOW_TICKS.max(1);
    let span = (2 * range) as f64;
    let max_idx = bins.len().saturating_sub(1) as f64;

    for sample in samples {
        let clamped = sample.delta_tick.clamp(-range, range) as f64;
        let ratio = (clamped + range as f64) / span;
        let idx = (ratio * max_idx).round() as usize;
        bins[idx] += 1.0;
    }

    smooth_bins(&bins)
}

fn smooth_bins(bins: &[f64]) -> Vec<f64> {
    if bins.len() <= 2 {
        return bins.to_vec();
    }

    let radius = (bins.len() / 40).clamp(1, 4);
    let mut out = vec![0.0_f64; bins.len()];
    for (idx, value) in out.iter_mut().enumerate() {
        let left = idx.saturating_sub(radius);
        let right = (idx + radius).min(bins.len() - 1);
        let mut weighted_sum = 0.0_f64;
        let mut weight_total = 0.0_f64;
        for (other, sample) in bins.iter().enumerate().take(right + 1).skip(left) {
            let distance = idx.abs_diff(other);
            let weight = (radius + 1 - distance) as f64;
            weighted_sum += sample * weight;
            weight_total += weight;
        }
        *value = if weight_total > 0.0 {
            weighted_sum / weight_total
        } else {
            0.0
        };
    }
    out
}

struct TimingStats {
    count: usize,
    avg_tick: i64,
    median_tick: i64,
    p90_abs_tick: i64,
}

fn timing_stats(samples: &[TimingSample]) -> TimingStats {
    let count = samples.len();
    let mut deltas = samples.iter().map(|s| s.delta_tick).collect::<Vec<_>>();
    deltas.sort_unstable();
    let sum = deltas.iter().copied().sum::<i64>();
    let avg_tick = (sum as f64 / count as f64).round() as i64;
    let median_tick = if count.is_multiple_of(2) {
        let hi = deltas[count / 2];
        let lo = deltas[count / 2 - 1];
        ((hi + lo) as f64 / 2.0).round() as i64
    } else {
        deltas[count / 2]
    };

    let mut abs = deltas.iter().map(|v| v.abs()).collect::<Vec<_>>();
    abs.sort_unstable();
    let p90_idx = ((count as f64 * 0.9).ceil() as usize)
        .saturating_sub(1)
        .min(count.saturating_sub(1));

    TimingStats {
        count,
        avg_tick,
        median_tick,
        p90_abs_tick: abs[p90_idx],
    }
}

fn tick_to_ms(tick: i64) -> f64 {
    tick as f64 / 1_000.0
}

fn build_axis_label(width: usize, range_ms: f64) -> String {
    let left_label = format!("Early <-{range_ms:.1}ms");
    let center_label = "0ms";
    let right_label = format!("+{range_ms:.1}ms Late");
    let zero_idx = timing_zero_index(width);

    let mut buf = vec![' '; width];

    // Left label at position 0
    for (i, c) in left_label.chars().enumerate() {
        if i < width {
            buf[i] = c;
        }
    }

    // Center label around zero_idx
    let center_start = zero_idx.saturating_sub(center_label.len() / 2);
    for (i, c) in center_label.chars().enumerate() {
        let pos = center_start + i;
        if pos < width {
            buf[pos] = c;
        }
    }

    // Right label right-aligned
    let right_start = width.saturating_sub(right_label.len());
    for (i, c) in right_label.chars().enumerate() {
        let pos = right_start + i;
        if pos < width {
            buf[pos] = c;
        }
    }

    buf.into_iter().collect()
}

/// Bin index boundaries for the GREAT/OK zones around center.
struct ZoneBoundaries {
    great_start: usize,
    great_end: usize,
    ok_left_start: usize,
    ok_left_end: usize,
    ok_right_start: usize,
    ok_right_end: usize,
}

fn timing_zone_boundaries(width: usize) -> ZoneBoundaries {
    let range = MISS_WINDOW_TICKS.max(1) as f64;
    let span = 2.0 * range;
    let max_idx = width.saturating_sub(1) as f64;
    let tick_to_idx =
        |tick: i64| -> usize { ((tick as f64 + range) / span * max_idx).round() as usize };
    ZoneBoundaries {
        great_start: tick_to_idx(-GREAT_WINDOW_TICKS),
        great_end: tick_to_idx(GREAT_WINDOW_TICKS),
        ok_left_start: tick_to_idx(-OK_WINDOW_TICKS),
        ok_left_end: tick_to_idx(-GREAT_WINDOW_TICKS),
        ok_right_start: tick_to_idx(GREAT_WINDOW_TICKS),
        ok_right_end: tick_to_idx(OK_WINDOW_TICKS),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimingZone {
    Miss,
    Ok,
    Great,
}

fn timing_zone_at(zones: &ZoneBoundaries, x: usize) -> TimingZone {
    if x >= zones.great_start && x <= zones.great_end {
        TimingZone::Great
    } else if (x >= zones.ok_left_start && x <= zones.ok_left_end)
        || (x >= zones.ok_right_start && x <= zones.ok_right_end)
    {
        TimingZone::Ok
    } else {
        TimingZone::Miss
    }
}

fn colorize_violin_row(
    row: &str,
    zones: &ZoneBoundaries,
    great_style: Style,
    ok_style: Style,
    default_style: Style,
) -> Line<'static> {
    let chars: Vec<char> = row.chars().collect();
    if chars.is_empty() {
        return Line::from(Span::styled(String::new(), default_style));
    }

    let style_for_zone = |zone: TimingZone| -> Style {
        match zone {
            TimingZone::Great => great_style,
            TimingZone::Ok => ok_style,
            TimingZone::Miss => default_style,
        }
    };

    let mut spans = Vec::new();
    let mut seg_start = 0;
    let mut seg_zone = timing_zone_at(zones, 0);

    for (i, _) in chars.iter().enumerate().skip(1) {
        let zone = timing_zone_at(zones, i);
        if zone != seg_zone {
            let segment: String = chars[seg_start..i].iter().collect();
            spans.push(Span::styled(segment, style_for_zone(seg_zone)));
            seg_start = i;
            seg_zone = zone;
        }
    }
    let segment: String = chars[seg_start..].iter().collect();
    spans.push(Span::styled(segment, style_for_zone(seg_zone)));

    Line::from(spans)
}
