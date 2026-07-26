# taiko-resource-server

HTTP + WebSocket server for delivering taiko resources (`library`, `chart`, `audio`) and multiplayer rooms to `taiko-game`.

You can run it either as:
- `taiko server ...` (subcommand on `taiko-game`)
- standalone binary `taiko-resource-server ...`

## Run

```bash
cargo run -p taiko-game --release -- server \
  --songdir ./taiko-game/songs --host 127.0.0.1 --port 4150

# or standalone
cargo run -p taiko-resource-server --release -- \
  --songdir ./taiko-game/songs --host 127.0.0.1 --port 4150
```

## Endpoints

- `GET /healthz`
- `GET /v1/library`
- `GET /v1/charts/{id}`
- `GET /v1/audio/{id}`
- `GET /v2/multiplayer/healthz`
- `WS /v2/multiplayer/ws`

## Client

Players start the game normally; play modes are selected inside the TUI:

```bash
cargo run -p taiko-game --release -- \
  --resource-endpoint http://127.0.0.1:4150/
```

Starting `server` makes the authority available but does not create a room.
Choose `Online Multiplayer` in the game, then:

- `Create` connects to this authority, creates a room, and displays one complete
  `taiko://` invitation;
- `Join` or `Spectate` accepts that complete invitation and a display name; or
- `Host here` embeds a temporary authority in the game process without using
  the independent server.

There is no player-facing multiplayer CLI. Only independent server operation
uses the `server` subcommand.

## Fail-closed resource limits

Startup scans and indexes the catalog sequentially. It accepts at most 4,096
TJA files. Importer v3 rejects over-budget source shape before the third-party
parser allocates notes, then constructs canonical output with fallible
per-course and per-import budgets (including effective object ceilings of
131,072 and 262,144). Before retaining authority state, the server independently
limits canonical JSON to 8 MiB per course and 64 MiB across the catalog, with
additional 250,000-per-course and 1,000,000-catalog object guards. Exceeding an
aggregate deployment limit aborts startup; an individual invalid song is
omitted with a bounded warning. Match execution uses only the validated catalog
retained at startup.

Audio is hashed and decoded from the same owned byte snapshot before a song is
advertised, so replacing a path between two opens cannot publish unvalidated
bytes. It must use the exact standard mono or stereo channel layout, remain at
8–96 kHz, be no longer than 15 minutes, at most 256 MiB encoded, and at most
256 MiB after conversion to stereo frames. Corrupt, non-finite, mid-stream
format-changing, overlong, or decompression-heavy audio is rejected. Every
course must also run under the official `Automatic` branch policy, and startup
fails if no playable song remains. The decoder engine, features, limits,
conversion, cancellation boundaries, and failure policy are an explicit
semantic fingerprint shared with every client and match manifest.

Chart/audio bodies have these admission and transfer controls:

- 16 concurrent streams globally and eight per client IP;
- a separate 512 MiB aggregate snapshot reservation, charged in 64 KiB units
  from the opened descriptor's exact length before copying;
- fixed 64 KiB read/stream chunks;
- HTTP `503` with `Retry-After: 1` when a stream or snapshot admission limit is
  full, or when private temporary storage cannot be prepared;
- a deadline of 10 seconds plus the content length at 1 MiB/s, capped at five
  minutes, independently for verification and body transfer;
- an independent bounded producer that owns both permits, so the deadline
  continues even when the HTTP consumer stops polling;
- release of stream and snapshot reservations on completion, deadline, read
  error, or body drop.

Chart and library documents are capped at 16 MiB; encoded audio is capped at
256 MiB. Each request copies and hashes one bounded source descriptor into a
private server-owned snapshot before sending headers, then streams only that
snapshot. Bytes already changed after indexing return `409 Conflict`; even an
equal-size in-place overwrite after verification cannot change the body behind
the immutable content-addressed URL.

Snapshots use the operating-system temporary directory and live until their
producer completes, times out, or observes body cancellation. Unix snapshots
are unlinked immediately after creation; Windows snapshots use delete-on-close,
so process termination cannot leave a named partial file on those platforms.
Other operating-system targets are rejected at compile time instead of using
an unverified best-effort cleanup path.
The aggregate reservation caps live snapshot payloads at 512 MiB even when
stream slots remain (for example, only two 256 MiB audio snapshots can coexist).
Production hosts must provision that space and configure their standard
temporary-directory environment if needed.

“Client IP” is the TCP peer address observed by this listener. If a reverse
proxy opens upstream connections from one address, its users share that
eight-stream budget; the server does not trust forwarded-IP headers.

## Operations boundary

The built-in listener is plaintext HTTP/WebSocket and does not provide TLS,
port forwarding, or NAT traversal. Put Internet deployments behind a TLS
reverse proxy that forwards resource requests and WebSocket upgrades. See the
[Multiplayer Guide](../docs/multiplayer.md) for complete invitations, LAN and
Internet setup, reconnect limits, and server-authoritative behavior.

Room state, actor identities, resume credentials, and live matches are held in
this process's memory. There is no database persistence, replication, failover,
or cross-node room migration. Restarting the process ends every room. The
built-in service also does not provide matchmaking or a relay.
