# Game Mode Architecture

This document defines the player-facing mode boundary and the ownership rules
that keep offline, local two-player, and online play from leaking state into one
another.

## Entry and navigation

Normal launch always enters `Page::ModeSelect`. `GameMode` is a closed enum:

- `SinglePlayer`
- `LocalTwoPlayer`
- `OnlineMultiplayer`

The selected mode is stored separately from the current `Page`. Pages represent
navigation/rendering state; the mode selects the valid transition graph. The
song menu is reachable only from an offline mode. Online connection state is
reachable only from `OnlineMultiplayer`. Returning to the mode selector tears
down audio, pending background work, online transport, authoritative resources,
and any embedded server before clearing the active mode.

Player multiplayer commands are deliberately absent from the CLI. The only
network gameplay entry is the in-game Online Multiplayer screen. `server`
remains a CLI subcommand because it is an independently operated authority,
not a player session.

## Gameplay presentation boundary

The default gameplay surface is player-facing. Its persistent hierarchy is:

1. song and course;
2. score, combo, and soul gauge;
3. transient judgement feedback;
4. the note highway;
5. one-line input help.

Performance timing, canonical tick values, replay hashes, offsets, and aggregate
judgement counts are not live-HUD content. Aggregate results remain available
on the result screen; runtime telemetry remains internal to performance
measurement. The renderer receives derived frame presentation data from
`rhythm-mode-taiko` and does not reinterpret canonical tempo or TJA directives.
That projection includes authoritative `gogo_active` state. Go-Go transitions
take effect at their exact chart tick; when multiple transitions share a tick,
canonical event order is applied and the last transition wins.

## Input and scoring contract

Every gameplay strike is one `TaikoAction` containing both a physical side
(`Left` or `Right`) and drum zone (`Don` or `Kat`). Input mapping, single-player
queues, both local-player queues, online wire messages, the server authority,
and replay digests retain both fields. Tap judgement compares only the zone, so
either side can resolve a matching note with one strike. Big notes do not
require a left-right chord.

Tap scoring is chart-normalized from a 1,000,000-point base pool:

- expected roll and balloon points are reserved first;
- every Great tap receives the same remaining per-tap share, rounded upward to
  the next 10 points;
- the rounding remainder is not redistributed to particular notes;
- an OK tap receives half the Great value;
- each accepted roll or balloon hit receives 100 points. Roll reserve assumes
  16 hits per second; a balloon with an explicit hit count reserves that count
  times 100 points.

Physical side, note size, combo, and Go-Go state never multiply score. Combo is
still tracked for player feedback, and Go-Go remains presentation-only. A
balloon that reaches its end before its required hit count simply closes: it
does not emit a tap `Miss`, reset combo, reduce soul gauge, or award a
completion bonus.

Branch normalization never adds the mutually exclusive N/E/M routes together.
Unbranched notes always count. Within each branch segment, the reference path
selects exactly one route: greatest tap count, then greatest roll/balloon score
reserve, then lowest route id. Branches are selected independently per segment,
so the resulting score and soul-gauge denominator describes one path a player
can actually traverse.

## Local two-player ownership

`LocalMultiplayerSession` owns exactly two `LocalPlayerSession` values.

Shared state:

- one selected song and one decoded audio playback;
- one monotonic chart clock derived from that playback;
- pause state, one calibrated input-to-chart offset, scroll speed, and audio
  volumes;
- one result transition deadline.

Per-player state:

- selected course and canonical chart;
- `TaikoRuntime`, score, branch controller, replay hash, and frame projection;
- pending input queue, kept in stable tick/arrival order and capped at 512
  events per player;
- last judge and transient input/judge flashes.

The default P1 bindings (`A/S/D/F`) are routed only to player index 0; the
default P2 bindings (`J/K/L/;`) are routed only to player index 1. Each binding
maps directly to `LeftKat`/`LeftDon`/`RightDon`/`RightKat` in `TaikoAction`;
there is no intermediate side-dropping input type. Bindings are configurable
but must remain unique. Each runtime advances to the same shared tick. One
player finishing early does not stop the other runtime or the shared audio.
Results are published only after both charts and the shared audio have
finished.

The local-versus gameplay view stacks P1 above P2. Both note highways retain
the full terminal width; horizontal space is never divided between players
because look-ahead distance is the critical gameplay dimension.

This model avoids two unsafe alternatives: merging both players into one score
engine, or running two independent audio clocks that can drift.

## Online ownership

Online gameplay continues to use one server-authoritative `OnlineDomain`.
The in-game connection screen creates one of four intents:

- `Host here`: start a loopback authority on an ephemeral port, then create;
- `Create`: create on an existing authority URL;
- `Join`: parse a complete invitation as a player;
- `Spectate`: parse the same invitation as a spectator.

Embedded authority catalog construction is blocking work and therefore runs in
`EmbeddedServerStartTask`, the same bounded latest-request background-worker
model used by other UI support work. Cancelling the screen invalidates its
generation. If startup later completes after cancellation, the worker shuts
the server down instead of publishing stale state.

Once prepared, `App` owns the `EmbeddedServer` for exactly as long as the
online session. Every bootstrap failure, connection failure, normal leave,
error transition, application shutdown, and cancellation path either reports
the shutdown error or completes `shutdown_and_join`; there is no detached
successful authority path.

The embedded UI authority is loopback-only. Cross-machine LAN/Internet play
uses the independently operated `server` command so interface binding, stable
ports, TLS termination, firewalls, and NAT remain explicit deployment
responsibilities.

## Reliability evidence

The automated suite verifies:

- closed three-mode selection and removal of the player multiplayer CLI;
- disjoint local-player input mapping and ready-state locking;
- one player input cannot affect the other runtime, score, or replay hash;
- embedded hosting starts off the UI thread;
- embedded authority uses loopback and an ephemeral port and shuts down cleanly;
- full production HTTP/WebSocket two-player matches;
- command/input acknowledgement loss, resume, actor identity, and old-transport
  fencing;
- graceful acknowledged leave and retry;
- strict invitation, resource identity, chart, audio, clock, and result
  invariants.

The authority remains a single in-memory process. It does not provide durable
rooms, replication, failover, matchmaking, relay, TLS termination, or NAT
traversal. These are explicit system boundaries rather than fallback paths.
