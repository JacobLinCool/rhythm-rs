# Multiplayer Guide

Taiko multiplayer uses one authoritative server for rooms, timing, accepted
inputs, scores, and final results. It is not lockstep peer-to-peer play.

## Choose how to host

### Embedded host

Start the normal game, choose `Online Multiplayer`, then choose `Host here`.
The game binds an authoritative server to `127.0.0.1` on a random free port,
creates a room, and connects the host as its first player. This is designed for
two or more game processes on the same computer and needs no player CLI
arguments.

The embedded server is the network authority, while the room leader is a room
role. Leadership can move to another player after a disconnect, but closing the
host process or leaving online mode shuts down every room on that embedded
server. For LAN or Internet reachability, use a dedicated server.

### Existing or dedicated server

An operator can keep the authoritative resource and multiplayer server running
independently:

```bash
# On the server machine.
cargo run -p taiko-game --release -- server \
  --songdir ./taiko-game/songs \
  --host 0.0.0.0 \
  --port 4150

```

On a player machine, start the normal game and select `Online Multiplayer` →
`Create`. Enter `http://192.168.1.20:4150` and a display name in the TUI.

Use a dedicated server when the room must survive the room creator leaving.
Rooms and resume tokens are currently process memory: restarting the server
ends them.

## Share the complete invitation

After creating a room, the lobby shows one invitation similar to:

```text
taiko://join?server=http%3A%2F%2F192.168.1.20%3A4150%2F&room=ABCD&token=<64-hex-token>
```

The invitation contains all three required values: the server endpoint, room
code, and invitation token. Share the complete string. A room code by itself
is not enough, and manually rebuilding the URL risks corrupting its escaping.
Treat the invitation as a secret capability: anyone who has it can attempt to
enter the room until its capacity is reached.

Players and spectators use the same invitation. In the normal game, choose
`Online Multiplayer` → `Join` or `Spectate`, paste the complete invitation, and
enter a display name. `Join` occupies a player slot; `Spectate` is read-only
and occupies a spectator slot. A room supports up to four players and 64
spectators. Spectators connect directly to the multiplayer WebSocket and do not
download the HTTP song library, charts, or audio.

## LAN, Internet, TLS, and NAT

- `127.0.0.1` is reachable only from the same computer.
- A LAN dedicated server normally binds `0.0.0.0`; players enter a concrete
  reachable address such as `http://192.168.1.20:4150`, and the host firewall
  must allow that TCP port.
- The built-in server speaks plaintext HTTP and WebSocket. For an Internet
  deployment, put it behind a reverse proxy that terminates TLS and forwards
  both HTTP resources and WebSocket upgrades. Give players the public
  `https://` base URL; the client derives `wss://` automatically.
- There is no built-in relay, matchmaking service, UPnP, hole punching, or NAT
  traversal. A home Internet host must arrange a publicly reachable address
  and port forwarding, or use a reachable dedicated server.
- Do not expose a plaintext `http://` authority across an untrusted network.
  The invitation token and game traffic otherwise travel without transport
  encryption.

The reverse proxy must preserve the server's base path, if one is used, and
forward at least `/v1/library`, `/v1/charts/`, `/v1/audio/`, and
`/v2/multiplayer/ws`.

## Room and match flow

1. A creator starts a room and shares its invite.
2. The leader selects a song.
3. Each player selects a course. Branch policy is not selectable network state:
   client and server both derive deterministic `Automatic`, and the server
   excludes charts that policy cannot run.
4. Selecting a course records ready intent. Each client downloads and verifies
   the exact immutable chart and, when present, audio named by the server
   manifest, compiles the chart, and fully decodes present audio within hard
   gameplay limits.
5. The client sends ready automatically only after that content work and both
   clock views are ready. When every online player is ready, the leader starts
   the countdown. A ready player in reconnect grace is not considered online,
   so the start control remains disabled. `r` can still unready, ready, or retry
   a failed preparation.
6. Clients judge locally for immediate audiovisual feedback, but the server
   decides which timestamped inputs are accepted and runs the authoritative
   scoring engines.
7. Live scores and immutable final results come from the server. Clients cannot
   upload a score or final result. A player finalized as DNF is displayed as
   `DNF`, never as a generic failed clear.
8. From the result screen, the leader can start a rematch with a new match
   epoch or return the room to the lobby.

