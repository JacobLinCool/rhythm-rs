# Multiplayer Reliability Testing

This document records what the current test suite actually verifies. Evidence
is separated by execution layer so a deterministic model result is not mistaken
for a production WebSocket or full-player result.

## Evidence labels

- **Production-direct** calls the real protocol validators, client domain,
  registry, room actor, resource-serving functions, or runtime in process.
- **Production-loopback** runs production server and/or client transport code
  over an operating-system loopback socket. A row states when the opposite
  endpoint is a scripted WebSocket peer rather than the full game authority.
- **Deterministic model** uses test-only link, replica, or retry state. It is
  reproducible and useful for invariants, but it does not execute the production
  room actor, WebSocket stack, or client UI.
- **Not covered** means the repository does not currently contain a test that
  supports the claim. Such a case remains a test backlog item, not an implicit
  pass.

## Network fault models

`TcpLikeLink` models an established TCP/WebSocket connection. Latency, jitter,
bandwidth, segment loss, retransmission delay, and receiver stalls affect
delivery time, while application messages remain ordered and exactly once.

`AdversarialLink` deliberately drops, duplicates, or delays whole protocol
messages in an explicitly scripted logical request/retry trace. It must not be
interpreted as application-message loss, duplication, or reordering inside one
established WebSocket, and it has no transport epoch with which to prove
superseded-connection fencing.

The following profiles are used only by deterministic model tests:

| Profile | One-way latency | Jitter | Segment loss | Bandwidth | Retransmit / stall |
| --- | ---: | ---: | ---: | ---: | --- |
| Perfect | 0 ms | 0 ms | 0% | unlimited | none |
| LAN | 2 ms | 1 ms | 0.01% | 100 MiB/s | 20 ms retransmit |
| WAN | 40 ms | 15 ms | 0.5% | 10 MiB/s | 120 ms retransmit |
| Poor WAN | 120 ms | 80 ms | 3% | 512 KiB/s | 300 ms retransmit; 750 ms every 17 messages |

Seeds are fixed in the tests. The reproducibility test runs the same seeded
probabilistic trace twice and requires identical deliveries.

## Verified production behavior

