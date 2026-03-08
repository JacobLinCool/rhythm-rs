use encoding_rs::SHIFT_JIS;
use rhythm_chart::{BranchDecisionHint, ChartImporter, ImportError, TICKS_PER_SECOND};
use rhythm_importer_tja::{build_branch_decision_table, TjaImporter};

const NOSFERATU_TJA: &[u8] = include_bytes!("../../taiko-game/samples/Nosferatu.tja");

#[test]
fn deterministic_import_is_stable() {
    let importer = TjaImporter;

    let a = importer.import_all(NOSFERATU_TJA).expect("import a");
    let b = importer.import_all(NOSFERATU_TJA).expect("import b");

    assert_eq!(a, b);
    assert!(!a.is_empty());
    assert!(!a[0].objects.is_empty());

    let json_a = serde_json::to_string(&a[0]).expect("serialize a");
    let json_b = serde_json::to_string(&b[0]).expect("serialize b");
    assert_eq!(json_a, json_b);
}

#[test]
fn import_song_exposes_course_context() {
    let importer = TjaImporter;
    let song = importer.import_song(NOSFERATU_TJA).expect("import song");

    assert!(!song.courses.is_empty());
    assert!(!song.title.is_empty());
    assert_eq!(
        song.courses[0].branch_decisions,
        build_branch_decision_table(&song.courses[0].chart)
    );
}

#[test]
fn import_song_sorts_courses_by_difficulty_order() {
    let raw = r#"
TITLE:Course Sort Demo
BPM:120
COURSE:Oni
LEVEL:4
#START
1000,
#END
COURSE:Easy
LEVEL:9
#START
1000,
#END
"#;

    let importer = TjaImporter;
    let song = importer.import_song(raw.as_bytes()).expect("import song");

    assert_eq!(song.courses.len(), 2);
    assert_eq!(
        song.courses[0].chart.metadata.difficulty_name.as_deref(),
        Some("Easy")
    );
    assert_eq!(
        song.courses[1].chart.metadata.difficulty_name.as_deref(),
        Some("Oni")
    );
}

#[test]
fn branch_routes_map_to_one_segment() {
    let raw = r#"
TITLE:Branch Demo
BPM:120
COURSE:Oni
LEVEL:9
#START
#BRANCHSTART p,70,85
#N
1000,
#E
0100,
#M
0010,
#BRANCHEND
#END
"#;

    let importer = TjaImporter;
    let chart = importer.import(raw.as_bytes()).expect("import");

    assert_eq!(chart.branch_segments.len(), 1);
    assert_eq!(chart.branch_segments[0].route_count, 3);
    assert_eq!(chart.objects.len(), 3);

    for route_id in 0_u8..=2 {
        assert!(chart
            .objects
            .iter()
            .any(|o| o.branch_segment_id == Some(1) && o.branch_route_id == route_id));
    }

    let points = build_branch_decision_table(&chart);
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].segment_id, 1);
}

#[test]
fn gogo_conflict_across_branch_routes_is_allowed() {
    let raw = r#"
TITLE:Branch Gogo Conflict
BPM:120
COURSE:Oni
LEVEL:9
#START
#BRANCHSTART p,70,85
#N
#GOGOSTART
1000,
#E
#GOGOEND
0100,
#M
#GOGOEND
0010,
#BRANCHEND
#END
"#;

    let importer = TjaImporter;
    let chart = importer.import(raw.as_bytes()).expect("import");

    assert_eq!(chart.branch_segments.len(), 1);
    assert_eq!(chart.objects.len(), 3);
}

#[test]
fn branch_decision_tick_is_one_measure_before_branch_start() {
    let raw = r#"
TITLE:Branch Timing
BPM:120
COURSE:Oni
LEVEL:9
#START
0000,
0000,
#BRANCHSTART p,70,85
#N
1000,
#E
0100,
#M
0010,
#BRANCHEND
#END
"#;

    let importer = TjaImporter;
    let song = importer.import_song(raw.as_bytes()).expect("import song");
    let points = &song.courses[0].branch_decisions;

    assert_eq!(points.len(), 1);
    assert_eq!(points[0].decision_tick, 2 * TICKS_PER_SECOND);
}

