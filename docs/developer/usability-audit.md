# Taiko TUI Usability Audit

## Audit plan

- **Product:** the `taiko-game` terminal game.
- **Scope boundary:** normal player launch through mode selection, song/course
  selection, settings, single-player, same-terminal two-player, online
  host/create/join/spectate, countdown, gameplay, results, retry/rematch,
  recoverable failures, and exit. Dedicated-server administration is checked
  only where it affects the player hand-off. Relay, matchmaking, TLS
  termination, NAT traversal, and high availability are out of scope.
- **Target users:** a first-time player using a keyboard, two players sharing a
  wide terminal, and a player joining a private authoritative room from another
  terminal.
- **Target tasks:** start without learning player CLI flags; choose each play
  mode; find a song and course; discover or change four-key bindings; play and
  read feedback; retry after a result or preparation failure; host and join
  with one invitation; leave without accidental state loss; use a silent chart;
  recover from a missing song directory, unavailable audio device, or network
  interruption.
- **Audit goal:** broad pre-release audit of the implemented redesign, with
  emphasis on task completion and recovery rather than visual taste alone.
- **Heuristic frame:** clarity of system status, match to the player's mental
  model, recognition over recall, consistency and predictability, error
  prevention and recovery, flow continuity, and gameplay information
  hierarchy.
- **Severity model:** `high` blocks or corrupts a critical task; `medium`
  creates likely failure or difficult recovery; `low` adds recurring friction
  without blocking completion; `watch` is plausible but needs live-user
  evidence. Confidence is recorded separately.
- **Deliverable:** evidence-backed prioritized findings, cross-flow patterns,
  and concrete follow-up tests.
- **Artifacts:** this document, semantic render tests, production loopback
  tests, the real Nosferatu projection regression, and release performance
  benchmarks in the repository.

## Task surface

| Task | Entry | Success state | Recovery and exit states |
| --- | --- | --- | --- |
| Choose play mode | Normal launch | Single, local 2P, or online flow opens | Back to mode menu; offline library failure does not hide online |
| Start one-player song | Song and course menus | Countdown/gameplay with one calibrated clock | Cancel preparation, retry failure, pause, guarded leave |
| Start local 2P song | Local mode | Two independently selected courses render top/bottom | Unready, pause, guarded leave, retry result |
| Host/join online | Online connect | Authoritative lobby reached with complete invitation | Typed validation error, preparation retry, reconnect, acknowledged leave |
| Read and act on result | Result | Clear/fail, accuracy, PB and next action are understandable | Retry/rematch, details, song/lobby exit |
| Use silent chart | Empty/missing `WAVE` | Chart remains playable from a monotonic silent clock | Audio capability is shown; no guessed same-stem audio |

## Findings

### UA-01 — An unavailable audio device blocked every player task

- **Task/surface:** application startup, including silent charts.
- **Heuristic:** error containment and recovery; match to the player's mental
  model.
- **Evidence:** a production binary launched in a PTY on a machine without an
  available CoreAudio output previously returned an audio initialization error
  before drawing the mode menu. This also prevented charts without `WAVE` from
  using their otherwise independent silent clock.
- **Impact:** no mode could be selected even when the chosen task required no
  audio output.
- **Severity/confidence:** **high / high**.
- **Status and direction:** **resolved.** Audio output is now an explicit
  capability. Startup continues with a visible one-shot notice, silent charts
  use the monotonic game clock, and attempting content that requires music
  produces a typed, recoverable error. An actual PTY startup reaches the mode
  menu and restores the terminal on exit; focused audio tests cover scheduling,
  pause, seek, rate, and failed sound effects.

### UA-02 — Tempo and scroll changes could produce the wrong visual velocity

- **Task/surface:** reading the note highway in `Nosferatu`, especially the Ura
  course.
- **Heuristic:** consistency and predictability; gameplay accuracy.
- **Evidence:** the supplied gameplay capture showed notes with visibly
  inconsistent travel speed. The old projection used `SCROLL` alone, while the
  chart deliberately pairs tempo and scroll changes such as `200 × 1.26`,
  `400 × 0.63`, `300 × 0.84`, and `50 × 5.04` to preserve the same visual
  velocity.
- **Impact:** visual timing stopped matching the chart author's intent, making
  correct hits harder even though judgement time remained deterministic.
- **Severity/confidence:** **high / high**.
- **Status and direction:** **resolved.** Projection speed is derived from the
  BPM active at the note multiplied by its scroll value, relative to the
  initial BPM. Regression coverage includes mid-measure changes and a long roll;
  both the repository sample and the supplied external chart import cleanly.

### UA-03 — Side-by-side local play sacrificed the game's most valuable space

