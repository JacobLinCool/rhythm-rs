use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::Terminal;
use rhythm_chart::{
    CanonicalChart, ChartMetadata, LaneOrRegion, Object, ObjectKind, TempoChange,
    TimeSignatureChange, TICKS_PER_SECOND,
};
use rhythm_importer_tja::TjaImporter;
use rhythm_mode_taiko::{ScheduledTaikoInput, TaikoBranchPolicy, TaikoRuntime};

use crate::app::{build_autoplay_inputs, compute_vsync_scroll_speed};
use crate::perf::PerfMeter;
use crate::screen::game_screen::{render_lane_view, LaneRenderOptions};
use crate::theme::Theme;

#[test]
#[ignore = "bench_smoke"]
fn bench_smoke_real_four_lane_gameplay_renderer() {
    let raw = include_bytes!("../samples/Nosferatu.tja");
    let importer = TjaImporter;
    let song = importer.import_song(raw).expect("import song");
    let course = song.courses.first().expect("course");

    let chart = &course.chart;
    let mut runtime = TaikoRuntime::new(
        chart,
        TaikoBranchPolicy::Disabled,
        course.branch_decisions.clone(),
    )
    .expect("runtime");

    let autoplay_inputs = build_autoplay_inputs(chart).expect("bounded autoplay inputs");
    let mut input_cursor = 0usize;

    let step_tick = (TICKS_PER_SECOND / 240).max(1);
    let frame_tick = (TICKS_PER_SECOND / 120).max(1);
    let end_tick = chart
        .objects
        .iter()
        .map(|object| object.end_tick)
        .max()
        .unwrap_or(0)
        + 2 * TICKS_PER_SECOND;

    let mut now = 0_i64;
    let mut next_frame = 0_i64;

    let mut perf = PerfMeter::default();
    let backend = TestBackend::new(160, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let theme = Theme::taiko_vivid(Theme::detect());

    while now <= end_tick {
        let start = input_cursor;
        while input_cursor < autoplay_inputs.len() && autoplay_inputs[input_cursor].tick <= now {
            input_cursor += 1;
        }
        let frame_inputs = autoplay_inputs[start..input_cursor]
            .iter()
            .copied()
            .map(ScheduledTaikoInput::unconditional)
            .collect::<Vec<_>>();

        let tick_start = Instant::now();
        let output = runtime.advance_to(now, &frame_inputs).expect("step");
        perf.record_tick(tick_start.elapsed());

        if now >= next_frame {
            let frame_start = Instant::now();
            terminal
                .draw(|frame| {
                    let size = frame.area();
                    let lanes = Layout::vertical([
                        Constraint::Ratio(1, 4),
                        Constraint::Ratio(1, 4),
                        Constraint::Ratio(1, 4),
                        Constraint::Ratio(1, 4),
                    ])
                    .split(size);
                    for lane in lanes.iter().copied() {
                        render_lane_view(
                            &theme,
                            frame,
                            lane,
                            &output.frame_view,
                            LaneRenderOptions {
                                scroll_speed: 1.0,
                                paused: false,
                                paused_label: "PAUSED",
                                judge_flash: None,
                                input_flash: None,
                            },
                        );
                    }
                })
                .expect("draw");
            perf.record_frame(frame_start.elapsed());
            next_frame += frame_tick;
        }

        if output.finished {
            break;
        }

        now += step_tick;
    }

    let snapshot = perf.snapshot();
    eprintln!(
        "bench_smoke_real_4p: tick_avg={:.3}ms tick_p95={:.3}ms frame_avg={:.3}ms frame_p95={:.3}ms tps={:.1} fps={:.1}",
        snapshot.tick.avg_ms,
        snapshot.tick.p95_ms,
        snapshot.frame.avg_ms,
        snapshot.frame.p95_ms,
        snapshot.tps,
        snapshot.fps
    );

    assert!(
        snapshot.tick.p95_ms < 2.0,
        "tick p95 too high: {:.3}ms",
        snapshot.tick.p95_ms
    );
    assert!(
        snapshot.frame.p95_ms < 8.3,
        "frame p95 too high: {:.3}ms",
        snapshot.frame.p95_ms
    );
    assert!(
        snapshot.tps >= 500.0,
        "TPS capacity too low: {:.1}",
        snapshot.tps
    );
    assert!(snapshot.fps >= 120.0, "FPS too low: {:.1}", snapshot.fps);
}

#[test]
#[ignore = "bench_smoke"]
fn bench_smoke_vsync_speed_computation_under_5ms() {
    let mut objects = Vec::with_capacity(100_000);
    let mut tick = 0_i64;
    for id in 0..100_000_u32 {
        let span = if id % 7 == 0 { 125_000 } else { 62_500 };
        objects.push(Object {
            id,
            kind: if id % 11 == 0 {
                ObjectKind::Roll
            } else {
                ObjectKind::Tap
            },
            start_tick: tick,
            end_tick: tick + span,
            lane_or_region: LaneOrRegion::None,
            flags: 0,
            required_hits: 0,
            slide_to: None,
            scroll_scaled: 1_000_000,
            branch_segment_id: None,
            branch_route_id: 0,
        });
        tick += span;
    }

    let chart = CanonicalChart {
        metadata: ChartMetadata::default(),
        tempo_map: vec![
            TempoChange {
                tick: 0,
                micros_per_quarter: 500_000,
            },
            TempoChange {
                tick: 8_000_000,
                micros_per_quarter: 428_571,
            },
            TempoChange {
                tick: 14_000_000,
                micros_per_quarter: 333_333,
            },
        ],
        signatures: vec![
            TimeSignatureChange {
                tick: 0,
                numerator: 4,
                denominator: 4,
            },
            TimeSignatureChange {
                tick: 10_000_000,
                numerator: 3,
                denominator: 4,
            },
            TimeSignatureChange {
                tick: 18_000_000,
                numerator: 7,
                denominator: 8,
            },
        ],
        lanes: Vec::new(),
        branch_segments: Vec::new(),
        objects,
        events: Vec::new(),
    };

    let start = Instant::now();
    let speed = compute_vsync_scroll_speed(&chart, 120);
    let elapsed = start.elapsed();
    eprintln!(
        "bench_smoke_vsync: speed={speed:.4} elapsed={:.3}ms",
        elapsed.as_secs_f64() * 1000.0
    );

    assert!(speed >= 1.0);
    assert!(speed <= 2.0);
    assert!(
        elapsed <= Duration::from_millis(5),
        "V-Sync speed computation exceeded budget: {elapsed:?}"
    );
}
