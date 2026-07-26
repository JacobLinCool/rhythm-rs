# Controller Input Architecture

## Boundary

Controller sources normalize physical strikes; they never judge notes, assign
score, estimate remote authority time, or mutate a runtime directly. The
canonical four-pad value remains `rhythm_mode_taiko::TaikoAction`, preserving
left/right and Don/Kat without a parallel mouse or web action model.

The supported sources are:

- keyboard bindings;
- native contact frames from a MacBook trackpad on macOS;
- a terminal pointer surface driven by a mouse left-click; and
- an embedded, explicitly started HTTP/WebSocket controller for trusted LAN
  phones.

Native Mac contact and terminal pointer input are deliberately separate
sources. `MacTrackpadContact` reads contact identity and normalized horizontal
position from macOS's private `MultitouchSupport.framework`; it does not read
click state or pressure. `TerminalPointer` accepts a mouse left-button down in
a rendered terminal cell. There is no conversion or fallback between them.
The browser surface uses Pointer Events and supports simultaneous phone
touches.

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

`MacTrackpad`

- is compiled as a native source only on macOS and dynamically probes
  `MultitouchSupport.framework` plus the default multitouch device at startup;
- reports an explicit unavailable/unsupported capability when the framework
  or device cannot be opened, including on non-macOS platforms, and never
  substitutes terminal clicks;
- divides normalized trackpad X into four equal regions ordered
  `LEFT_KAT | LEFT_DON | RIGHT_DON | RIGHT_KAT`;
- emits one hit for each newly touching contact identity, without consulting
  pressure or click state;
- retains contact identity while a finger is held or slides, so only a fully
  lifted and newly touching finger can emit another hit;
- enters a neutral-arm state whenever ownership changes between P1, P2, and
  Off while any contact is active. It resumes admission only after an empty
  contact frame, preventing a held finger from becoming a strike for the new
  owner;
- limits one framework frame to 32 distinct contacts and writes admitted hits
  to a bounded 64-entry queue. Queue overflow is counted and dropped rather
  than blocking the framework callback;
- timestamps the entire callback frame at receipt, before UI dispatch, and
  preserves that observation time through `ControllerStrike`;
- unregisters the callback, stops the device, releases framework ownership,
  and clears callback state during explicit application shutdown. Partial
  startup is rolled back in reverse order.

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
LAN bind IP, start or stop the controller server, independently choose which
local player receives native Mac trackpad contact and terminal mouse clicks,
show a scannable pairing QR, copy a pairing URL, and rotate either player
credential. The Controller Setup entry point and its position in the flow are
unchanged. QR rendering includes the mandatory quiet zone and uses explicit
black modules on white cells for camera contrast.

The Mac contact row is assignable only after the native capability probe
succeeds. The four pads shown in Controller Setup are a visual key for the
four equal regions of the physical trackpad; they are not terminal hit-test
targets for this source. Reassignment requires a neutral frame before the new
slot is armed. The terminal mouse row instead installs a rendered
`DrumSurfaceLayout` and requires an actual left-button down inside it.

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
physical press. Native Mac strikes are stamped when the framework delivers the
contact frame. Web strikes are stamped when the WebSocket text frame arrives.
The App later converts that same `Instant` with the existing calibrated local
clock or online authority estimate, so a UI scheduling delay does not become a
judgement delay.

The TUI reads at most 64 raw events into one batch, delivers every semantic
input from that batch before a due logic tick, and samples one audio clock
anchor for the entire App dispatch batch. Simultaneous P1/P2 hits therefore
retain equal timing while the fixed batch limit still prevents input floods
from starving scheduled work.

Native contact callbacks enqueue into their own fixed 64-hit queue. At every
TUI event boundary, the App first acquires the callback-state mutex to establish
a quiescent producer barrier, then drains that queue and compares each native
`observed_at` with the keyboard or pointer event timestamp. Earlier native hits
are admitted before that UI event; later native hits are admitted after the UI
transition and only if the selected player remains active. A target switch
holds the same mutex, clears hits from the old owner, changes the reducer target,
and only then releases the callback; a new owner's first hit therefore cannot
be erased by the old-owner flush. The reducer's neutral-arm rule prevents a
contact spanning that boundary from being reinterpreted. This ordered admission
keeps pause, leave, Controller Setup, and player-assignment transitions
deterministic. The callback never waits on queue capacity; only these short
state-boundary sections serialize it with the UI.

Each LAN slot has its own fixed queue. The App drains a fixed maximum per tick
and rejects stale receipts instead of replaying an input burst after a pause or
suspend. It holds all already-collected sources until the LAN ingress
completion epoch is quiescent, and it keeps holding when a bounded drain is
saturated; older LAN receipts therefore cannot be overtaken by a newer
keyboard, pointer, or native contact hit. Queue pressure in one slot cannot
block the other.
Before pause or leave changes gameplay state, the App closes LAN admission
under the same mutex used by strike admission, then drains the fixed queues
using the closing gameplay slots. Thus every strike accepted before the gate
closed is dispatched exactly once, while a frame reaching admission afterward
is rejected without delaying the transition.
The supported logic-rate floor is 60 Hz, comfortably inside the 100 ms LAN
freshness window.
