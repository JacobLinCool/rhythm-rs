# taiko-game

Playable terminal Taiko client built on:

- `rhythm-core` deterministic runtime
- `rhythm-mode-taiko` rules/scoring
- `rhythm-importer-tja` (`tja = =0.5.0`) importer

Player manual:

- [PLAYER_GUIDE.md](PLAYER_GUIDE.md)

## Run

```bash
cargo run -p taiko-game --release -- \
  --songdir ./taiko-game/songs
```

Remote resource mode:

```bash
cargo run -p taiko-game --release -- server \
  --songdir ./taiko-game/songs --host 127.0.0.1 --port 4150

cargo run -p taiko-game --release -- \
  --resource-endpoint http://127.0.0.1:4150/
```

Normal launch opens an in-game selector for Single Player, Local Two Player,
and Online Multiplayer. Online Host/Create/Join/Spectate are all configured
inside the TUI. Only an independently operated server uses a CLI subcommand.
For deployment, TLS/NAT limits, reconnect behavior, and the
server-authoritative match flow, see the
[Multiplayer Guide](../docs/multiplayer.md).

## CLI

```text
taiko --songdir <PATH> [--resource-endpoint <URL>] --tps <N> \
      --calibration-offset-ms <-500..500> \
      --demo <true|false> --songvol <0..100> --sevol <0..100> \
      [--resource-cache-memory-only]
taiko server --songdir <PATH> [--host <HOST>] [--port <PORT>]
taiko cache path
taiko cache list
taiko cache clear --endpoint <URL>
taiko cache clear --all
```

When `--resource-endpoint` is set, song list/chart/audio are loaded through HTTP and `--songdir` is ignored by the client.
Remote mode uses an app-data disk cache by default (`charts`/`audio` keyed by
content hash), with one v3 index and one global 2 GiB/8,192-entry LRU budget
across endpoints. Cache hits are rehashed and corrupt blobs are discarded and
downloaded again. Use `--resource-cache-memory-only` to disable disk cache.

In Online Multiplayer, `Host here` starts a loopback authoritative server owned
by the game and creates a room. `Create` uses an already-running server.
Joiners and spectators paste the complete invitation containing the server,
room, and secret token; a room code alone is insufficient. Spectators connect
without fetching the HTTP song library or match assets. An empty or missing
offline song directory remains a visible offline error but does not block the
Online Multiplayer mode.

## Controls

- Mode menu: `Up/Down` selects a mode, `Enter` confirms, and `S` opens
  persistent player settings.
- Settings: `Up/Down` or `Tab/Shift+Tab` moves; `Left/Right` adjusts;
  selecting a binding and pressing `Enter` captures one new key. The language,
  volumes, one input-to-chart calibration, scroll speed, preview, online name,
  and both players' four keys are saved atomically.
- Song menu: type to filter; `Backspace/Delete` edits; `Up/Down` moves;
  `Enter` confirms; `Ctrl+W` opens load warnings; `Esc` clears an active
  filter, then returns to modes once the filter is empty.
- Course menu: `Up/Down` selects a course; `Tab/Shift+Tab` focuses an
  adjustment; `Left/Right` changes it; `Enter` starts.
- Single-player game defaults: `A=Left Kat`, `S=Left Don`, `D=Right Don`,
  `F=Right Kat`; `P` pauses; `Esc` asks before abandoning the run.
- Local course selection: P1 `W/S` + `F`; P2 `Up/Down` + `J` or `Enter`.
- Local game defaults: P1 uses `A/S/D/F`; P2 uses `J/K/L/;`. Both players have
  four independent physical strikes. `P` is shared pause and `Esc` is a guarded
  leave action.
- Results: `Enter` retries/rematches, `Esc` returns to songs, and `D` toggles
  replay/performance details.
- Online: choose Host here/Create/Join/Spectate and fill every field in the TUI.
- Quit: `Ctrl+C`.

Local gameplay is stacked vertically: P1 above P2, with a full-width note
highway for each player.

## Song Filter (Magic Words)

Search terms are space-separated and combined with AND.

- difficulty+level: `oni=9`, `oni=8,9,10`, `hard=4-7`, `oni=*`
- any level: `lvl=8-10` / `level=8-10`
- branching: `branch`, `nobranch`
- bpm: `bpm=180`, `bpm=120-180`, `bpm>=180`, `bpm<=160`
- mixed: `alice oni=9 bpm>=180`

## Chart and rules policy

Missing or empty `WAVE` means the chart is intentionally silent. The client
does not guess a same-stem audio file; silent charts remain playable from the
deterministic monotonic game clock. If the operating system has no audio output,
the game still reaches the mode menu and explains that only silent content is
available.

The player client always uses the official automatic TJA branch policy; branch
policy is not a player setting. Modern scoring is chart-normalized and gives
every judged tap the same score value. Big and small notes have identical
judgement and scoring, Go-Go is visual-only, and there is no combo multiplier.
The four physical sides remain distinct for bindings, sound, networking, and
replay identity.

`Scroll Speed` has a special `Velocity Sync (S)` slot. `S` is computed per selected chart to minimize terminal-grid jitter and is constrained to `1.0 <= S <= 2.0`.

Autoplay behavior (Course Menu `Auto Play = ON`):

- auto play also plays Don/Kat hit sound effects
- roll auto rate is fixed at `16` hits/s across tempo changes, matching the ruleset's roll-score reserve

## Color Policy

Color mode is automatic:

- default: color enabled (`Taiko Vivid`)
- if `NO_COLOR` exists and is non-empty: color disabled

Gameplay color semantics:

- hit-zone base flashes by judge only: `Great=Yellow`, `OK=White`, `Miss=Blue`
- input feedback flashes on marker only (`|`, `◎`, `|`), duration `200ms`
- `RollHit` does not trigger hit-zone judge flash
- gauge line uses a dynamic-width bar with `PASS(difficulty/level dependent)` and `FULL(100%)` markers
- result screen includes timing distribution (horizontal violin-like plot in `ms`, early/late centered at `0ms`)

Disable color example:

```bash
NO_COLOR=1 cargo run -p taiko-game --release -- --songdir ./taiko-game/songs
```

## Benchmark

Run taiko-game benchmark smoke:

```bash
cargo test -p taiko-game --release -- --ignored bench_smoke
```

The benchmark uses:

- chart: `taiko-game/samples/Nosferatu.tja`
- autoplay: enabled
- logic rate: `240 TPS`
- terminal backend: `ratatui::TestBackend`

Thresholds asserted in test:

- tick p95 `< 2.0ms`
- frame p95 `< 8.3ms`
- TPS capacity `>= 500`
- FPS `>= 120`