#[test]
fn branch_accuracy_threshold_supports_decimal_values() {
    let raw = r#"
TITLE:Branch Accuracy Decimal
BPM:120
COURSE:Oni
LEVEL:9
#START
#BRANCHSTART p,60.5042,80
#N
1000,
#E
0100,
#M
0010,
#BRANCHEND
#END
"#;

    let importer = TjaImporter;
    let chart = importer.import(raw.as_bytes()).expect("import");

    assert_eq!(chart.branch_segments.len(), 1);
    assert_eq!(
        chart.branch_segments[0].decision_hint,
        Some(BranchDecisionHint::Accuracy {
            low: 605_042,
            high: 800_000,
        })
    );
}

#[test]
fn branch_accuracy_negative_thresholds_are_accepted() {
    let raw = r#"
TITLE:Branch Accuracy Negative
BPM:120
COURSE:Oni
LEVEL:9
#START
#BRANCHSTART p,-2,-1
#N
1000,
#E
0100,
#M
0010,
#BRANCHEND
#END
"#;

    let importer = TjaImporter;
    let chart = importer.import(raw.as_bytes()).expect("import");

    assert_eq!(chart.branch_segments.len(), 1);
    assert_eq!(
        chart.branch_segments[0].decision_hint,
        Some(BranchDecisionHint::Accuracy {
            low: -20_000,
            high: -10_000,
        })
    );
}

#[test]
fn branch_roll_negative_thresholds_are_accepted() {
    let raw = r#"
TITLE:Branch Roll Negative
BPM:120
COURSE:Oni
LEVEL:9
#START
#BRANCHSTART r,-2,-1
#N
1000,
#E
0100,
#M
0010,
#BRANCHEND
#END
"#;

    let importer = TjaImporter;
    let chart = importer.import(raw.as_bytes()).expect("import");

    assert_eq!(chart.branch_segments.len(), 1);
    assert_eq!(
        chart.branch_segments[0].decision_hint,
        Some(BranchDecisionHint::Roll { low: -2, high: -1 })
    );
}

#[test]
fn malformed_branch_missing_routes_fails() {
    let raw = r#"
TITLE:Bad Branch
BPM:120
COURSE:Oni
#START
#BRANCHSTART p,70,80
#N
1000,
#BRANCHEND
#END
"#;

    let importer = TjaImporter;
    let err = importer.import(raw.as_bytes()).expect_err("must fail");

    assert!(matches!(err, ImportError::InvalidFormat(_)));
}

#[test]
fn unmatched_roll_end_fails() {
    let raw = r#"
TITLE:Bad Roll End
BPM:120
COURSE:Oni
#START
8,
#END
"#;

    let importer = TjaImporter;
    let err = importer.import(raw.as_bytes()).expect_err("must fail");

    assert!(matches!(err, ImportError::InvalidFormat(_)));
}

#[test]
fn unclosed_roll_fails() {
    let raw = r#"
TITLE:Bad Roll Start
BPM:120
COURSE:Oni
#START
5,
#END
"#;

    let importer = TjaImporter;
    let err = importer.import(raw.as_bytes()).expect_err("must fail");

    assert!(matches!(err, ImportError::InvalidFormat(_)));
}

#[test]
fn nested_roll_start_in_same_stream_is_ignored() {
    let raw = r#"
TITLE:Nested Roll Start
BPM:120
COURSE:Oni
#START
900000000000000000000000000000000009000000000008,
#END
"#;

    let importer = TjaImporter;
    let chart = importer.import(raw.as_bytes()).expect("import");

    assert_eq!(chart.objects.len(), 1);
    assert!(chart.objects[0].end_tick >= chart.objects[0].start_tick);
}

#[test]
fn same_tick_bpm_change_inside_measure_is_allowed() {
    let raw = r#"
TITLE:Same Tick BPM
BPM:120
COURSE:Oni
LEVEL:6
#START
1000
#DELAY -0.5
#BPMCHANGE 150
1000,
#END
"#;

    let importer = TjaImporter;
    let chart = importer.import(raw.as_bytes()).expect("import");

    let tempo = chart
        .tempo_map
        .iter()
        .find(|tempo| tempo.tick == 500_000)
        .expect("tempo at 0.5s");
    assert_eq!(tempo.micros_per_quarter, 400_000);
}

