# Taiko Remote Resource Server

## Goal

`taiko-game` can load a remote song library over HTTP while preserving the same
chart semantics as local play:

- the song menu reads metadata and course summaries;
- selecting a course lazily downloads and imports the original TJA bytes;
- previews and matches lazily download audio when the manifest contains it;
  silent charts skip preview playback and use the gameplay clock without a
  media download.

Remote resources are immutable, content-addressed inputs. A client never trusts
a path, cache entry, resource ID, or parsed chart without validating it.

## Protocol v1

- API root: `<endpoint>/v1`
- Version field: `api_version`
- Current version: `1`
- Wire fingerprint field: `wire_schema_sha256`
- Current wire fingerprint:
  `f55ecfdf15bb0ccf8fe2a91c5e9925df3f146980a6ec8247279f8fe8cb86798f`

This is the intentionally incompatible resource protocol v1. There is no
adapter for the previous v1-shaped document. Clients reject a missing or
different wire fingerprint.

The library also carries the exact semantic versions and fingerprints used to
produce and play a chart:

```json
{
  "api_version": 1,
  "wire_schema_sha256": "<resource-wire-schema-sha256>",
  "semantics": {
    "canonical_schema_version": 1,
    "canonical_schema_sha256": "<canonical-schema-sha256>",
    "importer_semantics_version": 3,
    "importer_semantics_sha256": "<tja-importer-semantics-sha256>",
    "taiko_ruleset_version": 2,
    "taiko_ruleset_sha256": "<taiko-ruleset-sha256>",
    "audio_decoder_semantics_version": 1,
    "audio_decoder_semantics_sha256": "<audio-decoder-semantics-sha256>"
  },
  "songs": [],
  "warnings": []
}
```

The client requires every version and fingerprint to exactly match its local
canonical chart model, TJA importer, taiko ruleset, and bounded audio decoder. A
numeric version match alone is not sufficient.

Resource and multiplayer manifests share one presentation contract: title,
subtitle, and artist are limited to 160 UTF-8 bytes, while course names are
limited to 64. Required title/course text must be non-empty and cannot start or
end with whitespace; optional subtitle/artist text may be empty. The multiplayer
protocol imports these exact resource limits, so any validated resource library
can be converted to the match catalog without a later, narrower startup check.

The current semantic fingerprints are:

| Contract | Version | SHA-256 fingerprint |
| --- | ---: | --- |
| Canonical chart schema | 1 | `dd32375bd690fdf23d52a231249859b663158890608271173cdc068018bec245` |
| TJA importer semantics | 3 | `2d2624803b616e3d8644db66ed06778e29f197926603bdb639b6937a18951fb6` |
| Taiko ruleset | 2 | `16cd8a0bf582fc633f18686bffc97b55a4e323ee5613927465bd57b7294b50bc` |
| Audio decoder semantics | 1 | `6a71c64d00a05434402767950a7250ffa5d1a83ccc64ddd81e30575cd83e5eb5` |

## Endpoints

### `GET /v1/library`

Returns all song-menu metadata and immutable resource identities:

```json
{
  "api_version": 1,
  "wire_schema_sha256": "<resource-wire-schema-sha256>",
  "semantics": {
    "canonical_schema_version": 1,
    "canonical_schema_sha256": "<canonical-schema-sha256>",
    "importer_semantics_version": 3,
    "importer_semantics_sha256": "<tja-importer-semantics-sha256>",
    "taiko_ruleset_version": 2,
    "taiko_ruleset_sha256": "<taiko-ruleset-sha256>",
    "audio_decoder_semantics_version": 1,
    "audio_decoder_semantics_sha256": "<audio-decoder-semantics-sha256>"
  },
  "songs": [
    {
      "song_id": "<song-manifest-v1-sha256>",
      "source_path": "pack1/demo.tja",
      "source_id": "<sha256-of-raw-tja-bytes>",
      "audio_path": "pack1/demo.ogg",
      "audio_id": "<sha256-of-audio-bytes>",
      "title": "Demo Song",
      "subtitle": "",
      "artist": "Alice",
      "demo_start_seconds": 12.5,
      "courses": [
        {
          "index": 0,
          "name": "Oni",
          "level": 9,
          "canonical_chart_hash": "<canonical-chart-v1-sha256>",
          "object_count": 1234,
          "branch_segment_count": 1,
          "base_bpm": 180.0,
          "branch_decisions": [
            {
              "segment_id": 1,
              "decision_tick": 480000,
              "default_route_id": 0,
              "route_count": 3,
              "hint": {
                "Accuracy": {
                  "low": 700000,
                  "high": 900000
                }
              }
            }
          ]
        }
      ]
    }
  ],
  "warnings": [
    "skip broken/song.tja: ..."
  ]
}
```

`source_id` and a present `audio_id` are not path-derived lookup keys. Each ID
is exactly the lowercase SHA-256 digest of the bytes returned by its endpoint.
`audio_path` and `audio_id` are required-nullable and must either both contain
values or both be `null`; a silent chart never probes for a same-stem audio
file. These are the only blob identities in strict v1, so no duplicate hash
field can disagree.

