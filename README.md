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
- `taiko-audio`: shared fail-closed audio validation and decoded-frame model used
  by the game and resource server.
- `taiko-multiplayer-protocol`: shared websocket payload schema for taiko multiplayer/spectate.
- `taiko-resource-protocol`: shared HTTP payload schema for remote resource delivery.
- `taiko-resource-server`: HTTP+WS server that exposes song list/chart/audio resources and multiplayer rooms for `taiko-game`.

## Quick start

```bash
cargo run -p taiko-game --release -- --songdir ./taiko-game/songs

# The game opens a mode menu:
# Single Player / Local Two Player / Online Multiplayer

# Optional: run a dedicated server for remote resources and online rooms.
cargo run -p taiko-game --release -- server --songdir ./taiko-game/songs
cargo run -p taiko-game --release -- --resource-endpoint http://127.0.0.1:4150/

# inspect/clean remote cache
cargo run -p taiko-game -- cache list
cargo run -p taiko-game -- cache clear --all
```

## Controller Setup

Start the game normally, then press `C` from the play-mode menu. Keyboard,
terminal pointer, and phone controllers all feed the same
four distinct inputs for the selected player:
`LEFT KAT | LEFT DON | RIGHT DON | RIGHT KAT`.

- **MacBook trackpad or mouse:** set **Terminal pointer** to P1 or P2, then
  left-click the four drum pads shown in Controller Setup or during play. The
  terminal receives pointer-cell clicks rather than raw touch coordinates, so
  this is intended as a convenient casual controller.
- **Phone on the same LAN:** enter one exact local/LAN bind address and start
  **Phone controller server**. Select the P1 or P2 phone row and press `Enter`
  to show its one-time QR code, or press `C` to copy the link. Open it on a
  phone connected to the same network; the page supports multi-touch and
  follows the game's English, Traditional Chinese, or Japanese UI language.
  QR display needs roughly 32 terminal rows at the longest localized URL; if
  the terminal is shorter, enlarge it or use `C` to copy the link instead.
- The Controller Setup page accepts live test hits without changing score.
  Press `R` on a P1/P2 phone row to revoke its session and create a new
  one-time pairing link.

The phone server is unencrypted and deliberately has no relay, TLS, NAT
traversal, or Internet mode. Enable it only on a LAN whose other users you
trust, and stop it when play is finished.

## Developer docs

- [General Core](docs/developer/general-core.md)
- [Branching](docs/developer/branching.md)
- [Chart Importers](docs/developer/chart-importers.md)
- [Custom Chart Spec](docs/developer/custom-chart-spec.md)
- [Remote Resource Server](docs/remote-resource-server.md)
- [Multiplayer Guide](docs/multiplayer.md)
- [Multiplayer Protocol v2](docs/developer/multiplayer-protocol-v2.md)
- [Multiplayer Data and Sync Design Review](docs/developer/multiplayer-data-sync-review.md)
- [Multiplayer Reliability Testing](docs/developer/multiplayer-testing.md)
- [Game Mode Architecture](docs/developer/game-mode-architecture.md)
- [Controller Input Architecture](docs/developer/controller-input-architecture.md)
- [Player UI Localization](docs/developer/localization.md)
- [TUI Usability Audit](docs/developer/usability-audit.md)

## Quality gates

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p rhythm-mode-taiko --release -- --ignored bench_smoke
cargo test -p taiko-game --release -- --ignored bench_smoke
```
