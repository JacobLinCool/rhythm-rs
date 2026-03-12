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
- `taiko-multiplayer-protocol`: shared websocket payload schema for taiko multiplayer/spectate.
- `taiko-resource-protocol`: shared HTTP payload schema for remote resource delivery.
- `taiko-resource-server`: HTTP+WS server that exposes song list/chart/audio resources and multiplayer rooms for `taiko-game`.

## Quick start

```bash
cargo run -p taiko-game  --release -- --songdir ./taiko-game/songs

# or run with remote resources
cargo run -p taiko-game --release -- server --songdir ./taiko-game/songs
cargo run -p taiko-game --release -- --resource-endpoint http://127.0.0.1:4150/
cargo run -p taiko-game --release -- online create --server http://127.0.0.1:4150 --name host
cargo run -p taiko-game --release -- online join --server http://127.0.0.1:4150 --room <CODE> --name p2
cargo run -p taiko-game --release -- online spectate --server http://127.0.0.1:4150 --room <CODE> --name viewer

# inspect/clean remote cache
cargo run -p taiko-game -- cache list
cargo run -p taiko-game -- cache clear --all
```

## Developer docs

- [General Core](docs/developer/general-core.md)
- [Branching](docs/developer/branching.md)
- [Chart Importers](docs/developer/chart-importers.md)
- [Custom Chart Spec](docs/developer/custom-chart-spec.md)
- [Remote Resource Server](docs/remote-resource-server.md)

## Quality gates

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p rhythm-mode-taiko --release -- --ignored bench_smoke
cargo test -p taiko-game --release -- --ignored bench_smoke
```
