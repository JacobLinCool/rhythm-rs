use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;

/// A cloneable handle to the single wall-clock/monotonic epoch used by multiplayer.
///
/// The epoch is sampled once when the registry starts. Every room receives a clone
/// of this handle, so registry time-sync replies and serialized room deadlines stay
/// in the same server-time coordinate system.
#[derive(Clone, Debug)]
pub(super) struct ProcessClock {
    epoch: Arc<ClockEpoch>,
}

#[derive(Debug)]
struct ClockEpoch {
    monotonic_origin: Instant,
    server_origin_us: u64,
}

impl ProcessClock {
    pub(super) fn from_system_time() -> anyhow::Result<Self> {
        let monotonic_before = Instant::now();
        let system_now = SystemTime::now();
        let monotonic_after = Instant::now();
        let unix_duration = system_now
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?;
        let server_origin_us = u64::try_from(unix_duration.as_micros())
            .context("system clock does not fit the multiplayer microsecond timebase")?;
        let sampling_span = monotonic_after.duration_since(monotonic_before);
        let monotonic_origin = monotonic_before + sampling_span / 2;

        Ok(Self::from_epoch(monotonic_origin, server_origin_us))
    }

    pub(super) fn from_epoch(monotonic_origin: Instant, server_origin_us: u64) -> Self {
        Self {
            epoch: Arc::new(ClockEpoch {
                monotonic_origin,
                server_origin_us,
            }),
        }
    }

    pub(super) fn now_us(&self) -> u64 {
        self.server_us(Instant::now())
    }

    pub(super) fn uptime(&self) -> Duration {
        Instant::now().saturating_duration_since(self.epoch.monotonic_origin)
    }

    pub(super) fn server_us(&self, instant: Instant) -> u64 {
        if instant >= self.epoch.monotonic_origin {
            self.epoch.server_origin_us.saturating_add(duration_us(
                instant.duration_since(self.epoch.monotonic_origin),
            ))
        } else {
            self.epoch.server_origin_us.saturating_sub(duration_us(
                self.epoch.monotonic_origin.duration_since(instant),
            ))
        }
    }
}

fn duration_us(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_handles_share_one_deterministic_epoch() {
        let origin = Instant::now();
        let clock = ProcessClock::from_epoch(origin, 9_000_000);
        let clone = clock.clone();

        assert!(Arc::ptr_eq(&clock.epoch, &clone.epoch));
        assert_eq!(clock.server_us(origin), 9_000_000);
        assert_eq!(
            clone.server_us(origin + Duration::from_micros(123_456)),
            9_123_456
        );
        assert_eq!(
            clone.server_us(origin - Duration::from_micros(456)),
            8_999_544
        );
    }
}