#[test]
fn non_power_of_two_time_signature_denominator_is_allowed() {
    let raw = r#"
TITLE:Measure 5/6
BPM:120
COURSE:Oni
LEVEL:6
#START
0000,
#MEASURE 5/6
100000,
#END
"#;

    let importer = TjaImporter;
    let chart = importer.import(raw.as_bytes()).expect("import");

    assert!(chart
        .signatures
        .iter()
        .any(|signature| signature.numerator == 5 && signature.denominator == 6));
}

#[test]
fn initial_measure_at_tick_zero_overrides_seed_signature() {
    let raw = r#"
TITLE:Initial Measure
BPM:120
COURSE:Oni
LEVEL:6
#START
#MEASURE 1/8
3,
#END
"#;

    let importer = TjaImporter;
    let chart = importer.import(raw.as_bytes()).expect("import");

    let first = chart.signatures.first().expect("signature");
    assert_eq!(first.tick, 0);
    assert_eq!(first.numerator, 1);
    assert_eq!(first.denominator, 8);
}

#[test]
fn scroll_directive_maps_to_object_scroll_multiplier() {
    let raw = r#"
TITLE:Scroll Mapping
BPM:120
COURSE:Oni
LEVEL:6
#START
#SCROLL 2.5
1000,
#END
"#;

    let importer = TjaImporter;
    let chart = importer.import(raw.as_bytes()).expect("import");

    assert_eq!(chart.objects.len(), 1);
    assert_eq!(chart.objects[0].scroll_scaled, 2_500_000);
    let barline_scrolls = chart
        .events
        .iter()
        .filter_map(|event| match event.kind {
            rhythm_chart::ChartEventKind::BarLine { scroll_scaled } => Some(scroll_scaled),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(barline_scrolls, vec![2_500_000]);
}

#[test]
fn shift_jis_input_is_supported() {
    let source = "TITLE:テスト\nBPM:120\nCOURSE:Oni\n#START\n1,\n#END\n";
    let (encoded, _, had_errors) = SHIFT_JIS.encode(source);
    assert!(!had_errors);

    let importer = TjaImporter;
    let chart = importer.import(encoded.as_ref()).expect("import");

    assert_eq!(chart.metadata.title, "テスト");
    assert_eq!(chart.objects.len(), 1);
}

#[test]
fn golden_small_chart_hash() {
    let raw = r#"
TITLE:Golden
BPM:120
COURSE:Oni
LEVEL:6
#START
1000,
#END
"#;

    let importer = TjaImporter;
    let chart = importer.import(raw.as_bytes()).expect("import");
    let serialized = serde_json::to_vec(&chart).expect("serialize");

    assert_eq!(fnv1a64(&serialized), 16_210_016_265_926_925_368);
}

#[test]
fn dense_chart_capacity_is_tight_after_import() {
    let mut raw = String::from(
        r#"
TITLE:Dense Capacity
BPM:180
COURSE:Oni
LEVEL:10
#START
"#,
    );
    for _ in 0..256 {
        raw.push_str("1111111111111111,\n");
    }
    raw.push_str("#END\n");

    let importer = TjaImporter;
    let chart = importer.import(raw.as_bytes()).expect("import");

    assert_eq!(chart.tempo_map.len(), 1);
    assert_eq!(chart.signatures.len(), 1);
    assert!(
        chart.tempo_map.capacity() <= chart.tempo_map.len() + 8,
        "tempo capacity unexpectedly large: len={} cap={}",
        chart.tempo_map.len(),
        chart.tempo_map.capacity()
    );
    assert!(
        chart.signatures.capacity() <= chart.signatures.len() + 8,
        "signature capacity unexpectedly large: len={} cap={}",
        chart.signatures.len(),
        chart.signatures.capacity()
    );
}

fn fnv1a64(input: &[u8]) -> u64 {
    let mut state = 0xcbf29ce484222325_u64;
    for byte in input {
        state ^= u64::from(*byte);
        state = state.wrapping_mul(0x100000001b3);
    }
    state
}
