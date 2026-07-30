# rhythm-rs

Deterministic, headless rhythm game engine in Rust.

## Install the latest preview

On macOS (Apple silicon or Intel) or x86-64 Linux:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/JacobLinCool/rhythm-rs/main/install.sh | sh
```

The installer downloads the matching archive from the
[latest preview release](https://github.com/JacobLinCool/rhythm-rs/releases/tag/latest),
verifies it against `SHA256SUMS`, and installs `taiko` into
`$HOME/.local/bin`. It does not edit shell startup files. To choose another
user-writable destination:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/JacobLinCool/rhythm-rs/main/install.sh | TAIKO_INSTALL_DIR="$HOME/bin" sh
```

The preview release also provides a Windows x86-64 archive for manual
installation. Because `curl | sh` executes remote code, users who want to
inspect the installer first can download
[`install.sh`](https://raw.githubusercontent.com/JacobLinCool/rhythm-rs/main/install.sh),
review it, and then run it locally.

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
native Mac trackpad contact, terminal mouse click, and phone controllers all
feed the same four distinct inputs for the selected player:
`LEFT KAT | LEFT DON | RIGHT DON | RIGHT KAT`.

- **MacBook trackpad contact (macOS only):** set **Mac trackpad contact** to P1
  or P2 by pressing `Enter` on that row. The physical trackpad is divided
  horizontally into four equal zones, from left to right:
  `LEFT KAT | LEFT DON | RIGHT DON | RIGHT KAT`. Merely place a finger on a
  zone to strike; no click or pressure is read. Holding or sliding that finger
  does not repeat a strike. Lift it and touch again to re-arm it. After
  changing the assignment between P1 and P2, lift every finger once before
  playing so the new player starts from a neutral surface.
- **Terminal mouse click:** set **Terminal mouse click** to P1 or P2, then use
  the mouse's left button (or physically click a trackpad) on one of the four
  rendered drum pads. This is a separate terminal pointer source and does not
  turn ordinary trackpad contact into input.
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

Assignments are local controller slots: single-player and online play use
**LOCAL P1**; **LOCAL P2** is used only by local two-player mode. Pressing
`Enter` on either local-controller row cycles `OFF → LOCAL P1 → LOCAL P2`.

Native Mac trackpad contact uses macOS's private
`MultitouchSupport.framework`. The game probes that capability at startup. If
the framework or a compatible device cannot be opened, Controller Setup shows
the source as unsupported and does not silently fall back to mouse clicks.
Because this is a private Apple interface, a future macOS release may require
the integration to be updated.

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
- [Preview Releases and Installer](docs/developer/releasing.md)

## Quality gates

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p rhythm-mode-taiko --release bench_smoke_large_chart -- --ignored
cargo test --locked -p taiko-game --release bench::bench_smoke -- --ignored
```