| Area | Level | Cases currently verified |
| --- | --- | --- |
| Wire contract | Production-direct | Strict JSON shapes and tags, schema fingerprint, bounded UTF-8 values and vectors, fixed lowercase digest/token forms, every client/server variant, missing/unknown-field rejection, manifest/snapshot/live/final invariants, and challenge-response golden messages. |
| Clock admission | Production-direct | Stable server-timed samples become ready; excessive p95 path time, high jitter, expired evidence, forged tokens, unknown nonces, and replayed receipts do not. Pending challenges and client probes are bounded. Registry tests bind receipts to one session and classify pending-capacity as retryable but forgery as terminal. Client-domain tests require both local four-timestamp samples and a non-expired server acknowledgement, invalidate both on a new transport welcome, and force a fresh probe after `ClockNotReady`. |
| Room authority | Production-direct | One player cannot start; a late spectator does not change readiness; fresh server clock evidence is required at both `SetReady` and `StartMatch`; a pre-start transport resume clears clock evidence and downgrades `Ready` to `Prepared` until the client reasserts readiness with fresh evidence. Duplicate ready acknowledgements remain idempotent after evidence expiry. Leader migration, heartbeat lease expiry, countdown progress without inbound messages, authoritative input/score/finalization, resume fencing, grace reclamation, active leave/DNF cleanup, and new rematch epochs are exercised. |
| Capacity and queues | Production-direct | The room actor admits exactly four players and 64 spectators and rejects the next slot of each role. Session and room registries enforce 512-session and 256-room caps. Reliable outbound and client writer queues have explicit full behavior; live state is latest-value coalesced. |
| Epoch isolation | Production-direct | One hundred sequential rematches create increasing match IDs and reject input for every previously finished epoch without mutating the current room. |
| Registry lifecycle | Production-direct | Message-rate budgeting, create/duplicate/resume behavior, rejected invitation sequencing, session capacity, room capacity, and single-use clock receipts are exercised. A gap-progression regression sends a future command, fills the missing sequence, replays an older room-cache command, then retries the future envelope; the future `SequenceGap` is not cached and the old duplicate cannot regress the session watermark. A terminal `LeaveRoom` regression drops the first applied ACK logically, retries the identical sequence on the same still-open session after membership has been cleared, and requires the bounded session cache to replay the original ACK; different contents at that sequence are rejected. |
| Client domain and input processing | Production-direct | Contextual command acknowledgements, a hard 64-command window, selection-intent deduplication/coalescing, a hard 512-input window, retry gaps, stale snapshot/live rejection, new-match cleanup, and resume identity are exercised. Paired room-actor and `OnlineDomain` tests cover an acknowledged old-match input followed by a lost epoch-transition snapshot, disconnect, same-actor resume, and reconciliation from the mandatory authoritative new-epoch snapshot. A room-actor test also proves that one contiguous late input advances the processed watermark without scoring and accepts the next legitimate sequence; a client test proves that its dropped prefix does not discard a later pending input. Runtime tests separately cover automatic branch selection, future/regressing timestamps, rate limits, gaps, and conflicting duplicates. |
| Preparation and audio | Production-direct | Course confirmation records ready intent, but the auto-ready proof waits for chart/audio preparation and both clock views. Progress ordering is download → verify → load → prepared. Cancellation is observed during resource body reads, retry waits, and audio decode. Corrupt audio never reaches prepared; exact mono/stereo layouts, layout/rate stability, sample-rate/duration/decoded-memory bounds, immutable frame retention, and explicit retry of a latched preparation failure are exercised. The decoder semantic descriptor and all numeric limits have a pinned digest. |
| Client transport lifecycle | Production-direct | Graceful batches require exactly one final `LeaveRoom`; pending commands precede it. Every unsent global mutation in that batch is bound to the current known room revision, so later serialized mutations stale-reject instead of bypassing optimistic concurrency with `None`. Disconnect with active membership reports an unconfirmed leave. Reconnect backoff is bounded, and the attempt budget resets only after at least 30 seconds of membership followed by a `HeartbeatAck`, not after `Welcome` or a short join/drop cycle. Headless course confirmation applies changed selections, retries a failed unchanged selection, and starts only when the authoritative readiness predicate allows it. |
| Client resource invariants | Production-direct | Ordinary transport retries are limited to three consecutive attempts with 1/2-second waits. Admission-busy retries are independently bounded by 315 seconds and 16 requests, use reproducible bounded jitter, cap `Retry-After` at 30 seconds, and remain cancellable. Non-`503` behavior fails closed; content/size verification and corrupt-cache repair are exercised. Memory cache eviction is bounded. The v3 disk index enforces one deterministic access-LRU quota across endpoint directories, preserves accounting over restart, and rejects an entry that cannot fit without leaving partial state; production defaults are 2 GiB/8,192 entries. |
| Client resource transport | Production-loopback | Scripted real TCP peers exercise more than the old three `503` responses before success, permanent busy termination, permanent `404`, partial-body retry, distinct 5-second connect and 305-second header budgets, body-idle and absolute deadlines, oversized and hash-mismatched payloads, cancellation during a stalled response or busy wait, and observation that cancellation closes the peer socket. |
| Client cache coordination | Production-loopback | Real child processes contend for the cache-root OS file lock. Coordination uses one bounded deadline across the client mutex, process mutex, and OS lock; a busy read continues through verified network bytes, a busy store leaves no partial disk state while retaining the verified memory copy, cancellation stops before network fallback, and normal persistence/read recover after release. A second writer cannot create index/blob state while a holder owns the lock and completes atomically after release. |
| Server resource delivery | Production-direct | Content-address identity, path containment, size limits, serve-time copy-and-digest verification into a private snapshot, request-before-verification tamper rejection, and a same-inode overwrite after verification that cannot alter the streamed immutable body are exercised. Multi-chunk 64 KiB streaming, global/per-client admission, the separate 512 MiB snapshot reservation, `503` plus `Retry-After`, size-aware verification/transfer deadlines, real partial-snapshot cancellation, and reservation release after completion, deadline, or body drop are also covered. The producer owns permits and snapshot lifetime independently of consumer polling. |
| Server slow-reader recovery | Production-loopback | A real TCP client reads only the headers of a 16 MiB response. The test proves the permit remains occupied before the shortened deadline, is released no earlier than the deadline window, and the connection closes with fewer body bytes than its declared `Content-Length`; a replacement request then completes with the correct body. The direct producer test separately observes the explicit deadline error when a body is never polled. |
| Server catalog and importer | Production-direct | Startup hashes and decodes the same bounded owned audio snapshot, rejects undecodable audio and unsupported branch conditions, refuses an empty playable catalog, and enforces canonical per-course and aggregate footprint accounting before retention. The importer validates a strict source grammar and all numeric domains before the third-party parser, rejects silent-repair cases, requires exact course/segment/note/balloon reconciliation afterward, preserves big-roll and branch semantics, then applies fallible per-course and aggregate canonical-output budgets. Forty-eight importer regressions cover malformed metadata/directives, numeric boundaries, roll pairing, exact balloon tables, branches, structural loss, determinism, and budgets. Production-builder fixtures exercise sorted sequential indexing and fail-closed library construction; targeted helpers exercise duplicate-song replacement and aggregate accounting. The exact 4,096-file deployment boundary is listed below rather than inferred from those smaller fixtures. |
| Real authority socket smoke | Production-loopback | One real server admits four players plus one spectator, rejects a fifth player with a typed and idempotent result, resumes one player without changing actor identity, preserves one leader, and shuts down cleanly. A separate real-authority test requires one typed schema-mismatch fatal message before close. |
| Real client-transport socket | Production-loopback | The production client transport is connected to scripted loopback WebSocket peers. One test proves it does not report graceful completion until the peer acknowledges `LeaveRoom`; another withholds the first acknowledgement and requires an identical terminal command retry before acknowledging it; a third exhausts the reconnect attempt budget against a Welcome-then-drop peer. These do not execute the production room actor. |
| Production ACK-loss resume | Production-loopback | A test-only reverse proxy connects production headless clients to the production authority, drops an applied `SelectSong` acknowledgement, and later drops an accepted roll-input acknowledgement. Both paths resume with the same ActorId, fence the old transport with `SessionSuperseded`, drain pending state, and rebuild clock/readiness evidence. The room revision advances exactly once for the command and the authoritative runtime records exactly one roll hit for the retried input. |
| Full two-player match | Production-loopback | Two production headless clients use the production HTTP/WebSocket server to create/join, receive an invite, select one song and different courses, download and hash the TJA/audio, import/decode, establish both clock views, auto-ready, and start. In Playing, each sends a real Don or Kat through the production client transport at its authoritative note time; both server runtimes report exactly one hit and zero misses. The clients then converge on identical two-player results and leave through acknowledged shutdown. |

