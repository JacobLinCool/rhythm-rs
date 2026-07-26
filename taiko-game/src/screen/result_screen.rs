use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use rhythm_mode_taiko::{
    TaikoFinalResult, TaikoJudgeKind, GREAT_WINDOW_TICKS, MISS_WINDOW_TICKS, OK_WINDOW_TICKS,
};

use super::render_gauge_bar_line;
use crate::app::{App, ResultState, TimingSample};
use crate::localization::{display_width, truncate_to_width, Localizer, UiMessage, UiText};
use crate::theme::PerfMetricKind;
use crate::tui::Frame;

const TIMING_VIOLIN_HALF_HEIGHT: usize = 3;
const TIMING_PLOT_MIN_WIDTH: usize = 24;
const TIMING_PLOT_RESERVED_COLUMNS: usize = 2;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(result) = app.result.as_ref() else {
        let empty = Paragraph::new(Span::styled(app.text(UiText::NoResult), app.theme.error))
            .block(themed_block(app, app.text(UiText::Result)));
        frame.render_widget(empty, area);
        return;
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if app.result_details_visible {
            [
                Constraint::Length(9),
                Constraint::Length(5),
                Constraint::Min(7),
                Constraint::Length(7),
            ]
        } else {
            [
                Constraint::Length(9),
                Constraint::Length(5),
                Constraint::Min(7),
                Constraint::Length(3),
            ]
        })
        .split(area);

    let final_result = &result.final_result;
    let accuracy = result_accuracy(final_result);
    let full_combo =
        final_result.miss == 0 && final_result.great.saturating_add(final_result.ok) > 0;
    let (early, late, centered) = early_late_counts(&result.timing_samples);
    let status_style = if final_result.passed {
        app.theme.success
    } else {
        app.theme.error
    };
    let combo_label = if full_combo {
        format!("  •  {}", app.text(UiText::FullCombo))
    } else {
        String::new()
    };
    let summary = Paragraph::new(vec![
        kv_line(
            app,
            app.text(UiText::Song),
            format!("{} {}", result.title, result.subtitle),
        ),
        kv_line(
            app,
            app.text(UiText::SelectedCourse),
            result.course_name.clone(),
        ),
        Line::from(vec![
            Span::styled(
                app.text(if final_result.passed {
                    UiText::Clear
                } else {
                    UiText::Fail
                }),
                status_style,
            ),
            Span::styled(combo_label, app.theme.warning),
            Span::styled(
                format!("  •  {} ", app.text(UiText::Grade)),
                app.theme.label,
            ),
            Span::styled(result_grade(accuracy), app.theme.selection),
        ]),
        Line::from(vec![
            Span::styled(format!("{} ", app.text(UiText::Score)), app.theme.label),
            Span::styled(final_result.score.to_string(), app.theme.value),
            Span::styled(
                format!("  {}", personal_best_delta(app, result)),
                app.theme.warning,
            ),
        ]),
        Line::from(vec![
            Span::styled(format!("{} ", app.text(UiText::Accuracy)), app.theme.label),
            Span::styled(format!("{accuracy:.2}%"), app.theme.value),
            Span::styled(
                format!("  •  {} ", app.text(UiText::MaxCombo)),
                app.theme.label,
            ),
            Span::styled(final_result.max_combo.to_string(), app.theme.value),
        ]),
        render_gauge_bar_line(
            app,
            final_result.gauge,
            final_result.pass_threshold,
            layout[0].width.saturating_sub(2),
            true,
        ),
    ])
    .block(themed_block(app, app.text(UiText::ResultSummary)))
    .wrap(Wrap { trim: true });
    frame.render_widget(summary, layout[0]);

    let judge = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(format!("{} ", app.text(UiText::Great)), app.theme.label),
            Span::styled(
                final_result.great.to_string(),
                app.theme
                    .judge_style(rhythm_mode_taiko::TaikoJudge::Great { delta_tick: 0 }),
            ),
            Span::styled(format!("  {} ", app.text(UiText::Ok)), app.theme.label),
            Span::styled(
                final_result.ok.to_string(),
                app.theme
                    .judge_style(rhythm_mode_taiko::TaikoJudge::Ok { delta_tick: 0 }),
            ),
            Span::styled(format!("  {} ", app.text(UiText::Miss)), app.theme.label),
            Span::styled(
                final_result.miss.to_string(),
                app.theme
                    .judge_style(rhythm_mode_taiko::TaikoJudge::Miss { delta_tick: 0 }),
            ),
        ]),
        Line::from(vec![
            Span::styled(format!("{} ", app.text(UiText::Early)), app.theme.label),
            Span::styled(early.to_string(), app.theme.value),
            Span::styled(format!("  {} ", app.text(UiText::Late)), app.theme.label),
            Span::styled(late.to_string(), app.theme.value),
            Span::styled(
                format!("  {} ", app.text(UiText::Centered)),
                app.theme.label,
            ),
            Span::styled(centered.to_string(), app.theme.value),
            Span::styled(
                format!("  {} ", app.text(UiText::RollHits)),
                app.theme.label,
            ),
            Span::styled(final_result.roll_hits.to_string(), app.theme.value),
        ]),
    ])
    .block(themed_block(app, app.text(UiText::Judgement)))
    .wrap(Wrap { trim: true });
    frame.render_widget(judge, layout[1]);

    frame.render_widget(
        Paragraph::new(timing_lines(app, result, layout[2].width))
            .block(themed_block(app, app.text(UiText::TimingDistribution)))
            .wrap(Wrap { trim: false }),
        layout[2],
    );

    if app.result_details_visible {
        let perf = &result.perf;
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(format!("{} ", app.text(UiText::Replay)), app.theme.label),
                    Span::styled(format!("{:016x}", result.replay_hash), app.theme.metadata),
                    Span::styled(
                        format!("  {} ", app.text(UiText::BranchControls)),
                        app.theme.label,
                    ),
                    Span::styled(result.branch_controls.to_string(), app.theme.metadata),
                ]),
                perf_line(
                    app,
                    "Input dispatch",
                    perf.input_dispatch.p95_ms,
                    perf.input_dispatch.p99_ms,
                    perf.input_dispatch.max_ms,
                    PerfMetricKind::TickP95Ms,
                ),
                perf_line(
                    app,
                    "Logic tick",
                    perf.tick.p95_ms,
                    perf.tick.p99_ms,
                    perf.tick.max_ms,
                    PerfMetricKind::TickP95Ms,
                ),
                perf_line(
                    app,
                    "Frame",
                    perf.frame.p95_ms,
                    perf.frame.p99_ms,
                    perf.frame.max_ms,
                    PerfMetricKind::FrameP95Ms,
                ),
                Line::from(vec![
                    Span::styled("Throughput: ", app.theme.label),
                    Span::styled(
                        format!("{:.1} ticks/s", perf.tps),
                        app.theme.perf_style(PerfMetricKind::Tps, perf.tps),
                    ),
                    Span::styled("  ", app.theme.metadata),
                    Span::styled(
                        format!("{:.1} frames/s", perf.fps),
                        app.theme.perf_style(PerfMetricKind::Fps, perf.fps),
                    ),
                ]),
                Line::from(Span::styled(
                    app.text(UiText::ResultHideDetails),
                    app.theme.metadata,
                )),
            ])
            .block(themed_block(app, app.text(UiText::Details)))
            .wrap(Wrap { trim: true }),
            layout[3],
        );
    } else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                app.text(UiText::ResultShowDetails),
                app.theme.metadata,
            ))
            .block(themed_block(app, app.text(UiText::Next))),
            layout[3],
        );
    }
}

