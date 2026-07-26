# Multiplayer Data and Sync Design Review

This review evaluates the multiplayer data model, resource architecture,
connection synchronization, and player-facing flows as one system. The target
is a strict protocol v2: incompatible peers fail closed, and there
is no legacy or compatibility path.

## Verdict

The design is suitable for convenient and reliable two-to-four-player sessions
under one reachable authoritative server.

- Players can host, create, join, or spectate with one complete invitation.
  Authority-backed multiplayer remains available when the local song directory
  is missing, and spectators do not download gameplay resources.
- Room, match, input, score, and result state have one authority. Reconnect
  preserves player identity, while a mandatory current snapshot repairs missed
  transitions.
- Every exchanged chart identity, and every audio identity when audio exists,
  is content-addressed and bound to the canonical chart, importer, ruleset, and
  decoder semantics used to play it.
- Queues, collections, retries, deadlines, cache coordination, resource
  snapshots, rooms, sessions, players, and spectators all have explicit bounds.
- Failures are visible and typed. The UI does not claim that a disconnected
  player is ready, that an unacknowledged leave succeeded, or that a DNF is an
  ordinary failure.

This is deliberately not peer-to-peer lockstep, high availability, persistent
room storage, NAT traversal, matchmaking, or a complete anti-cheat system.
Those boundaries are operationally significant and are listed below.

## Authoritative data model

### Canonical chart

`CanonicalChart` is the deterministic exchange format. Its validation rejects
an empty or unsorted tempo map, invalid signatures, unknown lanes, non-dense
object IDs, invalid object ranges, invalid branch routes, and non-canonical
branch hints. Its SHA-256 identity commits to:

1. a domain separator;
2. the pinned canonical schema digest; and
3. compact JSON in the declared Rust field order and enum representation.

Golden tests pin the exact JSON shape. A schema or semantic change therefore
cannot silently retain the same chart identity.

### TJA source boundary

The shared importer treats `tja@0.5.0` as an untrusted parser implementation,
not as the data contract. A strict source scan runs first and:

- accepts only declared metadata, headers, directives, and note syntax;
- requires a finite positive BPM and validates every numeric conversion before
  it can be rounded into canonical fixed-point data;
- rejects malformed or unknown directives, incomplete courses, invalid
  branch/roll structure, unsupported branch conditions, and non-exact balloon
  hit tables;
- rejects content after `#BRANCHEND`, because the pinned parser would otherwise
  silently drop it; and
- reconciles the parser's course, segment, note, and balloon counts exactly
  against the source scan.

Big-roll flags, finite non-negative `DEMOSTART`, timestamp bounds, branch
threshold domains, and same-tick conflict rules are preserved explicitly.
The complete mapping, defaults, quantization, ordering, and budgets are pinned
by the importer semantic fingerprint. Invalid source fails before local and
server loading can diverge.

### Resource catalog

The server constructs the public resource library and retained authoritative
charts from the same validated inputs. Before multiplayer rooms can start, the
registry verifies:

- complete resource semantics;
- exact song-ID set equality, not only equal counts;
- complete song and course manifests in order;
- recomputed canonical chart hashes;
- object and branch-segment counts; and
- every branch-decision field.

A library and authority catalog that drift in any of these dimensions are
rejected at startup.

### Client song identity

The client represents a song origin as one closed choice:

- `Local { source_path, audio_path: Option<PathBuf> }`; or
- `Remote { RemoteSongIdentity }`.

Remote identity contains the manifest song ID, its content-addressed chart ID,
and a required-nullable audio ID. Loading, preparation, and manifest checks all
read this same identity, so a silent song cannot acquire audio by guessing a
same-stem filename and optional duplicate locators or hashes cannot disagree.

### Wire state

Protocol v2 uses typed IDs and stage-specific enums. Membership kind comes only
from `ActorId`; match state comes from the current `MatchId`; room mutation
order comes from `RoomRevision`; and result state is server-generated.
Branch policy is not selectable wire state: every node derives the official
`Automatic` policy.

## Connection and synchronization

The normal lifecycle is:

```text
connect
  -> strict Hello/schema negotiation
  -> Create/Join/Spectate intent
  -> MembershipGranted validated against that intent
  -> authoritative RoomSnapshot
  -> content preparation + two-sided clock evidence
  -> ready/start/countdown
  -> sequenced inputs + authoritative live state
  -> authoritative final result
  -> acknowledged LeaveRoom
```

The following invariants make recovery deterministic:

- Every control command has a monotonically increasing sequence and a typed
  acknowledgement. Exact retries inside the bounded replay window return the
  original outcome. A future gap is not cached or consumed, and replaying an
  older command cannot regress the session watermark.
- Graceful shutdown binds every unsent global room mutation to the latest known
  authoritative revision before taking its final command snapshot. A later
  serialized mutation may be stale-rejected, but it cannot use a missing
  revision to bypass the concurrency guard.
- `LeaveRoom` is a terminal command with a session-scoped confirmation path.
  Applying it removes the actor and revokes the membership token, while the
  registry keeps the exact envelope and original ACK in its bounded session
  cache. If that ACK alone is lost, the client can resend the same sequence on
  the same still-open session and receive the cached ACK. If the transport has
  also disconnected, there is no membership left to resume and no way to query
  that old session cache: the authority still records the actor as having left,
  but the client must treat confirmation as unknown. This is not
  cross-disconnect exactly-once confirmation.
- Every input has a `MatchId` and monotonically increasing input sequence. Only
  the contiguous prefix advances; a gap consumes nothing, while a valid but
  late/rate-limited input is acknowledged as dropped so later input cannot
  deadlock.
