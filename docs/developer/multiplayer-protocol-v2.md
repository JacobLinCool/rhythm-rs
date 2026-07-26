# Multiplayer Protocol v2

This document defines multiplayer protocol version `2`, including its strict
wire contract, authority boundaries, bounded state, and player-visible
lifecycles.

The current strict wire fingerprint is
`0792569ac7ed14846ad8bfcff1b7568b917505b34997df668db641925f437257`.
Version 2 has one wire shape. A peer with version 1, a missing fingerprint, or
a different fingerprint is rejected; there is no compatibility parser or
fallback route.

The only multiplayer endpoints are `WS /v2/multiplayer/ws` and
`GET /v2/multiplayer/healthz`. Version 1 multiplayer paths are not aliases.

## Product contract

- The server is authoritative for membership, room revisions, match epochs,
  phase deadlines, accepted inputs, score snapshots, and final results.
- The room leader selects the song. Every player selects only their own course.
  Branch policy is not client-selectable protocol state: both local preparation
  and the authority derive the deterministic `Automatic` policy, and startup
  excludes any course that policy cannot run.
- A room can play multiple matches. Every leader song selection or rematch
  creates a new `MatchId`; messages from an older match are rejected.
- A WebSocket session is transport only. Player identity survives a temporary
  disconnect through a server-issued resume token and reconnect grace period.
- Clients judge locally for immediate feedback. The server validates timestamped
  input and advances its own deterministic player engines behind a bounded
  lateness window.
- Every drum input contains both physical `side` (`left` or `right`) and
  `zone` (`don` or `kat`). The side remains part of the authoritative input and
  replay digest while judgement matches the zone.
- Reliable control messages and coalesced live-state messages use separate
  bounded delivery paths. A slow consumer never creates an unbounded queue.
- Charts and present audio are immutable match inputs identified directly by
  lowercase SHA-256 content digests; audio absence is a required null rather
  than a filename guess. There is no second opaque resource ID or duplicate
  hash field that can disagree. A match also identifies canonical-chart,
  importer, taiko-ruleset, and audio-decoder semantics.

## Room state machine

```text
Lobby
  └─ Leader selects song
       └─ Preparing(match_id)
            ├─ Leader changes song ─────────────┐
            ├─ Leader returns to lobby          │
            └─ Every online player is ready     │
                 + leader starts                │
                    └─ Countdown(deadline)      │
                         ├─ player lease expires│
                         │    └─ Preparing ──────┘
                         └─ Playing
                              └─ Finalizing
                                   └─ Finished
                                        ├─ Rematch ──> Preparing(new match_id)
                                        └─ Return ───> Lobby
```

`RoomSnapshot.stage` is an enum carrying the data required by each state, so
illegal combinations such as `Lobby` with a start deadline are not
representable.

## Preparation barrier

A player becomes ready only when all of the following are true:

1. The command references the current `MatchId`.
2. The selected `CourseId` exists in the current song manifest, whose canonical
   chart was already admitted under the authority-derived `Automatic` branch
   policy.
3. Source-chart ID, required-nullable audio ID, canonical chart, and
   canonical/importer/ruleset/audio-decoder semantic versions and digests match
   the server manifest.
4. The chart has compiled and, when audio is present, the complete stream has
   decoded locally within the shared encoded-size, sample-rate, channel,
   duration, frame, and decoded-memory limits. Audio absence instead prepares
   the monotonic silent clock. Entering `Playing` does not perform a first-time
   audio decode on the UI thread.
5. The client has at least four accepted four-timestamp samples for scheduling
   server deadlines, and the server has fresh challenge-response evidence that
   the current transport path satisfies its quality policy.

Changing song or course clears readiness. A stale ready command is rejected; it
is never reinterpreted for the current match.

Course selection records client ready intent, but it is not itself a ready
transition. The client automatically sends `SetReady { ready: true, proof }`
only after all five conditions above hold. Players may explicitly toggle
ready/unready with `r`; after a local preparation failure, `r` or confirming
the same selection with `Enter` clears the failure latch and starts one new
preparation attempt. A failed identity is otherwise latched and is not retried
in a background loop.

### Clock evidence and trust boundary

Clock synchronization has two distinct views:

1. The client sends `TimeSyncRequest { nonce, client_send_us }`.
2. The server records the issue instant, generates an unpredictable one-time
   `probe_token`, and returns it with the echoed client timestamp and server
   receive/send timestamps.
