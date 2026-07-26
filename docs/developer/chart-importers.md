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

`TjaImporter` treats the pinned parser as an implementation detail, not as the
accepted-data contract. Before parsing it validates:

- complete uppercase `#START` / `#END` course structure;
- the declared metadata/header/directive set and note-line grammar;
- finite, domain-valid, canonically representable BPM, offset, delay, scroll,
  measure, demo-start, course, level, non-empty legacy score metadata, and
  explicit balloon values;
- exact one-to-one positive hit counts when `BALLOON` is non-empty; a missing or
  explicitly empty `BALLOON:` instead assigns this application's fingerprinted
  `DEFAULT_BALLOON_HITS` value (`5`) to every balloon note;
- strict N/E/M branch cycles and supported `p`/`r`/`s` thresholds; and
- matched, non-nested, forward roll pairs.

After parsing it requires exact course, segment, note, and balloon-count
agreement with the source scan. This catches parser-side defaulting, ignored
malformed directives, and dropped content. In particular, content after
`#BRANCHEND` is rejected because `tja@0.5.0` would silently discard it.

All violations return `ImportError::InvalidFormat`; there is no repair or
compatibility path. Per-course and whole-import budgets also bound raw bytes,
line/value size, courses, note symbols, segments, canonical objects/events/maps,
and branch data before each allocation or insertion.

`WAVE` is optional: a missing or empty value produces `audio_path: None` and
never probes for a same-stem audio file. `SCOREINIT`, `SCOREDIFF`, and
`SCOREMODE` are also optional-empty legacy metadata. Non-empty values remain
strictly parsed, but they do not affect the canonical chart or the current
ruleset's equivalent scoring.

The balloon value `5` is the application's canonical default, retained from
the importer's former implicit behavior and now named, tested, and included in
the importer semantic fingerprint. It is not presented as an official TJA
format default.

## Deterministic Notes / 決定性注意事項

1. Output courses are sorted by known TJA course rank, then level, then source
   order.
2. Seconds, BPM, and scroll are converted with the rounding and range rules in
   `TJA_IMPORTER_SEMANTICS_DESCRIPTOR`.
3. Note digits map explicitly to Don/Kat/big/roll/balloon objects; big-roll
   flags and balloon hit counts are preserved.
4. Tempo, signature, scroll, bar-line, gogo, and branch policies—including
   same-tick behavior and default-route projection—are all part of that pinned
   semantic descriptor.
5. The converter always calls `sort_and_validate()` before returning.
6. The branch decision table is derived deterministically from canonical
   objects.

`TJA_IMPORTER_SEMANTICS_VERSION` and
`TJA_IMPORTER_SEMANTICS_SHA256` bind the complete mapping and all default
resource budgets into multiplayer resource manifests. A peer with different
import behavior cannot pass readiness.

## Visual velocity / 視覺速度

Canonical charts deliberately keep tempo and `scroll_scaled` as separate source
semantics. Judgement continues to use the canonical microsecond timestamps.
When `rhythm-mode-taiko` compiles presentation data, it derives a fixed-point
visual speed for every note and bar line:

```text
visual speed = scroll × BPM at object / initial BPM
             = scroll × initial micros-per-quarter / object micros-per-quarter
```

The TUI renderer consumes this derived visual speed and applies only the
player's global scroll setting. It must not project notes from raw
`scroll_scaled` alone. For example, `BPM 200 × SCROLL 1.26`, `BPM 400 ×
SCROLL 0.63`, and `BPM 50 × SCROLL 5.04` all produce the same visual velocity.
This separation preserves exact judgement time while rendering intentional TJA
velocity-sync sections correctly.
