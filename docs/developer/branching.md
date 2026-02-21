# Branching Guide / 分支控制指南

## Core Boundary / 核心邊界

- Condition evaluation is always external.
- `rhythm-core` only consumes controls: `TimedControl<BranchControl>`.
- Deterministic order in controlled step:
  - `consume_expired -> consume_control -> consume_input -> apply_judge`

## Control Channel / 控制通道

```rust
pub enum BranchControl {
    SetBranchRoute { segment_id: u32, route_id: u8 },
    EnableSegment { segment_id: u32 },
    DisableSegment { segment_id: u32 },
}
```

## TJA p/r/s Policy (taiko-game) / TJA p/r/s 策略（taiko-game）

Decision formula (strict):

- `value < low => N(route 0)`
- `low <= value < high => E(route 1)`
- `value >= high => M(route 2)`

Metric mapping:

- `p = (great + ok) / (great + ok + miss) * 100`
- `r = roll_hits`
- `s = score`

Decision window:

- Uses delta score between previous decision point and current decision point.

Decision time semantics:

- Control at `tick = T` affects unresolved objects with `start_tick >= T`.

## BranchController Example / BranchController 範例

```rust
use rhythm_core::{BranchControl, TimedControl, Tick};

fn emit_route(segment_id: u32, tick: Tick, route_id: u8) -> TimedControl<BranchControl> {
    TimedControl {
        tick,
        control: BranchControl::SetBranchRoute { segment_id, route_id },
    }
}
```

## Error Strategy / 錯誤策略

`taiko-game` follows strict fail-fast:

- Missing/unsupported hint for selected policy -> error page.
- Invalid fixed route -> error page.
- No silent fallback route rewriting.

## Determinism / 決定性

Replay hash includes:

- inputs
- controls
- judges

So same chart + same inputs + same controls => same replay hash.