3. The client immediately returns
   `TimeSyncReceipt { nonce, probe_token }`. The token and nonce must name a
   pending challenge in that exact transport session; forged, unknown, or
   replayed receipts are rejected.
4. The server measures challenge-to-receipt time with its own monotonic clock
   and returns `ClockProbeAck`. Readiness requires at least four samples spaced
   at least 100 ms apart in the current 10-second evidence window, p95 observed
   path time no greater than 250 ms, and p95 jitter no greater than 100 ms.
   The acknowledgement carries the absolute server-time expiry of the newest
   still-ready sample suffix; the client must stop treating an expired
   acknowledgement as ready.
5. Independently, the client uses the four timestamps to estimate server-clock
   offset, reject malformed or extreme samples, and slew its local estimate
   instead of discontinuously jumping it.

The preparation proof contains content and semantic digests only. It does not
contain client-reported clock quality. The room actor checks fresh
server-measured evidence when serializing `SetReady` and rechecks every
player's lease when applying `StartMatch`. Every new transport session,
including a resumed player, clears the old evidence and must establish fresh
evidence.

This evidence is a quality-of-service admission check for the currently
observed response path. It does not attest the client's hardware clock, prove
the truth of client timestamps, or provide input anti-cheat. Input sequence,
lateness, deterministic server simulation, and server-generated results are
separate authority controls; they also do not turn an untrusted client into a
trusted execution environment.

## Wire state ownership

Protocol fields are present only when they carry independent state:

- `Welcome` carries the negotiated version and schema fingerprint, heartbeat
  and reconnect intervals, whether this connection resumed an identity, and
  the next expected command sequence. A transport-local server session ID is
  not player identity and is therefore not exposed. `Welcome` also carries no
  clock timestamps; the challenge-response time-sync exchange is the sole
  clock-evidence path.
- `MembershipGranted.actor_id` is the only source of membership kind:
  `ActorId::Player` means player and `ActorId::Spectator` means spectator.
  There is no second `role` field that could disagree with the tagged actor
  identity.
- `Heartbeat` and `HeartbeatAck` echo only a nonce. Room and input progress
  come from authoritative snapshots and acknowledgements, not unused
  client-reported heartbeat watermarks; heartbeat acknowledgements are lease
  evidence, not a second server-clock sample.
- `PlayerSnapshot.last_acked_input_seq` is optional. `None` means that no input
  has been processed, while `Some(n)` always contains a sequence at or above
  `FIRST_INPUT_SEQ`. Zero is rejected rather than interpreted as a sentinel.
  `ResumeRequest` deliberately does not carry this match-scoped watermark:
  after resume, the mandatory authoritative snapshot reconciles the current
  match epoch, pending inputs, and next input sequence. This prevents a lost
  rematch/song-selection snapshot from applying an old match watermark to the
  new match.
- Before applying state, the client checks `Welcome.resumed` against whether it
  already owns membership, requires the handshake command watermark to be
  nonzero and within its acknowledged/sent evidence, and checks the first or
  resumed `MembershipGranted` against the requested room, role, actor identity,
  invitation token, and resume token. Every accepted room snapshot must contain
  that granted local actor. Contradictory context is terminal rather than being
  reconciled as a retry.

## Ordering and idempotency

- Every control command carries a monotonically increasing `CommandSeq`.
  Global room mutations (song selection, start, rematch, and return) also carry
  the expected `RoomRevision`. Player-scoped preparation/course/ready commands
  instead bind to actor identity and `MatchId`; requiring the global revision
  there would create false conflicts between independent players.
- The server returns a `CommandAck` for every command. Exact duplicate commands
  within the bounded replay window return the original outcome without applying
  twice. A future `SequenceGap` does not consume or enter that replay window,
  and an old duplicate never regresses the session watermark.
- Before graceful shutdown snapshots its pending batch, the client binds every
  unsent global mutation to the latest authoritative revision it actually
  knows. Multiple queued mutations may therefore stale-reject in sequence, but
  none may bypass optimistic concurrency with an absent revision.
