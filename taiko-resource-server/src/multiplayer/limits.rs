use std::time::Duration;

pub(super) const ROOM_MAX_PLAYERS: usize = 4;
pub(super) const ROOM_MAX_SPECTATORS: usize = 64;
pub(super) const MAX_CONCURRENT_SESSIONS: usize = 512;
pub(super) const MAX_CONCURRENT_ROOMS: usize = 256;
pub(super) const SESSION_MESSAGE_RATE_PER_SECOND: u64 = 64;
pub(super) const SESSION_MESSAGE_BURST: u64 = 128;

pub(super) const ROOM_MAILBOX_CAPACITY: usize = 256;
pub(super) const REGISTRY_LIFECYCLE_CAPACITY: usize = 64;
pub(super) const RELIABLE_OUTBOUND_CAPACITY: usize = 64;
pub(super) const COMMAND_ACK_CACHE_CAPACITY: usize = 64;
pub(super) const ROOM_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) const RECONNECT_GRACE: Duration = Duration::from_secs(15);
pub(super) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
pub(super) const SESSION_LEASE: Duration = Duration::from_secs(8);
pub(crate) const SESSION_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const UNAFFILIATED_SESSION_TTL: Duration = Duration::from_secs(30);
pub(super) const LOBBY_IDLE_TTL: Duration = Duration::from_secs(15 * 60);
pub(super) const MATCH_TTL: Duration = Duration::from_secs(15 * 60);
pub(super) const FINALIZATION_GRACE: Duration = Duration::from_secs(1);
pub(super) const LIVE_STATE_INTERVAL: Duration = Duration::from_millis(50);
pub(super) const FINISHED_TTL: Duration = Duration::from_secs(2 * 60);
pub(super) const ROOM_MAX_LIFETIME: Duration = Duration::from_secs(6 * 60 * 60);

pub(super) const INPUT_LATENESS: Duration = Duration::from_millis(250);
pub(super) const MIN_CLOCK_SAMPLES: u16 = 4;
pub(super) const MAX_CLOCK_RTT_MS: u32 = 250;
pub(super) const MAX_CLOCK_JITTER_MS: u32 = 100;

#[cfg(any(test, feature = "test-fast-countdown"))]
pub(super) const MATCH_COUNTDOWN: Duration = Duration::from_millis(500);
#[cfg(not(any(test, feature = "test-fast-countdown")))]
pub(super) const MATCH_COUNTDOWN: Duration = Duration::from_secs(3);
