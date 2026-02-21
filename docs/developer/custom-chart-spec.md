# Custom Chart Spec Guide / 自訂譜面格式指南

## Goal / 目標

Convert external chart formats into deterministic `CanonicalChart`.

## Required Mapping / 必要映射

1. Time base -> `Tick (i64)`
2. Timing -> `tempo_map`, `signatures`
3. Objects -> `ObjectKind` + `Object`
4. Lanes/regions -> `lanes`, `lane_or_region`
5. Optional branch -> `branch_segments` + object route fields

## Importer Skeleton / 匯入器骨架

```rust
use rhythm_chart::{CanonicalChart, ChartImporter, ImportError};

pub struct MyImporter;

impl ChartImporter for MyImporter {
    fn import(&self, raw: &[u8]) -> Result<CanonicalChart, ImportError> {
        let mut chart = parse_my_format(raw)?;
        chart.sort_and_validate()?;
        Ok(chart)
    }
}

fn parse_my_format(_raw: &[u8]) -> Result<CanonicalChart, ImportError> {
    todo!()
}
```

## Validation Checklist / 驗證清單

- non-empty tempo map
- unique object IDs
- sorted objects by `(start_tick, id)`
- `end_tick >= start_tick`
- branch segment IDs strictly ascending
- object branch route in range
- non-branch objects must use `branch_route_id = 0`

## Branching Mapping Advice / 分支映射建議

If source format has branch directives:

- map branch metadata to `BranchSegment.decision_hint`
- keep route IDs deterministic (`0..route_count-1`)
- assign each object with `branch_segment_id` + `branch_route_id`

If adapter/UI needs extra metadata (for example decision ticks), expose helper context outside `CanonicalChart` (same pattern as `rhythm-importer-tja::import_song`).

## Error Strategy / 錯誤策略

Use strict fail-fast:

- malformed source -> `ImportError::InvalidFormat`
- invalid encoding -> `ImportError::InvalidEncoding`
- no silent fallback or auto-repair for invalid branch data