Spectators receive room and live-state updates but cannot select content,
become ready, start a match, or submit gameplay input.

## Disconnect and resume

The client stores the server-issued actor identity and resume token in memory
and reconnects automatically. The reconnect policy permits at most 32
consecutive unhealthy connection attempts, with exponential delay from 250 ms
to 5 seconds. Merely receiving `Welcome`, briefly joining, or repeatedly
flapping does not replenish that budget. It resets only after the transport has
held authoritative membership for at least 30 seconds and then receives a
`HeartbeatAck`; this lets a genuinely recovered long session survive later
outages without giving an unstable endpoint an infinite retry loop.

A resumed connection receives the latest authoritative room snapshot and
command/input acknowledgements; an older connection for the same actor is
fenced out. Clock-quality evidence is scoped to the old transport and is
deliberately cleared on resume.

New transports must complete `Hello` within 10 seconds and create, join, or
resume a room within 30 seconds. A transport left unaffiliated after
`LeaveRoom` receives a new 30-second admission window. Missing either deadline
produces a retryable `SessionExpired` error and closes that transport.

The server retains a disconnected player's slot only during the reconnect
grace period (currently 15 seconds). If the player resumes in time, their room
identity and sequence state continue. After grace expires:

- a lobby/preparation slot is reclaimed;
- an active-match player becomes DNF;
- a stale resume token is rejected.

Reconnect cannot recover from the authoritative server process stopping,
losing network reachability for longer than the grace period, or the room
being closed. Keep the complete invitation if a fresh join may be needed, but
a fresh join is a new room identity rather than a resume.

Normal quit is acknowledgement-based: pending commands are sent in order,
`LeaveRoom` is sent last, and the client waits for its server
`CommandAck` before sending the WebSocket close frame. If the client is already
disconnected but still believes it has active membership, it cannot prove that
the authority observed the leave and reports that explicitly. The server's
15-second grace/DNF rules remain the authority in that case.

## Authority and content integrity

The server owns membership, leader election, room revisions, match epochs,
deadlines, accepted input order, live scores, and final results. Control
commands and input batches carry monotonic sequence numbers, so duplicates are
idempotent and stale room or match messages are rejected.

The input watermark means “processed,” not necessarily “scored.” A contiguous
input that arrives too late, too far ahead, above the physical rate envelope,
or after the player has finished is acknowledged as dropped and never enters
the scoring engine. Advancing past that dropped sequence lets later legitimate
input continue after an acknowledgement loss or reconnect. Gaps are retried;
conflicting duplicates and malformed timelines remain terminal.

Before readying, the server checks each selected course against the current
assignment and runs the authority-derived `Automatic` branch policy. Every
client submits a proof binding its
source-chart content ID, canonical chart, required-nullable audio content ID,
canonical schema, importer, ruleset, and audio-decoder semantics to the server
manifest. The source ID, and the audio ID when present, are strict lowercase
SHA-256 values; the multiplayer model has no second duplicate hash field that
could disagree. This prevents two players from silently playing different
content while appearing to share a match.

The authority and clients execute the same pinned `TaikoRuntime`: balloon hits
are worth 100 points without a completion bonus, an incomplete balloon is not
a tap miss, and branch score/gauge normalization includes only one playable
route per segment rather than summing mutually exclusive N/E/M notes.

Clock readiness is separate from that content proof. The client derives a
server-time estimate from four-timestamp exchanges, while the server issues
one-time challenge tokens and measures receipt timing on the current
connection. Readying requires at least four fresh server-observed samples,
p95 path time at or below 250 ms, and p95 jitter at or below 100 ms in the
10-second evidence window. A reconnect must establish new evidence, so a player
may briefly remain “not ready” while probes accumulate.

This is a connection quality check, not hardware-clock attestation or input
anti-cheat. The server still relies on input ordering, lateness limits, its own
deterministic simulation, and server-generated results for game authority.

Chart and present-audio downloads are content-addressed and verified before
use. A null audio identity starts gameplay on the monotonic silent clock and
does not issue an audio request.
Server startup and client preparation use the same fail-closed decoder limits:
exact standard mono/stereo layouts only, 8–96 kHz, at most 15 minutes, at most
256 MiB encoded, and at most 256 MiB of decoded stereo frames. Unsupported,
corrupt, non-finite, format-changing, overlong, or decompression-heavy audio
cannot pass the ready barrier. The decoder version/features, limits,
conversion, cancellation boundaries, and failure policy are also committed by
one semantic SHA-256 fingerprint, so differing decoder builds cannot silently
ready together.

