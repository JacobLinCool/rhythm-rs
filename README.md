# rhythm-rs

Deterministic, headless rhythm game engine in Rust.

## Workspace crates

- `rhythm-chart`: canonical chart IR (`CanonicalChart`) and importer trait (`ChartImporter`).
- `rhythm-core`: deterministic simulation runtime (`BasicEngine<M>` / `ControlledEngine<M>` + `Mode` trait).
- `rhythm-mode-taiko`: taiko rules/scoring plugin.
- `rhythm-mode-lane`: lane-based (tap/hold/flick) rules/scoring plugin.
- `rhythm-mode-radial`: radial/touch/slide rules/scoring plugin.
- `rhythm-importer-tja`: `.tja` importer adapter (`TjaImporter`) backed by crates.io `tja`.
- `taiko-game`: playable TUI taiko game using `rhythm-core` + `rhythm-mode-taiko`.

## Quick start

```bash
cargo run -p taiko-game  --release -- --songdir ./taiko-game/songs
```

## Developer docs

- [General Core](docs/developer/general-core.md)
- [Branching](docs/developer/branching.md)
- [Chart Importers](docs/developer/chart-importers.md)
- [Custom Chart Spec](docs/developer/custom-chart-spec.md)

## Quality gates

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p rhythm-mode-taiko --release -- --ignored bench_smoke
cargo test -p taiko-game --release -- --ignored bench_smoke
```
