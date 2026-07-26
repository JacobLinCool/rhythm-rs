# Branching Guide / 分支控制指南

## Core Boundary / 核心邊界

- Condition evaluation is always external.
- `rhythm-core` only consumes controls: `TimedControl<BranchControl>`.
- `TaikoRuntime` advances exactly to each decision boundary. At a boundary it
  expires objects and applies those judges, evaluates the branch window,
  applies the resulting control, then processes same-tick inputs and applies
  their judges. Coarse and fine caller cadence therefore produce the same
  replay.

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

- `p = (2 × great + ok) / (2 × (great + ok + miss)) × 100`
- `r = roll_hits`
- `s = score`

Decision window:

- Every metric uses the delta between the previous decision tick and the
  current decision tick. Multiple decisions on the same tick observe the same
  window snapshot.
- With no judged taps in the window, accuracy is `100%`.
- The player client and authority always derive `Automatic`, which selects the
  metric from each TJA `p`/`r`/`s` hint. It is not a CLI, preference, UI, or wire
  option.

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

- Missing, raw, or otherwise unsupported TJA hints are rejected before play.
- The engine library retains explicit policy/fixed-route APIs for controlled
  engine use and tests, but the player client and server have no path that can
  select them.
- No silent fallback route rewriting.

## Determinism / 決定性

Replay hash includes:

- inputs
- controls
- judges

So same chart + same inputs + same controls => same replay hash.
