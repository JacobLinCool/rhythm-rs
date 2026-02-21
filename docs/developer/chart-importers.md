# Chart Importers Guide / 譜面匯入器指南

## Importer Contract / 匯入器介面

```rust
pub trait ChartImporter {
    fn import(&self, raw: &[u8]) -> Result<CanonicalChart, ImportError>;
}
```

## Built-in Importers / 內建匯入器

- `JsonImporter` (`rhythm-chart`): canonical JSON -> `CanonicalChart`
- `TjaImporter` (`rhythm-importer-tja`): `.tja` -> `CanonicalChart`

`rhythm-importer-tja` backend parser is pinned:

- `tja = "=0.5.0"`

## TjaImporter Facade / TjaImporter 對外介面

```rust
use rhythm_importer_tja::TjaImporter;

# fn run(raw: &[u8]) -> anyhow::Result<()> {
let importer = TjaImporter;
let chart = importer.import(raw)?;
let courses = importer.import_all(raw)?;
let song = importer.import_song(raw)?; // song metadata + per-course branch decision table
# Ok(()) }
```

## Branch Decision Table / 分支決策表

For adapter/UI layer, importer exposes per-course decision points:

- `segment_id`
- `decision_tick`
- `route_count`
- `hint`

This keeps `CanonicalChart` unchanged while enabling external `BranchController`.

## Strictness / 嚴格策略

Importer is strict fail-fast:

- malformed branch block
- missing N/E/M routes
- unmatched roll end / unclosed roll
- conflicting timing states

All above return `ImportError::InvalidFormat`.

## Deterministic Notes / 決定性注意事項

1. Output courses are sorted by `difficulty_level` asc and stable by original order.
2. Converter always calls `sort_and_validate()` before returning.
3. Branch decision table is derived deterministically from canonical objects.