- Every room snapshot has a revision, and stale revisions are ignored.
- Every song selection or rematch creates a new match epoch. Old-epoch input,
  readiness, and acknowledgements are rejected.
- Resume credentials identify membership, not match progress. A resumed
  transport must receive the current authoritative snapshot; the client
  reconciles its pending state from that snapshot and server acknowledgements.
- A successful resume fences the old transport. Fresh and resumed `Welcome`
  flags, non-regressing command watermarks, room IDs, roles, actor kinds,
  invitation/resume credentials, and the local actor's presence in each
  accepted snapshot are validated against locally held evidence before state
  is applied.
- Clock readiness requires both a client offset estimate and fresh,
  server-observed challenge-response evidence for the current transport.
  Reconnect clears the old evidence.

## Resource delivery and local storage

Chart and audio URLs use their lowercase SHA-256 content IDs directly. At serve
time, the server opens the indexed file, reserves its exact size against a
512 MiB aggregate budget, copies and hashes it into a private snapshot, and
streams only the verified snapshot. A same-inode overwrite therefore cannot
change a response after verification.

Resource delivery is bounded by:

- 16 global streams and eight streams per observed client IP;
- a 512 MiB aggregate snapshot reservation in 64 KiB units;
- 64 KiB copy and body chunks;
- size-aware verification and transfer deadlines;
- retryable `503` responses for admission or temporary-storage pressure; and
- cancellation-owned cleanup and reservation release.

Unix snapshots are unlinked immediately; Windows uses delete-on-close. Other
server targets are rejected at compile time instead of using best-effort
snapshot cleanup.

The remote cache has one deterministic access-LRU quota across endpoints.
Cache coordination uses one bounded deadline across in-process and
cross-process locks. A busy read becomes a verified network miss; a busy write
keeps verified bytes in memory without creating partial disk state.

## Player-facing behavior

- A complete invitation carries the server URL, room code, and capability
  token; users do not reconstruct these values manually.
- Create, Join, and Spectate are distinct flows. A dedicated server can outlive
  the room leader; an embedded host cannot outlive its process.
- Chart download/import and complete audio decode happen before automatic ready.
  Preparation exposes ordered progress and a failed preparation is latched
  until the player explicitly retries it.
- Course/start/result controls pause while the transport is reconnecting.
- The TUI displays rejection and reconnect reasons. The headless driver is
  test-only and exercises the same production transport/domain.
- A clean quit sends `LeaveRoom` after pending commands and waits for its
  acknowledgement. An offline quit reports that the leave was unconfirmed,
  including the case where authority may already have applied the terminal
  command but the transport failed before its session-scoped ACK arrived.

## Findings resolved by this redesign

| Prior risk | Resolution |
| --- | --- |
| Duplicate remote song locators and optional hashes | One closed `SongOrigin` identity |
| Client-selectable or duplicated branch policy | One derived `Automatic` policy |
| Match-scoped watermark in resume credentials | Recovery only from authoritative current-epoch state |
| Catalog comparison by count | Exact IDs, manifests, recomputed charts, counts, and decisions |
| Canonical descriptor could drift from serde output | Golden JSON shape plus schema-bound hash |
| Third-party parser silently ignored or repaired malformed TJA | Strict source grammar, exact parser-shape reconciliation, and a complete importer fingerprint |
| Source file could change after verification | Verify and stream a private immutable snapshot |
| Concurrent snapshots could consume multiple GiB | Separate 512 MiB weighted reservation |
| Temporary-storage pressure looked permanent | Retryable `503` with bounded client policy |
| Cache lock contention could stall startup or writes | One operation deadline and fail-safe network/memory behavior |
| Forged or contradictory handshake context | Validate Welcome, watermark, membership, room, role, and actor before apply |
| Empty offline song directory blocked online play | Multiplayer remains reachable; the local error stays visible |
| A second player-facing CLI could diverge from the TUI state machine | Player CLI removed; one TUI adapter over `OnlineDomain` |

## Verification

The reliability suite separates production-direct tests, production loopback
tests, and deterministic network models. It covers:

- strict wire serialization and schema fingerprints;
- strict TJA syntax/numeric/roll/balloon/branch rejection and exact
  source-to-parser reconciliation;
- two-player production matches through real HTTP and WebSocket transports;
- lost command and input acknowledgements followed by same-identity resume;
- four-player plus spectator capacity and resume over a real authority socket;
- good LAN, constrained mobile, poor WAN, and stall/backpressure profiles;
- deterministic drop, duplicate, reorder, and reconnect/retry faults;
- slow readers, body cancellation, partial snapshots, storage admission, and
  exact reservation release; and
- real cross-process cache-lock contention.

See [Multiplayer Reliability Testing](multiplayer-testing.md) for the exact
coverage and the intentionally untested production environments.

## Deployment boundaries

- The authority and room state are in one process and in memory. Restarting it
  ends rooms and invalidates resume credentials; there is no replication,
  durable log, failover, or rolling-upgrade handoff.
- The built-in listener is plaintext HTTP/WebSocket. Internet deployment
  requires an external TLS reverse proxy.
- There is no built-in relay, matchmaking, UPnP, hole punching, or NAT
  traversal.
- Server-observed clock evidence is a quality gate, not hardware-clock
  attestation. Server-side deterministic scoring reduces trust in clients but
  is not a complete adversarial anti-cheat boundary.
- Deterministic network models do not replace validation on the operator's
  actual WAN, proxy, TLS, firewall, storage, and process-supervision stack.

Player and operator instructions are in the
[Multiplayer Guide](../multiplayer.md); the precise state and wire contract is
in [Multiplayer Protocol v2](multiplayer-protocol-v2.md).