- `LeaveRoom` is terminal for membership but not for the still-open transport
  session. Once applied, the room removes the actor and revokes its resume
  credential; the registry retains the exact command and original
  `CommandAck` in its bounded session cache before clearing that session's
  membership. If only the ACK is lost while the same session remains open, the
  client resends the identical envelope with the same `CommandSeq` and receives
  that original ACK; reusing the sequence with different contents is rejected.
  If the transport instead disconnects after application, the revoked
  membership cannot be resumed and the lost session-scoped ACK cannot confirm
  the result. The authoritative state still reflects that the actor left, but
  the protocol does not promise exactly-once confirmation across that
  disconnect.
- Inputs carry `MatchId` and monotonically increasing `InputSeq`. Every event's
  action is the strict object `{ "side": "left" | "right", "zone": "don" |
  "kat" }`; both fields are required and unknown fields or the former
  Don/Kat-only string shape are rejected.
- The server processes only the contiguous input prefix and returns an
  `InputAck` containing the processed watermark, next expected sequence, and a
  typed accepted/rejected outcome. A late, too-far-ahead, rate-limited, or
  post-finish event is consumed as a dropped input without entering the scoring
  engine; this keeps a lost acknowledgement from deadlocking the sequence.
  Exact retries are idempotent. Sequence gaps consume nothing and can be
  retransmitted after reconnect, while conflicting duplicates and structurally
  invalid timelines are terminal protocol violations.
- Snapshots carry `RoomRevision`; older snapshots are ignored.
- Final results are server generated and immutable. Their replay digest commits
  to the match/song/semantic/player-assignment context and the ordered inputs
  accepted into authoritative scoring, including both side and zone for every
  strike. Attempts consumed as dropped are deliberately excluded, so an
  acknowledgement retry after a player has finished cannot mutate an already
  published result.

## Delivery and limits

- Per-session reliable queue: bounded; overflow closes the slow session with a
  typed reason.
- The authority admits at most 512 concurrent WebSocket sessions and 256 live
  rooms. Each session has a 64-message/second token bucket with burst 128;
  exceeding it closes that session with a typed rate-limit error.
- A transport must send a valid `Hello` within 10 seconds and create, join, or
  resume a room within 30 seconds of admission. Leaving a room starts a fresh
  30-second unaffiliated deadline. Expiry sends a retryable, typed
  `SessionExpired` error and closes the transport.
- Live score state: latest-value coalescing at 20 Hz by default.
- Input batches: bounded by the protocol constant. The client also bounds its
  pending command window to 64 and its pending input window to 512. Repeated
  unsent song/course/ready intent is deduplicated or coalesced. When 512 input
  acknowledgements are outstanding, a new local strike is explicitly dropped
  without allocating a sequence number or ending the session.
- Accepted player timestamps use an authoritative token bucket: an initial
  four-strike burst, then 50 strikes per second. A batch that exceeds the
  envelope is atomically excluded from scoring and its contiguous sequence is
  acknowledged as dropped, so same-tick roll-score injection cannot amplify
  score or spectator broadcasts and cannot wedge later legitimate input.
- Display names, build identifiers, errors, and vectors have protocol limits
  enforced before entering a room actor. Bounded sequences use streaming
  deserialization and reject the first element beyond their maximum instead of
  allocating an unbounded intermediate vector.
- Heartbeats renew a session lease. Missing heartbeats move a player to
  `Reconnecting`. The session lease is 8 seconds and the reconnect grace is 15
  seconds; grace expiry reclaims an inactive slot or makes an active player
  DNF.
- Room phase deadlines are driven by monotonic server timers, never by unrelated
  client traffic.
- The companion HTTP resource service permits 16 chart/audio transfers in
  total and eight per client IP, plus a 512 MiB aggregate private-snapshot
  budget charged in 64 KiB units. It copies and validates each selected file's
  size and digest from one open descriptor, then streams only the immutable
  snapshot in 64 KiB chunks. Stream/snapshot exhaustion or temporary-storage
  pressure receives HTTP `503` with `Retry-After: 1`. Verification and body
  transfer each use `10 seconds + ceil(content_length / 1 MiB/s)`, capped at
  five minutes. A bounded producer task, rather than consumer polling, owns all
  reservations and the body deadline. Completion, deadline, error, or body drop
  releases every reservation. Chart and library documents are capped at 16 MiB
  and encoded audio at 256 MiB.
