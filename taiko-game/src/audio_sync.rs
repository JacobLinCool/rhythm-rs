use std::time::{Duration, Instant};

const OBSERVATION_INTERVAL: Duration = Duration::from_millis(250);
const STARTUP_GRACE: Duration = Duration::from_millis(500);
const SETTLED_DRIFT_SECONDS: f64 = 0.010;
const HARD_RESYNC_DRIFT_SECONDS: f64 = 0.100;
const MIN_PLAYBACK_RATE: f64 = 0.98;
const MAX_PLAYBACK_RATE: f64 = 1.02;
const PROPORTIONAL_GAIN: f64 = 0.15;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AudioSyncDecision {
    None,
    SetPlaybackRate(f64),
    SeekTo(f64),
}

#[derive(Debug, Clone)]
pub(crate) struct AudioSyncController {
    started_at: Instant,
    next_observation_at: Instant,
    playback_rate: f64,
    last_drift_seconds: f64,
    correction_count: u64,
}

impl AudioSyncController {
    pub(crate) fn started(now: Instant) -> Self {
        Self {
            started_at: now,
            next_observation_at: now + OBSERVATION_INTERVAL,
            playback_rate: 1.0,
            last_drift_seconds: 0.0,
            correction_count: 0,
        }
    }

    pub(crate) fn observe(
        &mut self,
        now: Instant,
        authoritative_seconds: f64,
        audio_seconds: f64,
    ) -> AudioSyncDecision {
        if now < self.next_observation_at {
            return AudioSyncDecision::None;
        }
        self.next_observation_at = now + OBSERVATION_INTERVAL;

        if now.saturating_duration_since(self.started_at) < STARTUP_GRACE
            || !authoritative_seconds.is_finite()
            || !audio_seconds.is_finite()
            || authoritative_seconds < 0.0
            || audio_seconds < 0.0
        {
            return AudioSyncDecision::None;
        }

        let drift = audio_seconds - authoritative_seconds;
        self.last_drift_seconds = drift;
        if drift.abs() >= HARD_RESYNC_DRIFT_SECONDS {
            self.playback_rate = 1.0;
            self.correction_count = self.correction_count.saturating_add(1);
            return AudioSyncDecision::SeekTo(authoritative_seconds);
        }

        let desired_rate = if drift.abs() <= SETTLED_DRIFT_SECONDS {
            1.0
        } else {
            (1.0 - drift * PROPORTIONAL_GAIN).clamp(MIN_PLAYBACK_RATE, MAX_PLAYBACK_RATE)
        };
        if (desired_rate - self.playback_rate).abs() < 0.000_5 {
            return AudioSyncDecision::None;
        }

        self.playback_rate = desired_rate;
        self.correction_count = self.correction_count.saturating_add(1);
        AudioSyncDecision::SetPlaybackRate(desired_rate)
    }

    #[cfg(test)]
    pub(crate) fn correction_count(&self) -> u64 {
        self.correction_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_samples_during_backend_startup_grace() {
        let started = Instant::now();
        let mut sync = AudioSyncController::started(started);
        assert_eq!(
            sync.observe(started + Duration::from_millis(250), 0.25, 0.0),
            AudioSyncDecision::None
        );
    }

    #[test]
    fn bounded_rate_correction_moves_in_the_right_direction() {
        let started = Instant::now();
        let mut behind = AudioSyncController::started(started);
        let decision = behind.observe(started + Duration::from_secs(1), 1.0, 0.95);
        assert!(matches!(
            decision,
            AudioSyncDecision::SetPlaybackRate(rate) if rate > 1.0 && rate <= MAX_PLAYBACK_RATE
        ));

        let mut ahead = AudioSyncController::started(started);
        let decision = ahead.observe(started + Duration::from_secs(1), 1.0, 1.05);
        assert!(matches!(
            decision,
            AudioSyncDecision::SetPlaybackRate(rate)
                if (MIN_PLAYBACK_RATE..1.0).contains(&rate)
        ));
    }

    #[test]
    fn large_drift_uses_one_explicit_seek() {
        let started = Instant::now();
        let mut sync = AudioSyncController::started(started);
        assert_eq!(
            sync.observe(started + Duration::from_secs(1), 1.0, 0.75),
            AudioSyncDecision::SeekTo(1.0)
        );
        assert_eq!(sync.correction_count(), 1);
    }

    #[test]
    fn invalid_clock_samples_never_mutate_audio() {
        let started = Instant::now();
        let mut sync = AudioSyncController::started(started);
        assert_eq!(
            sync.observe(started + Duration::from_secs(1), f64::NAN, 0.0),
            AudioSyncDecision::None
        );
        assert_eq!(sync.correction_count(), 0);
    }
}