`song_id` is the domain-separated identity used to select a song in
multiplayer:

```text
SHA-256(
  "taiko-song-manifest/v1\0"
  || source_id
  || audio-presence byte
  || audio_id when present
  || canonical-schema version and fingerprint
  || importer-semantics version and fingerprint
  || taiko-ruleset version and fingerprint
  || audio-decoder-semantics version and fingerprint
  || course_count
  || ordered canonical_chart_hash values
)
```

Digests are their fixed 64-byte lowercase ASCII form; versions and the course
count are unsigned 32-bit big-endian integers.

Paths and display metadata are intentionally excluded. Two library entries with
identical immutable inputs therefore share a `song_id` and one authoritative
engine input. The server publishes only the duplicate whose normalized
`source_path` is lexicographically smallest. A different chart, audio file,
semantic contract, or course order always produces a different identity.

`canonical_chart_hash` is computed from:

```text
SHA-256(
  "taiko-canonical-chart/v1\0"
  || canonical_schema_sha256
  || "\0"
  || compact-json(CanonicalChart)
)
```

The chart is validated before serialization. `canonical_schema_sha256` is its
fixed 64-byte lowercase ASCII digest, not the decoded 32 digest bytes. The
first NUL terminates the hash domain and the second separates that schema
digest from the compact JSON. The canonical model contains ordered
structs/vectors and integer timing values. Changing its wire representation
requires a new canonical schema fingerprint and therefore changes every
canonical chart hash.

### `GET /v1/charts/{id}`

Returns the original TJA bytes whose SHA-256 digest is `{id}`. The client imports
the bytes only when a course is selected, then recomputes the selected course's
`canonical_chart_hash`.

### `GET /v1/audio/{id}`

Returns audio bytes whose SHA-256 digest is `{id}`. Supported containers include
FLAC, MP3, OGG/Vorbis, PCM, and WAV. The exact Symphonia version, enabled
features, stream-shape rules, limits, sample conversion, cancellation points,
and failure policy are committed by the audio-decoder semantic fingerprint.

## Server indexing and containment

`taiko server` (or `taiko-resource-server`) applies these rules at startup:

1. Canonicalize `--songdir` and require it to be a directory.
2. Find regular `.tja` files recursively.
3. Canonicalize every chart path and require it to remain below `--songdir`.
4. When TJA `WAVE` is non-empty, resolve it relative to its chart, canonicalize
   it, and require the result to remain below `--songdir`. `..`, absolute
   paths, and symlinks cannot escape the root. Missing or empty `WAVE` is
   explicit audio absence and performs no filesystem probe.
5. Fail startup if discovery exceeds 4,096 TJA files, then process the sorted
   candidates sequentially so startup does not retain a parallel parse set.
6. Bound chart and audio reads before allocation. Hash and decode the same
   owned audio-byte snapshot before advertising it, so a path replacement
   cannot bind an unvalidated payload to the catalog: exact standard
   mono/stereo layouts only, 8–96 kHz, no more than 15 minutes, 256 MiB
   encoded, or 256 MiB of decoded stereo frames.
7. Enforce importer-v3's strict known source grammar, numeric domains,
   branch/roll/balloon rules, and source-shape limits before the third-party
   parser allocates notes. Reconcile its course, segment, note, and balloon
   output counts exactly, then build canonical output with fallible per-course
   and aggregate budgets; effective object limits are 131,072 per course and
   262,144 per import.
8. Compile every course and construct its official `Automatic` branch
   controller before advertising it. Before retention, a course may occupy at
   most 8 MiB of canonical JSON and the authority applies an independent
   250,000-object guard.
9. Compute raw chart and canonical-course digests, plus a raw-audio digest only
   when audio is present.
10. Retain the validated canonical charts and branch-decision tables in an
   authoritative catalog keyed by `song_id`; match execution does not re-read
   or re-import files. Fail startup if the retained catalog would exceed
   64 MiB of canonical JSON or 1,000,000 chart objects.
11. Validate the completed song and library against the shared v1 contract,
    and refuse to start if no playable song remains.

Invalid songs are omitted with a bounded, sanitized warning. They are never
advertised through an unverified fallback.

Files can change after indexing, so every chart/audio response is read through
its size bound and rehashed immediately before it is served. A missing file is
rejected; changed bytes return HTTP `409 Conflict`.

The server admits 16 chart/audio streams globally and eight per client IP, plus
a separate 512 MiB aggregate private-snapshot budget charged in 64 KiB units
from the opened descriptor's exact length. Each response is copied, verified,
and streamed only from that immutable snapshot in 64 KiB chunks. Stream or
snapshot capacity exhaustion and temporary-storage pressure return HTTP `503`
with `Retry-After: 1`. Verification and body transfer each use a size-aware
deadline of 10 seconds plus `ceil(content_length / 1 MiB/s)`, capped at five
minutes. An independent capacity-one producer owns all reservations, so the
body deadline remains active even if the consumer stops polling. Completion,
deadline, error, or client body drop releases every reservation.
Unix snapshots are unlinked immediately and Windows snapshots use
delete-on-close; other server targets are not accepted because they cannot
provide either verified cleanup mechanism.
The per-client key is the listener's TCP peer IP. Reverse-proxied users share
one eight-stream budget when the proxy connects from one address; forwarded-IP
headers are deliberately not trusted.