- **Task/surface:** same-terminal two-player gameplay.
- **Heuristic:** gameplay information hierarchy; fit to device and task.
- **Evidence:** the supplied local-versus capture split a very wide terminal
  into two narrow highways. Each player lost roughly half of the horizontal
  look-ahead distance while most vertical space remained unused.
- **Impact:** both players had less time to parse incoming patterns, and the
  layout felt like a diagnostic dashboard rather than a rhythm game.
- **Severity/confidence:** **medium / high**.
- **Status and direction:** **resolved.** P1 is rendered above P2; both highways
  use the full terminal width and retain independent score, gauge, judgement,
  course, and input state. The supported local-play minimum is 80×27.

### UA-04 — The primary HUD exposed implementation state instead of player state

- **Task/surface:** single-player and local gameplay, then results.
- **Heuristic:** information hierarchy; recognition over recall.
- **Evidence:** the supplied capture put tick/frame duration, replay hashes,
  internal timing offsets, and other diagnostics in the permanent HUD while
  score, gauge, progress, judgement, and controls lacked a strong game-like
  hierarchy.
- **Impact:** players had to distinguish operational telemetry from actionable
  play information during a timing-sensitive task.
- **Severity/confidence:** **medium / high**.
- **Status and direction:** **resolved.** Gameplay now prioritizes
  score/combo/best, soul gauge, judgement, progress/time, the full-width lane,
  and the player's four bindings. Replay/performance and branch diagnostics are
  progressively disclosed behind the result-details action.

### UA-05 — Play modes were a launch-time configuration rather than an in-game choice

- **Task/surface:** discovering single-player, local two-player, and online play.
- **Heuristic:** recognition over recall; flow continuity.
- **Evidence:** the earlier entry path required players to know command-line
  mode switches before entering the product.
- **Impact:** multiplayer was difficult to discover and changing modes required
  restarting with different launch knowledge.
- **Severity/confidence:** **medium / high**.
- **Status and direction:** **resolved.** A normal launch opens an in-game mode
  selector with explanations. Host-here, create, join, and spectate are
  configured inside the TUI; only independent server operation remains a CLI
  subcommand.

### UA-06 — Import assumptions made valid chart-viewing use cases look broken

- **Task/surface:** loading the bundled catalog and opening silent charts.
- **Heuristic:** match to the content model; useful error recovery.
- **Evidence:** missing/empty `WAVE`, score metadata that is irrelevant to the
  modern ruleset, and omitted balloon counts could reject otherwise playable
  charts. Guessing a same-stem audio file would also make resource identity
  depend on undeclared files.
- **Impact:** only part of the catalog appeared and a player could not reliably
  tell invalid content from supported silent content.
- **Severity/confidence:** **medium / high**.
- **Status and direction:** **resolved.** Missing or empty `WAVE` explicitly
  means a silent chart; no audio path is guessed. `SCOREINIT`, `SCOREDIFF`, and
  `SCOREMODE` are accepted but do not affect modern scoring. Missing or empty
  `BALLOON` uses the documented application default. The local catalog scan
  loads all 41 songs with zero warnings while malformed non-empty fields remain
  strict errors.

### UA-07 — Shared-keyboard input needed a physical four-key model and bounded timing

- **Task/surface:** single-player and same-terminal two-player strikes.
- **Heuristic:** consistency and predictability; error prevention.
- **Evidence:** a two-action abstraction could not preserve left/right physical
  strikes for sound, bindings, and replay. Terminal key-repeat events and an
  unbounded pending-input burst could also distort a timing-sensitive session.
- **Impact:** controls did not match a four-sensor drum mental model, and noisy
  input could degrade determinism or responsiveness.
- **Severity/confidence:** **medium / high**.
- **Status and direction:** **resolved.** Every player has left Kat, left Don,
  right Don, and right Kat bindings (defaults P1 `A/S/D/F`, P2 `J/K/L/;`).
  Side and zone survive transport and replay while all four tap variants retain
  equal judgement and score. Repeat/release events are discarded before the
  app loop, event-observation delay is removed from chart time, and each local
  player has an independent stable 512-event pending window.

### UA-08 — Preparation and destructive navigation lacked a consistent recovery contract

- **Task/surface:** async library/content preparation, leaving play, online
  reconnect, and result actions.
- **Heuristic:** visibility of system status; error prevention and recovery.
- **Evidence:** loading, retry, stale completion, and leave behavior crossed
  several screens without one explicit ownership model.
- **Impact:** a slow previous request could replace a newer choice, failures
  could force a restart, and an accidental `Esc` could discard an active run.
- **Severity/confidence:** **medium / medium-high**.
- **Status and direction:** **resolved.** Background preparation is
  latest-request-wins and generation-fenced. Recoverable errors retain a typed
  destination and retry action, gameplay leave requires confirmation, online
  countdown/reconnect state is visible, and results provide retry/rematch and a
  clear exit destination. A same-match countdown cancellation or authoritative
  start-time change also invalidates scheduled playback: the client stops the
  old song, clears audio-sync ownership, and rearms from the next authoritative
  countdown instead of allowing delayed music to start in the lobby.

