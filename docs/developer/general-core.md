# General Core Guide / 通用核心指南

## Purpose / 目的

**EN**: `rhythm-core` is a deterministic headless runtime. Rules are implemented by `Mode` plugins. UI/audio/input stay outside core.

**中文**：`rhythm-core` 是可決定性無頭 runtime，規則由 `Mode` plugin 實作；UI/音訊/輸入都在核心外。

## Main Interfaces / 主要介面

- `Mode`: gameplay plugin contract
- `ControlledMode`: optional control channel contract
- `BasicEngine<M>`
  - `new_basic(chart)`
  - `step_to(now, inputs) -> Result<FrameOutput<M>, StepError>`
- `ControlledEngine<M>`
  - `new_controlled(chart)`
  - `step_to_with_controls(now, controls, inputs) -> Result<FrameOutput<M>, StepError>`

## Deterministic Rules / 決定性規則

- `step_to`: `expired -> input -> apply`
- `step_to_with_controls`: `expired -> control -> input -> apply`
- inputs/controls must be monotonic and sorted by tick
- replay hash includes input + control + judge

## Minimal Controlled Loop / 最小控制迴圈

```rust
use rhythm_core::{ControlledEngine, TimedControl, TimedInput, BranchControl};
use rhythm_mode_taiko::{TaikoAction, TaikoMode};

# fn run(chart: &rhythm_chart::CanonicalChart) -> anyhow::Result<()> {
let mut engine = ControlledEngine::<TaikoMode>::new_controlled(chart)?;

let controls = [TimedControl {
    tick: 1_000_000,
    control: BranchControl::SetBranchRoute { segment_id: 1, route_id: 1 },
}];

let inputs = [TimedInput {
    tick: 1_000_000,
    action: TaikoAction::LEFT_DON,
}];

let frame = engine.step_to_with_controls(1_000_000, &controls, &inputs)?;
println!("hash={}", frame.replay_hash);
# Ok(()) }
```

## Adapter Pattern / Adapter 模式

`taiko-game` uses this layering:

1. importer reads chart -> canonical + branch decision table
2. adapter computes external branch policy
3. adapter sends `TimedControl` + `TimedInput` each tick
4. adapter renders only `FrameOutput`

This keeps gameplay semantics in core and UI behavior in adapter.