fn result_accuracy(result: &TaikoFinalResult) -> f64 {
    let total = result
        .great
        .saturating_add(result.ok)
        .saturating_add(result.miss);
    if total == 0 {
        return 0.0;
    }
    (f64::from(result.great) + f64::from(result.ok) * 0.5) * 100.0 / f64::from(total)
}

fn result_grade(accuracy: f64) -> &'static str {
    if accuracy >= 95.0 {
        "S"
    } else if accuracy >= 90.0 {
        "A"
    } else if accuracy >= 80.0 {
        "B"
    } else if accuracy >= 70.0 {
        "C"
    } else {
        "D"
    }
}

fn early_late_counts(samples: &[TimingSample]) -> (usize, usize, usize) {
    samples.iter().fold((0, 0, 0), |mut counts, sample| {
        if sample.delta_tick < 0 {
            counts.0 += 1;
        } else if sample.delta_tick > 0 {
            counts.1 += 1;
        } else {
            counts.2 += 1;
        }
        counts
    })
}

fn personal_best_delta(app: &App, result: &ResultState) -> String {
    match result.previous_best_score {
        None => app.text(UiText::NewPersonalBest).to_owned(),
        Some(previous) if result.final_result.score > previous => format!(
            "{}  +{}",
            app.text(UiText::NewPersonalBest),
            result.final_result.score - previous
        ),
        Some(previous) if result.final_result.score == previous => {
            app.text(UiText::PersonalBestMatched).to_owned()
        }
        Some(previous) => format!(
            "{}  -{}",
            app.text(UiText::PersonalBestDelta),
            previous.saturating_sub(result.final_result.score)
        ),
    }
}