Ordinary transient transport/body failures use at most three consecutive
attempts, with 1-second and 2-second waits. The first HTTP `503` starts a
separate admission-busy window: from that point the fetch is bounded by both
315 seconds and 16 total requests, including requests that subsequently fail
at the transport layer. Busy backoff is 1, 2, 4, 8, 16, and then at most 30
seconds, with bounded additive jitter; `Retry-After` is a minimum wait but is
itself capped at 30 seconds. A non-`503` HTTP error fails closed. Connecting
may take at most 5 seconds and receiving response headers at most 305 seconds,
which covers the server's size-aware verification deadline of at most five
minutes. A body may not remain idle between chunks for more than 10 seconds.
Its absolute deadline mirrors the server's size-aware budget
(`10 seconds + ceil(Content-Length / 1 MiB/s)`, capped at five minutes) with
five seconds of client-side transport tolerance. When `Content-Length` is
absent, the client uses that endpoint's hard payload limit for the calculation.
Song/course changes and shutdown cancel the in-flight request, body reads,
retry waits, and audio decode instead of leaving obsolete preparation work
running. The server budget intentionally assumes at least 1 MiB/s of sustained
body progress; slower links are not guaranteed to complete large assets before
the deadline.

The on-disk remote cache uses one v3 index and a global 2 GiB/8,192-entry
budget across all endpoints. Deterministic access-LRU eviction keeps the total
bounded; every hit is rehashed, and a missing, corrupt, or bit-rotted blob is
removed and fetched again. Use `taiko cache path`, `taiko cache list`,
`taiko cache clear --endpoint <URL>`, or `taiko cache clear --all` to inspect
or clear it. Online **players** use this disk cache by default; resource-free
spectators do not open it. The global `--resource-cache-memory-only` option on
normal game startup disables disk caching for authority catalogs and match
resources.

The server permits 16 chart/audio streams globally and eight per client IP,
with a separate 512 MiB aggregate private-snapshot budget. Each file is copied
and revalidated from one opened descriptor, then streamed only from that
snapshot in 64 KiB chunks. Stream/snapshot admission exhaustion or temporary
storage pressure returns HTTP `503` with `Retry-After: 1`. Verification and
body transfer each use a 10-second base plus content length at 1 MiB/s, capped
at five minutes. An independent bounded producer owns all reservations, so its
deadline continues even when an HTTP consumer stops polling the response body.
Completion, timeout, read failure, or body drop releases every reservation.
Chart and library responses are limited to 16 MiB and encoded audio to 256
MiB. The client-IP key is the listener's TCP peer; users behind a reverse proxy
that connects from one address share its eight-stream budget.

Catalog startup is also fail-closed and sequential: it accepts at most 4,096
TJA files. Before the third-party parser allocates note data, the shared
importer validates a strict known syntax, finite numeric domains, exact balloon
tables, branch and roll pairing, and decoded source shape, including 16 MiB of
raw input, 16 courses, note symbols, segments, branches, and metadata values.
It compares course, segment, note, and balloon counts exactly with the pinned
parser output, rather than accepting anything that parser silently ignored or
defaulted. Canonical construction then enforces fallible per-course and
aggregate budgets; its effective object ceilings are 131,072 per course and
262,144 per import.

Before authority retention, the server independently measures at most 8 MiB of
canonical JSON per course and 64 MiB across the catalog, with additional
250,000-per-course and 1,000,000-catalog object guards. Exceeding an aggregate
deployment bound aborts startup rather than silently advertising an authority
it cannot safely retain. An individual invalid song is omitted with a bounded
warning.

## Deployment boundary

Rooms, actor identities, resume credentials, and live match state are held in
one authority process. There is no persistence, replication, failover, or
cross-node room migration; restarting or losing that process ends its rooms.
The built-in listener also does not provide TLS, matchmaking, relay, port
forwarding, or NAT traversal. Those are operator/network responsibilities, not
hidden fallback paths in protocol v2.

For the wire-level contract, see
[Multiplayer Protocol v2](developer/multiplayer-protocol-v2.md). For network
fault coverage, see
[Multiplayer Reliability Testing](developer/multiplayer-testing.md).