## Client validation and cache behavior

When `taiko-game` is configured with `--resource-endpoint`, it:

1. bounds the library response before JSON deserialization;
2. rejects unknown fields and validates the complete v1 document;
3. requires exact wire and semantic fingerprints;
4. requires lowercase SHA-256 IDs and `resource_id == expected_hash`;
5. bounds chart and audio bodies even when the server omits `Content-Length`;
6. verifies every downloaded or cached blob before use;
7. imports TJA bytes and verifies the selected canonical course hash;
8. recomputes song/course metadata, counts, BPM, and branch decisions from that
   verified import and rejects a mismatched library summary.

Chart and audio caches use the content digest as the cache filename and memory
key. The in-process cache has a 256 MiB aggregate budget and evicts its oldest
entries instead of growing with every preview.

The default disk cache has one v3 index and one global 2 GiB/8,192-entry quota
across all endpoint directories. Access updates a deterministic LRU clock.
Every hit is size-checked and rehashed. A missing, corrupt, bit-rotted,
unindexed, or inconsistent blob is removed and fetched again; it is never
accepted through a legacy mapping or unverified fallback. Atomic blob/index
writes, a cache-root file lock across client processes, and restart
reconciliation keep accounting bounded. Each cache operation uses one
two-second deadline across its in-process and OS lock acquisition and checks
preparation cancellation every 25 ms. If that deadline expires, a read is a
cache miss and continues through the ordinary bounded download plus SHA-256
verification path; touch and store skip disk persistence while retaining
already-verified bytes in memory.
Initialization reconciliation is deferred until the next successful cache
operation rather than blocking client startup indefinitely. Cache inspection
and removal return a bounded error if coordination remains busy.

An ordinary transient transport/body failure may make at most three
consecutive attempts, waiting 1 second and then 2 seconds. The first HTTP
`503` starts a distinct admission-busy policy. From that response onward, the
whole fetch is bounded by 315 seconds and 16 total requests, including any
intervening transport failures. Busy waits use 1, 2, 4, 8, 16, and then at
most 30 seconds plus bounded additive jitter. `Retry-After` is honored as a
minimum, capped at 30 seconds. Non-`503` HTTP errors fail closed. Connecting
is capped at 5 seconds; response headers are capped at 305 seconds so a legal
server-side size-aware verification may use its five-minute maximum. A
body that makes no progress for 10 seconds is retried. The absolute body
deadline mirrors the server's
`10 seconds + ceil(Content-Length / 1 MiB/s)` budget (capped at five minutes),
plus 5 seconds of client transport tolerance. An absent `Content-Length` uses
the endpoint's hard payload limit. Course/song changes and shutdown drop the
in-flight request and cancel body reads, busy/backoff waits, and the shared
audio decoder promptly. The deadline assumes at least 1 MiB/s of sustained
body progress; it does not promise that a large asset will complete over a
slower link.

Default limits are shared by server and client:

- library document: 16 MiB;
- chart: 16 MiB;
- audio: 256 MiB.

Use `--resource-cache-memory-only` to disable the disk cache. Cache inspection
and removal are available through:

```bash
taiko cache path
taiko cache list
taiko cache clear --endpoint <URL>
taiko cache clear --all
```

Without `--resource-endpoint`, the game uses the local song directory.

The cache quota is global, so clearing one endpoint removes only that
endpoint's blobs and atomically rewrites the v3 index while preserving other
endpoints; `--all` removes the complete remote cache root. Cache mutations and
inspection coordinate through the same process mutex and cache-root OS file
lock.

## Multiplayer endpoints

- `GET /v2/multiplayer/healthz`
- `WS /v2/multiplayer/ws`

Multiplayer uses a separate server-authoritative protocol. Its authority model,
state machine, readiness proof, reconnect behavior, and final-result rules are
defined in [Multiplayer Protocol v2](developer/multiplayer-protocol-v2.md).
Player-facing hosting, invitation, LAN/Internet, TLS/NAT, and recovery guidance
is in the [Multiplayer Guide](multiplayer.md).

The built-in listener serves plaintext HTTP and WebSocket. It does not
provision certificates, open firewalls, forward router ports, or traverse NAT.
For an Internet deployment, terminate TLS at a reverse proxy and forward both
resource requests and `/v2/multiplayer/ws` WebSocket upgrades. Players use the
public `https://` base endpoint, from which the client derives `wss://`.

## Example

```bash
# Start the resource and multiplayer server.
cargo run -p taiko-game --release -- server \
  --songdir ./taiko-game/songs --host 127.0.0.1 --port 4150

# Start the game against the remote resource endpoint.
cargo run -p taiko-game --release -- \
  --resource-endpoint http://127.0.0.1:4150/
```

For online rooms, each player starts the normal game and chooses
`Online Multiplayer`. The creator enters `http://127.0.0.1:4150` in `Create`;
joiners or spectators paste the complete invitation in `Join` or `Spectate`.
