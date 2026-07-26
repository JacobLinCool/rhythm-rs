# Controller Input Architecture

## Boundary

Controller sources normalize physical strikes; they never judge notes, assign
score, estimate remote authority time, or mutate a runtime directly. The
canonical four-pad value remains `rhythm_mode_taiko::TaikoAction`, preserving
left/right and Don/Kat without a parallel mouse or web action model.

The supported sources are:

- keyboard bindings;
- a terminal pointer surface driven by a mouse or MacBook trackpad click; and
- an embedded, explicitly started HTTP/WebSocket controller for trusted LAN
  phones.

Terminal protocols expose pointer-cell clicks, not raw trackpad finger
coordinates, pressure, or simultaneous touches. The terminal surface is
therefore a casual/accessibility controller. The browser surface uses Pointer
Events and supports simultaneous phone touches.

## Contracts

`ControllerStrike`

- carries one player slot, one `TaikoAction`, the local receipt `Instant`, the
  source, and an optional source sequence;
- never accepts a client clock;
- is admitted by one App dispatcher shared by single-player, local two-player,
  and the local client of an online match.

`DrumSurfaceLayout`

- is the single source of truth for rendering and hit testing the ordered
  `LEFT_KAT | LEFT_DON | RIGHT_DON | RIGHT_KAT` surface;
- is installed only after a successful render and is invalidated by resize or
  page changes;
- accepts only a left-button down event inside the rendered area.

`LanControllers`

- automatically considers only recognized physical `en*`, `eth*`, and `wl*`
  interfaces with an active broadcast-capable private IPv4 address, excludes
  point-to-point/VPN/bridge/VM candidates, and uses the active route only as a
  preference among those safe candidates; unknown interface names require an
  explicit player-entered bind address;
- selects a visibly labelled local-only loopback address when no
  phone-reachable candidate exists, while still allowing the player to enter
  one exact bind address;
- binds that one explicit local interface address on an ephemeral port;
- owns independent bounded P1 and P2 input queues;
- issues a separate 256-bit one-time pairing token for each slot;
- speaks the `taiko-controller-v1` WebSocket subprotocol and commits a
  one-time pairing only after the browser acknowledges its first `ready`
  response;
- upgrades a committed pairing to a resumable session token and fences the
  prior socket on resume; fencing rejects new admission from that socket but
  never revokes a hit already committed to the bounded queue and acknowledged
  as accepted;
- authenticates before accepting sequenced hits, stamps receipt time before
  parsing or queueing work, and rejects binary, oversized, unknown, out-of-
  sequence, over-rate, inactive, stale-generation, or overflowing input;
- requires exact Host, Origin, and WebSocket subprotocol values;
- closes every socket before joining its server thread during shutdown.

## Player flow

The mode screen opens Controller Setup with `C`. The player can edit the exact
LAN bind IP, start or stop the controller server, choose which local player the
terminal pointer controls, show a scannable pairing QR, copy a pairing URL, and
rotate either player credential. QR rendering includes the mandatory quiet
zone and uses explicit black modules on white cells for camera contrast.

The pairing URL carries the current game UI language as an exact
`?lang=en`, `?lang=zh-Hant`, or `?lang=ja` query parameter. The embedded page
localizes every player-visible state, drum label, and ARIA label without
external assets. The 256-bit credential remains exclusively in
`#token=...`; fragment data is not sent in the HTTP request, Referer, or server
log. An absent or invalid language tag selects English, while an absent or
invalid token fails closed without opening a WebSocket.

The server is plaintext by design and is only for a trusted LAN. It does not
provide TLS, relay, NAT traversal, Internet exposure, or matchmaking.

## Timing and bounds

Keyboard and terminal pointer strikes are stamped when Crossterm reads the
physical press. Web strikes are stamped when the WebSocket text frame arrives.
The App later converts that same `Instant` with the existing calibrated local
clock or online authority estimate, so a UI scheduling delay does not become a
judgement delay.

The TUI reads at most 64 raw events into one batch, delivers every semantic
input from that batch before a due logic tick, and samples one audio clock
anchor for the entire App dispatch batch. Simultaneous P1/P2 hits therefore
retain equal timing while the fixed batch limit still prevents input floods
from starving scheduled work.

Each LAN slot has its own fixed queue. The App drains a fixed maximum per tick
and rejects stale receipts instead of replaying an input burst after a pause or
suspend. It holds all already-collected sources until the LAN ingress
completion epoch is quiescent, and it keeps holding when a bounded drain is
saturated; older LAN receipts therefore cannot be overtaken by a newer
keyboard or pointer hit. Queue pressure in one slot cannot block the other.
Before pause or leave changes gameplay state, the App closes LAN admission
under the same mutex used by strike admission, then drains the fixed queues
using the closing gameplay slots. Thus every strike accepted before the gate
closed is dispatched exactly once, while a frame reaching admission afterward
is rejected without delaying the transition.
The supported logic-rate floor is 60 Hz, comfortably inside the 100 ms LAN
freshness window.
