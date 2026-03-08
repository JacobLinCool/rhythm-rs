# taiko-game

Playable terminal Taiko client built on:

- `rhythm-core` deterministic runtime
- `rhythm-mode-taiko` rules/scoring
- `rhythm-importer-tja` (`tja = =0.5.0`) importer

Player manual:

- `/Users/jacoblincool/Documents/GitHub/taiko-rs/taiko-game/PLAYER_GUIDE.md`

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

## CLI

```text
taiko --songdir <PATH> [--resource-endpoint <URL>] --tps <N> --track-offset <SEC> \
      --demo <true|false> --songvol <0..100> --sevol <0..100> \
      [--resource-cache-memory-only]
taiko server --songdir <PATH> [--host <HOST>] [--port <PORT>]
taiko cache path
taiko cache list
taiko cache clear --endpoint <URL>
taiko cache clear --all
```

When `--resource-endpoint` is set, song list/chart/audio are loaded through HTTP and `--songdir` is ignored by the client.
Remote mode uses app-data disk cache by default (`charts`/`audio` keyed by content hash). Use `--resource-cache-memory-only` to disable disk cache.

## Controls

- Song Menu: type to search/filter, `Backspace/Delete` edit, `Arrow Up/Down` move, `Enter` confirm, `Ctrl+W` open load warnings
- Load Warnings: `Up/Down` scroll, `Left/Right` page scroll, `Esc`/`Enter`/`Ctrl+W` back
- Course Menu:
- `Arrow Up/Down` (or Kat key groups): select course
- `Tab/Shift+Tab`: focus setting
- `Arrow Left/Right`: adjust focused setting (Auto Play/Volumes/Note Offset/Music Offset/Scroll; offset step is `5ms` in `[-500ms, +500ms]`; scroll is cyclic and includes `V-Sync (S)`)
- `Enter`/Don: start
- Game: Don/Kat hit, `P` pause/resume, `Esc` back to Course Menu
- Back: `Esc`
- Quit: `Ctrl+C`

Don key group:

- `Space f g h j c v b n m`

Kat key groups:

- Left: `d s a t r e w q x z`
- Right: `k l ; ' y u i o , . /`

## Song Filter (Magic Words)

Search terms are space-separated and combined with AND.

- difficulty+level: `oni=9`, `oni=8,9,10`, `hard=4-7`, `oni=*`
- any level: `lvl=8-10` / `level=8-10`
- branching: `branch`, `nobranch`
- bpm: `bpm=180`, `bpm=120-180`, `bpm>=180`, `bpm<=160`
- mixed: `alice oni=9 bpm>=180`

## Branching

Branch conditions are evaluated outside core by `BranchController` and sent through `TimedControl<BranchControl>`.
Player client uses fixed policy `auto` (follow TJA hint kind `p/r/s`), not selectable in UI.

`Scroll Speed` has a special `V-Sync (S)` slot. `S` is computed per selected chart to minimize terminal-grid jitter and is constrained to `1.0 <= S <= 2.0`.

Autoplay behavior (Course Menu `Auto Play = ON`):

- auto play also plays Don/Kat hit sound effects
- roll auto rate defaults to `16 * (bpm / 120)` hits/s

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