Terminal `LeaveRoom` ACK recovery is deliberately session-scoped. If the
transport dies after the authority applied the leave, membership and its resume
token are already revoked: the actor is authoritatively out of the room, but a
new connection cannot resume merely to recover the old session cache. The
client must therefore report an unconfirmed leave rather than claim
cross-disconnect exactly-once confirmation.

Room deadline tests advance the actor with explicit monotonic `Instant` values;
they do not sleep until countdown, match, reconnect-grace, or room deadlines.
Loopback tests use wall-clock timeouts only to bound real asynchronous I/O.

## Verified deterministic-model behavior

| Model | Composition and workload | Invariant |
| --- | --- | --- |
| Ordered link | 200 messages under each of Perfect, LAN, WAN, and Poor WAN | Established-link delivery remains ordered and exactly once. |
| Backpressure link | Three serialized messages with 1,000 B/s and periodic stalls | Completion is delayed without application-message loss. |
| Synthetic ordered-delivery replica | Two players and one spectator, 96 pre-generated room revisions, one link per replica, all four profiles | Every test replica observes ordered snapshots, contiguous local command acknowledgements, unique actor identities, and final revision convergence. Poor WAN takes longer in the synthetic delivery schedule. This does not execute the room actor or bounded transport queues. |
| Adversarial link | Scripted drop, duplicate, delay/reorder plus seeded probabilistic faults | The injector reproduces the requested message-layer trace and identical seeds reproduce identical traces. |
| Retry sequence | One player model, command and input retries across scripted drop/duplicate/delay | Commands and inputs apply once, retryable gaps are exposed, cached command results are stable, and the model converges to contiguous next sequence numbers. |

These models validate ordering and retry algorithms. They do not show that a
four-player production room completes a match under Poor WAN, nor that a real
WebSocket can reorder messages.

## Explicit coverage gaps

The current suite does **not** claim any of the following:

- the same full production flow with three or four real players, or a
  production-socket rematch after the covered two-player finished match;
- production WebSocket or room-actor execution under the Perfect/LAN/WAN/Poor
  WAN profiles, packet shaping, TLS proxies, NAT, or Internet paths; in
  particular, the model's 512 KiB/s Poor WAN profile is below the resource
  server's 1 MiB/s large-body deadline assumption and is not a download
  service guarantee;
- player-control responsiveness with four players plus 64 simultaneously slow
  real spectators (capacity and bounded-queue behavior are tested separately);
- a real-socket 65th-spectator rejection (the exact spectator limit is tested
  directly in the room actor);
- loopback expiry of the 10-second `Hello` deadline, the 30-second unaffiliated
  session deadline, or its post-`LeaveRoom` reset;
- 17 concurrent real HTTP downloads against the production resource limit;
- a real slow-reader HTTP workload that holds eight streams from one IP while
  another IP completes, or one that waits for the five-minute maximum body
  deadline (one real stalled TCP reader and replacement are covered with a
  shortened production path);
- process-kill/restart automation for an embedded or dedicated authority;
- startup at the exact 4,096-file, 64 MiB, or 1,000,000-object catalog
  deployment boundaries with a real corpus (the bound/accounting paths have
  targeted tests);
- end-to-end TUI-versus-headless equivalence. Both adapters share
  `OnlineDomain`, and headless selection/branch mapping has targeted unit tests,
  but there is no full UI-driving multiplayer scenario.

These gaps should be closed before making stronger deployment or scale claims.

## Reproduction commands

```bash
cargo test -p taiko-multiplayer-protocol --locked
cargo test -p taiko-resource-server --locked
cargo test -p taiko-game --locked
cargo test --workspace --locked
```

The real loopback suite binds a local TCP port, so a restricted sandbox may
need explicit loopback permission. Network model tests require no external
network access.

Player-facing expectations for invitations, reachability, TLS/NAT boundaries,
session admission, headless controls, resource limits, and reconnect grace are
documented in the [Multiplayer Guide](../multiplayer.md).