fn perf_line(
    app: &App,
    label: &str,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
    metric: PerfMetricKind,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), app.theme.label),
        Span::styled(
            format!("p95 {p95_ms:.3} ms"),
            app.theme.perf_style(metric, p95_ms),
        ),
        Span::styled(
            format!("  p99 {p99_ms:.3} ms  max {max_ms:.3} ms"),
            app.theme.metadata,
        ),
    ])
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
            Span::styled(
                app.text(UiText::NoTapTimingSamples),
                app.theme.text_secondary,
            ),
            Span::styled(
                format!(" {}", app.text(UiText::RollExpiredMissExcluded)),
                app.theme.metadata,
            ),
        ])];
    }

    let stats = timing_stats(&samples);
    let plot_width = timing_plot_width(area_width);
    let rows = timing_violin_rows(plot_width, &samples);
    let range_ms = tick_to_ms(MISS_WINDOW_TICKS);

    let mut lines = Vec::with_capacity(rows.len() + 3);
    let stats_str = app.localizer().message(UiMessage::TimingStats {
        count: stats.count,
        average_ms: tick_to_ms(stats.avg_tick),
        median_ms: tick_to_ms(stats.median_tick),
        p90_absolute_ms: tick_to_ms(stats.p90_abs_tick),
    });
    lines.push(Line::from(vec![Span::styled(
        fit_to_display_width(&stats_str, plot_width),
        app.theme.value,
    )]));
    lines.push(Line::from(vec![Span::styled(
        build_axis_label(app.localizer(), plot_width, range_ms),
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

fn fit_to_display_width(value: &str, width: usize) -> String {
    let mut fitted = truncate_to_width(value, width);
    fitted.extend(std::iter::repeat_n(
        ' ',
        width.saturating_sub(display_width(&fitted)),
    ));
    fitted
}

fn build_axis_label(localizer: Localizer, width: usize, range_ms: f64) -> String {
    let left_label = localizer.message(UiMessage::TimingAxisEarly { range_ms });
    let center_label = truncate_to_width(localizer.text(UiText::TimingZero), width);
    let right_label = localizer.message(UiMessage::TimingAxisLate { range_ms });
    let center_width = display_width(&center_label);
    let center_start = timing_zero_index(width)
        .saturating_sub(center_width / 2)
        .min(width.saturating_sub(center_width));
    let right_width = width.saturating_sub(center_start + center_width);

    let left = fit_to_display_width(&left_label, center_start);
    let right = truncate_to_width(&right_label, right_width);
    let right_padding = right_width.saturating_sub(display_width(&right));

    format!("{left}{center_label}{}{right}", " ".repeat(right_padding))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::UiLanguage;

    #[test]
    fn localized_axis_labels_have_exact_expected_layout() {
        let cases = [
            (
                UiLanguage::English,
                "Early <-100.0ms   0ms      +100.0ms Late",
            ),
            (
                UiLanguage::TraditionalChinese,
                "偏早 <-100.0ms    0ms      +100.0ms 偏晚",
            ),
            (
                UiLanguage::Japanese,
                "早い <-100.0ms    0ms      +100.0ms 遅い",
            ),
        ];

        for (language, expected) in cases {
            let rendered = build_axis_label(Localizer::new(language), 40, 100.0);
            assert_eq!(rendered, expected, "language={language:?}");
            assert_eq!(display_width(&rendered), 40, "language={language:?}");
        }
    }

    #[test]
    fn localized_axis_labels_never_overflow_or_split_graphemes() {
        for language in UiLanguage::ALL {
            for width in [0, 1, 2, 3, 8, 24, 31, 80] {
                let rendered = build_axis_label(Localizer::new(language), width, 200.0);
                assert_eq!(
                    display_width(&rendered),
                    width,
                    "language={language:?}, width={width}, rendered={rendered:?}"
                );
                assert!(!rendered.contains('\u{fffd}'));
            }
        }
    }

    #[test]
    fn localized_timing_stats_fit_exact_display_width() {
        for language in UiLanguage::ALL {
            let localizer = Localizer::new(language);
            let stats = localizer.message(UiMessage::TimingStats {
                count: 123,
                average_ms: -1.25,
                median_ms: 0.5,
                p90_absolute_ms: 4.75,
            });

            for width in [1, 24, 40, 80] {
                let rendered = fit_to_display_width(&stats, width);
                assert_eq!(
                    display_width(&rendered),
                    width,
                    "language={language:?}, width={width}, rendered={rendered:?}"
                );
            }
        }
    }
}