### UA-09 — Player copy and width handling assumed English byte geometry

- **Task/surface:** all normal player screens at supported terminal sizes.
- **Heuristic:** accessibility; consistency and readability.
- **Evidence:** mixed hard-coded English copy, byte-based truncation, and raw
  protocol/domain labels made Traditional Chinese and Japanese presentation
  incomplete and risked broken CJK alignment.
- **Impact:** non-English players received inconsistent instructions and wide
  characters could corrupt layout boundaries.
- **Severity/confidence:** **medium / high**.
- **Status and direction:** **resolved.** Fixed copy is represented by
  exhaustive typed keys with
  English, Traditional Chinese, and Japanese variants. Dynamic sentences use
  typed messages; metadata and technical details remain source data.
  Truncation is grapheme-aware and display-column-aware. Semantic render tests
  cover 80×24, local 80×27, and 120×36 CJK layouts. Normal connection progress,
  local validation, binding conflicts, clock readiness, course controls, and
  online gameplay controls remain localized; raw protocol and device failure
  payloads appear only as technical reasons.

## Cross-flow patterns

1. **Player concepts and implementation concepts had leaked into each other.**
   Branch ticks, hashes, paths, frame timing, and launch flags are useful
   diagnostic data but poor primary controls. The redesign keeps these behind
   details while promoting mode, song, course, readiness, judgement, gauge,
   progress, and next action.
2. **A fail-closed core still needs a recoverable shell.** Content hashes,
   protocol epochs, importer grammar, preferences schemas, and audio validation
   remain strict; absence of an optional capability is represented explicitly
   rather than handled by guessing or by terminating unrelated tasks.
3. **Multiplayer shares authority and time, not player state.** Local players
   share one song clock but keep separate input queues and runtimes. Online
   players share a server-authoritative match epoch and snapshots but keep typed
   identities, sequence acknowledgement, and individually selected courses.
4. **Progressive disclosure is the appropriate HUD policy.** Immediate
   gameplay needs a small stable vocabulary; replay hashes, branch controls,
   detailed timing distributions, and network diagnostics belong in opt-in
   result/error detail views.

## Recommended next actions

These are follow-up validation tasks, not unimplemented release blockers:

1. Run a five-person moderated usability pass: one first-time keyboard player,
   one experienced Taiko player, one same-keyboard pair, and one remote pair.
   Measure time to first song, wrong-key rate, recovery without help, invitation
   completion, and whether either local player loses track of their lane.
2. Test real keyboard rollover combinations for the configured eight local
   keys on representative laptop and external keyboards. Software preserves
   simultaneous inputs, but commodity keyboard matrices can still ghost.
3. Conduct a calibrated audio/video capture against a physical display and
   audio device to tune the default offset guidance; automated clocks prove
   ordering and drift bounds, not a human's end-to-end perception latency.
4. Add an accessibility study for color-vision variants and screen
   magnification. The current semantic tests prove layout bounds, not
   legibility for every visual ability.

## Coverage and caveats

- This is an implementation-grounded expert audit, not a substitute for
  observed human usability research. Confidence reflects code, supplied
  captures, reproducible content, and automated behavior.
- Terminal UI automation uses `ratatui::TestBackend` semantic rendering and a
  real PTY startup/exit check. Browser automation is not applicable.
- Production loopback tests exercise authoritative online flows. Relay,
  matchmaking, TLS termination, NAT traversal, and high availability were
  explicitly excluded from the product scope.
- Screen readers, terminal IME composition, wide-area-network packet shaping,
  and hardware keyboard rollover have not been validated.
- Preferences intentionally have a strict schema with no legacy fallback. A
  malformed or obsolete preferences file therefore fails before the TUI and
  requires explicit repair; this is a deliberate operational trade-off worth
  monitoring with real users.

## Automation notes

- Import/catalog integration checks validate strict parsing, optional audio,
  canonical identities, and the real 41-song local corpus.
- Runtime regressions cover equal modern scoring, Go-Go as visual-only state,
  branch normalization, balloon expiration, four physical actions, bounded
  input queues, replay identity, and the `Nosferatu` visual projection.
- Semantic screen tests exercise player copy and CJK terminal geometry at the
  supported minimum and normal viewports.
- PTY verification checks that unavailable audio does not prevent startup and
  that terminal state is restored on exit.
- Production loopback tests cover room creation, invitation credentials,
  resource proof, readiness, authoritative countdown/snapshots, reconnection,
  acknowledgement, spectator state, and clean leave.
- Release-mode ignored benchmarks record tick/frame p95, p99, and maximum
  latency against explicit performance thresholds.