- Ordinary transient transport/body failures make at most three consecutive
  attempts with 1- and 2-second waits. The first HTTP `503` starts an
  admission-busy budget: from that point the fetch is limited to 315 seconds
  and 16 total requests, including intervening transport failures. Busy waits
  use 1, 2, 4, 8, 16, then at most 30 seconds plus bounded additive jitter;
  `Retry-After` is a minimum capped at 30 seconds. Other HTTP errors fail
  closed. Connect timeout is 5 seconds and response-header timeout is 305
  seconds, covering the server's size-aware five-minute verification maximum.
  Body idle time is 10 seconds, and the absolute body deadline mirrors the
  server's size-aware 1 MiB/s budget plus 5 seconds of transport tolerance.
  Cancellation drops the active request during body reads or retry waits, and
  audio decode uses the same cancellation signal. Links below 1 MiB/s are not
  guaranteed to complete large assets before the server deadline.
- The default disk cache has one v3 index and a global 2 GiB/8,192-entry quota
  shared across endpoint directories. Access-LRU eviction is deterministic.
  Cache hits are size-checked and rehashed; corrupt, missing, unindexed, or
  otherwise inconsistent blobs are removed, not trusted through a fallback
  mapping.
- Authority startup scans charts sequentially and fails closed above 4,096 TJA
  files. Before the third-party parser allocates note data, importer v3
  enforces its pinned source grammar, numeric domains, branch/roll/balloon
  semantics, and hard limits. It then requires exact course, segment, note,
  and balloon reconciliation with parser output, so parser-side ignore/default
  behavior cannot change accepted data. Canonical objects are capped at
  131,072 per course and 262,144 per import with fallible growth. Before
  catalog retention, the authority also
  limits canonical JSON to 8 MiB per course and 64 MiB in aggregate, with
  independent 250,000-per-course and 1,000,000-catalog object guards.

## Player flows

### Embedded authority

1. The player starts the normal game and selects `Online Multiplayer` →
   `Host here`.
2. The process binds a loopback authoritative server on a random free port,
   connects the host, and shows one `taiko://join?...` invite containing the
   endpoint, room code, and invitation token.
3. Other game processes on the same machine select `Online Multiplayer` →
   `Join` and paste the complete invite.
4. The network authority and room leader are distinct roles. Leadership can
   migrate while a server remains alive; terminating the embedded authority
   process ends every room on it.
5. LAN and Internet hosting use a dedicated server; the embedded UI host is
   deliberately loopback-only.

### Dedicated server

1. An operator runs `taiko server --songdir <PATH> --host <HOST> --port <PORT>`.
2. A player starts the normal game, selects `Online Multiplayer` → `Create`,
   and enters the server URL and display name.
3. The creator shares the complete emitted
   `taiko://join?server=...&room=...&token=...` invite. The room code alone is
   insufficient. The invitation is a secret capability.
4. Players choose `Join`; spectators choose `Spectate`; both paste the same
   complete invite in the TUI.

The built-in authority speaks plaintext HTTP/WebSocket. Internet deployments
terminate TLS at a reverse proxy that forwards both resource requests and
WebSocket upgrades; clients given an `https://` endpoint derive `wss://`.
There is no built-in relay, matchmaking, UPnP, hole punching, or NAT traversal.
Rooms and resume credentials are process memory, so an authority restart ends
them.

The production player surface has one TUI adapter over `OnlineDomain`; there is
no second multiplayer state machine or player CLI. A headless driver exists
only under `cfg(test)` to exercise the same production transport/domain in
reliability tests.

## Reconnect and deployment boundary

The default client allows at most 32 consecutive unhealthy connection attempts
with exponential delay from 250 ms to 5 seconds. `Welcome` and brief membership
do not reset this budget. A connection becomes stable only after it has held
`MembershipGranted` for at least 30 seconds and subsequently observes a
`HeartbeatAck`; only that evidence resets both the attempt counter and backoff.
This separates recovery after a healthy session from endless retry against a
flapping endpoint.

Rooms and resume credentials are process-memory state under one authority.
Protocol v2 does not define durable storage, high availability, replication,
failover, or cross-authority migration. The built-in listener is plaintext and
does not implement TLS termination, matchmaking, relay, UPnP, hole punching,
or NAT traversal.

## Verification scope

Production actor, registry, protocol, client-domain, resource-streaming, and
real loopback WebSocket tests are complemented by deterministic network and
retry models. Model results are not described as end-to-end production socket
results. The exact verified cases and explicit coverage gaps are maintained in
[Multiplayer Reliability Testing](multiplayer-testing.md).
