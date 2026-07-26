use encoding_rs::SHIFT_JIS;
use rhythm_chart::{BranchDecisionHint, ChartImporter, ImportError, TICKS_PER_SECOND};
use rhythm_importer_tja::{
    build_branch_decision_table, TjaImportLimits, TjaImporter, DEFAULT_BALLOON_HITS,
    TJA_IMPORTER_SEMANTICS_DESCRIPTOR, TJA_IMPORTER_SEMANTICS_SHA256,
    TJA_IMPORTER_SEMANTICS_VERSION,
};
use sha2::{Digest, Sha256};

const NOSFERATU_TJA: &[u8] = include_bytes!("../../taiko-game/samples/Nosferatu.tja");

fn tempo_micros_at(chart: &rhythm_chart::CanonicalChart, tick: rhythm_chart::Tick) -> u32 {
    let index = chart.tempo_map.partition_point(|tempo| tempo.tick <= tick);
    chart.tempo_map[index.saturating_sub(1)].micros_per_quarter
}

#[test]
fn nosferatu_ura_preserves_every_equal_bpm_scroll_pair() {
    let song = TjaImporter
        .import_song(NOSFERATU_TJA)
        .expect("import Nosferatu");
    let chart = song
        .courses
        .iter()
        .map(|course| &course.chart)
        .find(|chart| chart.metadata.difficulty_name.as_deref() == Some("4"))
        .expect("Nosferatu Ura course");
    let reference_micros = tempo_micros_at(chart, 0);
    assert_eq!(reference_micros, 300_000, "the chart starts at 200 BPM");

    let pairs = chart
        .objects
        .iter()
        .map(|object| {
            (
                object.scroll_scaled,
                tempo_micros_at(chart, object.start_tick),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        pairs,
        std::collections::BTreeSet::from([
            (630_000, 150_000),     // 400 BPM × 0.63
            (840_000, 200_000),     // 300 BPM × 0.84
            (1_260_000, 300_000),   // 200 BPM × 1.26
            (5_040_000, 1_200_000), // 50 BPM × 5.04
        ])
    );

    for object in &chart.objects {
        let object_micros = tempo_micros_at(chart, object.start_tick);
        let normalized_speed = i128::from(object.scroll_scaled) * i128::from(reference_micros)
            / i128::from(object_micros);
        assert_eq!(
            normalized_speed, 1_260_000,
            "object {} at tick {} changed visual speed",
            object.id, object.start_tick
        );
    }
}

#[test]
fn nosferatu_non_ura_courses_preserve_the_literal_point_67_scroll() {
    let song = TjaImporter
        .import_song(NOSFERATU_TJA)
        .expect("import Nosferatu");

    for difficulty in ["0", "1", "2", "3"] {
        let chart = song
            .courses
            .iter()
            .map(|course| &course.chart)
            .find(|chart| chart.metadata.difficulty_name.as_deref() == Some(difficulty))
            .expect("Nosferatu non-Ura course");
        let object = chart
            .objects
            .iter()
            .find(|object| {
                object.scroll_scaled == 670_000
                    && tempo_micros_at(chart, object.start_tick) == 200_000
            })
            .expect("300 BPM × 0.67 object");

        let normalized_speed = i128::from(object.scroll_scaled)
            * i128::from(tempo_micros_at(chart, 0))
            / i128::from(tempo_micros_at(chart, object.start_tick));
        assert_eq!(
            normalized_speed, 1_005_000,
            "course {difficulty} must honor the source's literal 0.67 multiplier"
        );
    }
}

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
fn branch_decision_before_chart_start_is_clamped_to_zero() {
    let raw = r#"
TITLE:Immediate Branch
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
    let song = importer.import_song(raw.as_bytes()).expect("import song");
    let points = &song.courses[0].branch_decisions;

    assert_eq!(points.len(), 1);
    assert_eq!(points[0].decision_tick, 0);
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
fn branch_accuracy_thresholds_outside_zero_to_one_hundred_are_rejected() {
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
    let error = importer.import(raw.as_bytes()).expect_err("must reject");
    assert!(error.to_string().contains("0..=100 percent"), "{error}");

    let over_one_hundred = raw.replace("p,-2,-1", "p,80,100.0001");
    let error = importer
        .import(over_one_hundred.as_bytes())
        .expect_err("must reject");
    assert!(error.to_string().contains("0..=100 percent"), "{error}");
}

#[test]
fn branch_roll_negative_thresholds_are_rejected() {
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
    let error = importer.import(raw.as_bytes()).expect_err("must reject");
    assert!(
        error.to_string().contains("must be non-negative"),
        "{error}"
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
fn branch_without_any_playable_object_is_rejected() {
    let raw = r#"
TITLE:Empty Branch
BPM:120
COURSE:Oni
#START
#BRANCHSTART p,70,80
#N
0,
#E
0,
#M
0,
#BRANCHEND
#END
"#;

    assert_invalid_contains(raw, "contains no playable objects");
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
fn nested_roll_start_in_same_stream_is_rejected() {
    let raw = r#"
TITLE:Nested Roll Start
BPM:120
COURSE:Oni
#START
500000000000000000000000000000000006000000000008,
#END
"#;

    let importer = TjaImporter;
    let error = importer.import(raw.as_bytes()).expect_err("must reject");
    assert!(error.to_string().contains("nested roll start"), "{error}");
}

#[test]
fn roll_end_before_start_is_rejected_instead_of_clamped() {
    let raw = r#"
TITLE:Backwards Roll
BPM:120
COURSE:Oni
#START
5,
#DELAY -10
8,
#END
"#;

    let error = TjaImporter.import(raw.as_bytes()).expect_err("must reject");
    assert!(
        error.to_string().contains("roll end precedes start"),
        "{error}"
    );
}

#[test]
fn big_roll_preserves_the_big_flag() {
    let raw = r#"
TITLE:Big Roll
BPM:120
COURSE:Oni
#START
6008,
#END
"#;

    let chart = TjaImporter.import(raw.as_bytes()).expect("import");
    assert_eq!(chart.objects.len(), 1);
    assert_eq!(chart.objects[0].flags, rhythm_importer_tja::FLAG_BIG);
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
fn bpm_scroll_and_measure_directives_apply_at_the_next_symbol_boundary() {
    let raw = r#"
TITLE:Mid-measure speed boundary
BPM:200
COURSE:Oni
LEVEL:10
#START
#MEASURE 3/4
#SCROLL 1.26
1000
#BPMCHANGE 400
#SCROLL 0.63
1000
#BPMCHANGE 50
#SCROLL 5.04
1000,
#END
"#;

    let chart = TjaImporter.import(raw.as_bytes()).expect("import");
    let objects = chart
        .objects
        .iter()
        .map(|object| {
            (
                object.start_tick,
                object.scroll_scaled,
                tempo_micros_at(&chart, object.start_tick),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        objects,
        vec![
            (0, 1_260_000, 300_000),
            (300_000, 630_000, 150_000),
            (450_000, 5_040_000, 1_200_000),
        ]
    );
    assert_eq!(
        chart
            .signatures
            .iter()
            .map(|signature| (signature.tick, signature.numerator, signature.denominator))
            .collect::<Vec<_>>(),
        vec![(0, 3, 4)]
    );
}

#[test]
fn long_roll_duration_integrates_mid_measure_bpm_changes() {
    let raw = r#"
TITLE:Mid-measure roll boundary
BPM:200
COURSE:Oni
LEVEL:10
#START
#MEASURE 3/4
#SCROLL 1.26
5000
#BPMCHANGE 400
#SCROLL 0.63
0000
#BPMCHANGE 50
#SCROLL 5.04
0008,
#END
"#;

    let chart = TjaImporter.import(raw.as_bytes()).expect("import");
    assert_eq!(chart.objects.len(), 1);
    let roll = &chart.objects[0];
    assert_eq!(roll.kind, rhythm_chart::ObjectKind::Roll);
    assert_eq!(roll.start_tick, 0);
    assert_eq!(roll.end_tick, 1_350_000);
    assert_eq!(roll.scroll_scaled, 1_260_000);
    assert_eq!(
        chart
            .tempo_map
            .iter()
            .map(|tempo| (tempo.tick, tempo.micros_per_quarter))
            .collect::<Vec<_>>(),
        vec![(0, 300_000), (300_000, 150_000), (450_000, 1_200_000)]
    );
}

#[test]
fn strict_numeric_contract_rejects_invalid_metadata() {
    for (field, value, expected) in [
        ("BPM", "garbage", "invalid BPM"),
        ("BPM", "NaN", "must be finite"),
        ("BPM", "0", "finite and positive"),
        ("BPM", "-1", "finite and positive"),
        ("BPM", "0.001", "outside the canonical tempo range"),
        ("BPM", "120000001", "outside the canonical tempo range"),
        ("OFFSET", "garbage", "invalid OFFSET"),
        ("OFFSET", "NaN", "must be finite"),
        (
            "OFFSET",
            "9223372036854.775808",
            "outside the canonical tick range",
        ),
        ("DEMOSTART", "NaN", "must be finite"),
        ("DEMOSTART", "-0.1", "must be non-negative"),
    ] {
        let raw = format!(
            "TITLE:Invalid Numeric\nBPM:120\n{field}:{value}\nCOURSE:Oni\n#START\n1,\n#END\n"
        );
        assert_invalid_contains(&raw, expected);
    }

    let missing_bpm = "TITLE:Missing BPM\nCOURSE:Oni\n#START\n1,\n#END\n";
    assert_invalid_contains(missing_bpm, "missing required");
}

#[test]
fn strict_numeric_contract_rejects_malformed_chart_directives_even_without_notes() {
    for (directive, expected) in [
        ("#BPMCHANGE garbage", "invalid #BPMCHANGE"),
        ("#BPMCHANGE NaN", "must be finite"),
        ("#BPMCHANGE 0", "finite and positive"),
        ("#SCROLL garbage", "invalid #SCROLL"),
        ("#SCROLL NaN", "must be finite"),
        ("#SCROLL 2148", "outside the canonical fixed-point range"),
        ("#DELAY garbage", "invalid #DELAY"),
        ("#DELAY inf", "must be finite"),
        ("#MEASURE 4", "requires numerator/denominator"),
        ("#MEASURE 4/0", "must be non-zero"),
        ("#MEASURE 256/4", "numerator must be"),
    ] {
        let raw = simple_chart(&format!("{directive}\n0,"));
        assert_invalid_contains(&raw, expected);
    }
}

#[test]
fn strict_headers_reject_invalid_course_and_level() {
    for raw in [
        "TITLE:Bad Course\nBPM:120\nCOURSE:Impossible\n#START\n1,\n#END\n",
        "TITLE:Bad Level\nBPM:120\nCOURSE:Oni\nLEVEL:0\n#START\n1,\n#END\n",
        "TITLE:Bad Level\nBPM:120\nCOURSE:Oni\nLEVEL:11\n#START\n1,\n#END\n",
        "TITLE:Bad Level\nBPM:120\nCOURSE:Oni\nLEVEL:nope\n#START\n1,\n#END\n",
    ] {
        assert_invalid_contains(
            raw,
            if raw.contains("COURSE:Impossible") {
                "unsupported COURSE"
            } else {
                "LEVEL must be in 1..=10"
            },
        );
    }
}

#[test]
fn balloon_counts_are_strict_and_exact() {
    let cases = [
        (
            "TITLE:B\nBPM:120\nCOURSE:Oni\nBALLOON:0\n#START\n7008,\n#END\n",
            "must be positive",
        ),
        (
            "TITLE:B\nBPM:120\nCOURSE:Oni\nBALLOON:-1\n#START\n7008,\n#END\n",
            "hit counts must be",
        ),
        (
            "TITLE:B\nBPM:120\nCOURSE:Oni\nBALLOON:65536\n#START\n7008,\n#END\n",
            "hit counts must be",
        ),
        (
            "TITLE:B\nBPM:120\nCOURSE:Oni\nBALLOON:5,bad\n#START\n7008,\n#END\n",
            "hit counts must be",
        ),
        (
            "TITLE:B\nBPM:120\nCOURSE:Oni\nBALLOON:5,\n#START\n7008,\n#END\n",
            "hit counts must be",
        ),
        (
            "TITLE:B\nBPM:120\nCOURSE:Oni\nBALLOON:5,6\n#START\n7008,\n#END\n",
            "BALLOON values must match",
        ),
    ];
    for (raw, expected) in cases {
        assert_invalid_contains(raw, expected);
    }

    let valid = "TITLE:B\nBPM:120\nCOURSE:Oni\nBALLOON:5\n#START\n7008,\n#END\n";
    let chart = TjaImporter.import(valid.as_bytes()).expect("valid balloon");
    assert_eq!(chart.objects[0].required_hits, 5);

    let explicit_default = "TITLE:B\nBPM:120\nCOURSE:Oni\nBALLOON:\n#START\n700800007008,\n#END\n";
    let chart = TjaImporter
        .import(explicit_default.as_bytes())
        .expect("explicit empty BALLOON uses the documented default");
    assert_eq!(chart.objects.len(), 2);
    assert!(chart
        .objects
        .iter()
        .all(|object| object.required_hits == DEFAULT_BALLOON_HITS));

    let missing_header = "TITLE:B\nBPM:120\nCOURSE:Oni\n#START\n700800007008,\n#END\n";
    let chart = TjaImporter
        .import(missing_header.as_bytes())
        .expect("missing BALLOON uses the documented application default");
    assert_eq!(chart.objects.len(), 2);
    assert!(chart
        .objects
        .iter()
        .all(|object| object.required_hits == DEFAULT_BALLOON_HITS));
}

#[test]
fn optional_audio_and_legacy_score_metadata_accept_missing_or_empty_values() {
    let raw = r#"
TITLE:Silent Chart
WAVE:
BPM:120
SCOREMODE:
COURSE:Oni
LEVEL:5
SCOREINIT:
SCOREDIFF:
#START
1000,
#END
"#;

    let song = TjaImporter
        .import_song(raw.as_bytes())
        .expect("empty optional metadata is valid");
    assert_eq!(song.audio_path, None);

    let paired_score_init = raw.replace("SCOREINIT:", "SCOREINIT:500,1200");
    TjaImporter
        .import_song(paired_score_init.as_bytes())
        .expect("one- and two-player legacy SCOREINIT values are valid and ignored");
    assert_eq!(song.courses[0].chart.metadata.audio_path, None);

    let without_optional_headers = raw
        .replace("WAVE:\n", "")
        .replace("SCOREMODE:\n", "")
        .replace("SCOREINIT:\n", "")
        .replace("SCOREDIFF:\n", "");
    let song = TjaImporter
        .import_song(without_optional_headers.as_bytes())
        .expect("missing optional metadata is valid");
    assert_eq!(song.audio_path, None);
}

#[test]
fn nonempty_legacy_score_metadata_remains_strictly_validated() {
    for (field, scope) in [
        ("SCOREMODE:not-a-number", "metadata"),
        ("SCOREINIT:not-a-number", "course"),
        ("SCOREDIFF:not-a-number", "course"),
    ] {
        let raw = match scope {
            "metadata" => format!("TITLE:X\nBPM:120\n{field}\nCOURSE:Oni\n#START\n1,\n#END\n"),
            "course" => {
                format!("TITLE:X\nBPM:120\nCOURSE:Oni\n{field}\n#START\n1,\n#END\n")
            }
            _ => unreachable!(),
        };
        assert_invalid_contains(
            raw,
            if field.starts_with("SCOREINIT") {
                "one or two unsigned integers"
            } else {
                "must be an unsigned integer"
            },
        );
    }

    for value in ["1,2,3", "1,", ",2"] {
        let raw = format!("TITLE:X\nBPM:120\nCOURSE:Oni\nSCOREINIT:{value}\n#START\n1,\n#END\n");
        assert_invalid_contains(raw, "one or two unsigned integers");
    }
}

#[test]
fn strict_source_structure_prevents_silent_content_loss() {
    let cases = [
        (
            "TITLE:X\nBPM:120\nCOURSE:Oni\n#START\n1,\n#START\n1,\n#END\n",
            "nested #START",
        ),
        ("TITLE:X\nBPM:120\nCOURSE:Oni\n#START\n1,\n", "missing #END"),
        (
            "TITLE:X\nBPM:120\nCOURSE:Oni\n#END\n",
            "#END without an open course",
        ),
        (
            "TITLE:X\nBPM:120\nCOURSE:Oni\n1,\n#START\n1,\n#END\n",
            "expected a metadata/header",
        ),
        (
            "TITLE:X\nBPM:120\nCOURSE:Oni\n#START\n1x,\n#END\n",
            "chart data may contain only",
        ),
        (
            "TITLE:X\nBPM:120\nCOURSE:Oni\n#START\n#UNKNOWN\n1,\n#END\n",
            "unsupported or malformed directive",
        ),
        (
            "TITLE:X\nBPM:120\nCOURSE:Oni\n#START\n#SECTION\n1,\n#END\n",
            "#SECTION is unsupported",
        ),
        ("TITLE:X\nBPM:120\nCOURSE:Oni\n", "no complete courses"),
    ];
    for (raw, expected) in cases {
        assert_invalid_contains(raw, expected);
    }
}

#[test]
fn content_after_branch_end_is_rejected_instead_of_dropped_by_upstream_parser() {
    let raw = r#"
TITLE:Branch Tail
BPM:120
COURSE:Oni
#START
#BRANCHSTART p,70,85
#N
1,
#E
2,
#M
3,
#BRANCHEND
4,
#END
"#;

    assert_invalid_contains(raw, "only #END may follow #BRANCHEND");
}

#[test]
fn unknown_branch_condition_kind_is_rejected() {
    let raw = r#"
TITLE:Unknown Branch
BPM:120
COURSE:Oni
#START
#BRANCHSTART custom,1,2
#N
1,
#E
2,
#M
3,
#BRANCHEND
#END
"#;

    assert_invalid_contains(raw, "unsupported branch condition kind");
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

#[test]
fn bounded_semantics_fingerprint_is_pinned() {
    assert_eq!(TJA_IMPORTER_SEMANTICS_VERSION, 3);
    assert_eq!(
        hex::encode(Sha256::digest(TJA_IMPORTER_SEMANTICS_DESCRIPTOR.as_bytes())),
        TJA_IMPORTER_SEMANTICS_SHA256
    );
}

#[test]
fn note_symbol_limit_rejects_the_first_parser_amplifying_symbol() {
    let raw = simple_chart("000,");
    let limits = TjaImportLimits {
        max_note_symbols_per_course: 2,
        ..TjaImportLimits::default()
    };

    assert_limit_error(&raw, limits, "note symbols per course 3 > 2");
}

#[test]
fn segment_limit_rejects_the_first_excess_comma() {
    let raw = simple_chart("0,0,");
    let limits = TjaImportLimits {
        max_segments_per_course: 1,
        ..TjaImportLimits::default()
    };

    assert_limit_error(&raw, limits, "segments per course 2 > 1");
}

#[test]
fn object_limit_rejects_the_first_excess_object_token_before_parse() {
    let raw = simple_chart("1100,");
    let limits = TjaImportLimits {
        max_objects_per_course: 1,
        ..TjaImportLimits::default()
    };

    assert_limit_error(&raw, limits, "objects per course 2 > 1");
}

#[test]
fn event_limit_rejects_the_first_excess_canonical_event() {
    let raw = simple_chart("1000,\n1000,");
    let limits = TjaImportLimits {
        max_events_per_course: 1,
        ..TjaImportLimits::default()
    };

    assert_limit_error(&raw, limits, "events per course 2 > 1");
}

#[test]
fn tempo_limit_rejects_the_first_excess_canonical_change() {
    let raw = simple_chart("1,\n#BPMCHANGE 180\n1,");
    let limits = TjaImportLimits {
        max_tempo_changes_per_course: 1,
        ..TjaImportLimits::default()
    };

    assert_limit_error(&raw, limits, "tempo changes per course 2 > 1");
}

#[test]
fn signature_limit_rejects_the_first_excess_canonical_change() {
    let raw = simple_chart("#MEASURE 3/4\n1,\n#MEASURE 5/4\n1,");
    let limits = TjaImportLimits {
        max_time_signatures_per_course: 1,
        ..TjaImportLimits::default()
    };

    assert_limit_error(&raw, limits, "time signatures per course 2 > 1");
}

#[test]
fn branch_segment_limit_rejects_before_allocating_the_excess_cycle() {
    let raw = repeated_branch_chart();
    let limits = TjaImportLimits {
        max_branch_segments_per_course: 1,
        max_branch_decisions_per_course: 8,
        ..TjaImportLimits::default()
    };

    assert_limit_error(&raw, limits, "branch segments per course 2 > 1");
}

#[test]
fn branch_decision_limit_rejects_before_pushing_the_excess_decision() {
    let raw = repeated_branch_chart();
    let limits = TjaImportLimits {
        max_branch_segments_per_course: 8,
        max_branch_decisions_per_course: 1,
        ..TjaImportLimits::default()
    };

    assert_limit_error(&raw, limits, "branch decisions per course 2 > 1");
}

#[test]
fn aggregate_limit_cannot_be_multiplied_by_multiple_courses() {
    let raw = r#"
TITLE:Aggregate Bound
BPM:120
COURSE:Easy
#START
1,
#END
COURSE:Oni
#START
1,
#END
"#;
    let limits = TjaImportLimits {
        max_total_objects: 1,
        ..TjaImportLimits::default()
    };

    assert_limit_error(raw, limits, "total objects 2 > 1");
}

#[test]
fn balloon_header_is_bounded_before_upstream_header_allocation() {
    let raw = r#"
TITLE:Balloon Bound
BPM:120
COURSE:Oni
BALLOON:1,2
#START
7,8,
#END
"#;
    let limits = TjaImportLimits {
        max_balloon_values_per_course: 1,
        ..TjaImportLimits::default()
    };

    assert_limit_error(raw, limits, "balloon values per course 2 > 1");
}

fn simple_chart(body: &str) -> String {
    format!("TITLE:Bounded\nBPM:120\nCOURSE:Oni\n#START\n{body}\n#END\n")
}

fn repeated_branch_chart() -> String {
    simple_chart(
        "#BRANCHSTART p,70,85\n\
         #N\n1,\n\
         #E\n1,\n\
         #M\n1,\n\
         #N\n1,\n\
         #E\n1,\n\
         #M\n1,\n\
         #BRANCHEND",
    )
}

fn assert_limit_error(raw: impl AsRef<[u8]>, limits: TjaImportLimits, expected: &str) {
    let error = TjaImporter
        .import_song_with_limits(raw.as_ref(), limits)
        .expect_err("limit must reject input");
    assert!(
        error.to_string().contains(expected),
        "expected {expected:?}, got {error}"
    );
}

fn assert_invalid_contains(raw: impl AsRef<[u8]>, expected: &str) {
    let error = TjaImporter
        .import(raw.as_ref())
        .expect_err("input must be rejected");
    assert!(
        error.to_string().contains(expected),
        "expected {expected:?}, got {error}"
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
