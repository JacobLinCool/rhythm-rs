use std::collections::VecDeque;
use std::time::{Duration, Instant};

use taiko_multiplayer_protocol::{ClockProbeAck, ClockProbeToken, ClockQuality, TimeSyncReceipt};
use thiserror::Error;

use super::clock::ProcessClock;
use super::limits::{MAX_CLOCK_JITTER_MS, MAX_CLOCK_RTT_MS, MIN_CLOCK_SAMPLES};

const MAX_PENDING_PROBES: usize = 16;
const MAX_CLOCK_SAMPLES: usize = 64;
const PENDING_PROBE_TTL: Duration = Duration::from_secs(10);
const CLOCK_EVIDENCE_WINDOW: Duration = Duration::from_secs(10);
const MIN_SAMPLE_SPACING: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum ClockProbeError {
    #[error("clock probe nonce is already pending")]
    DuplicateNonce,
    #[error("too many clock probes are awaiting receipts")]
    PendingCapacity,
    #[error("clock probe receipt does not name a pending probe")]
    UnknownNonce,
    #[error("clock probe receipt token does not match its challenge")]
    TokenMismatch,
    #[error("secure clock probe token generation failed")]
    EntropyUnavailable,
}

struct PendingProbe {
    nonce: u64,
    token: ClockProbeToken,
    enqueued_at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct ProbeSample {
    observed_at: Instant,
    round_trip: Duration,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct VerifiedClockQuality {
    quality: ClockQuality,
    valid_until: Instant,
}

impl VerifiedClockQuality {
    pub(crate) fn is_ready_at(self, now: Instant) -> bool {
        now <= self.valid_until && quality_is_ready(self.quality)
    }

    #[cfg(test)]
    pub(crate) fn valid_until(self) -> Instant {
        self.valid_until
    }

    #[cfg(test)]
    pub(crate) fn ready_for_tests(now: Instant) -> Self {
        Self {
            quality: ClockQuality {
                accepted_samples: MIN_CLOCK_SAMPLES,
                p95_rtt_ms: 1,
                jitter_ms: 0,
            },
            valid_until: now + CLOCK_EVIDENCE_WINDOW,
        }
    }
}

#[derive(Debug)]
pub(crate) struct AcknowledgedClockProbe {
    pub(crate) ack: ClockProbeAck,
    pub(crate) verified: VerifiedClockQuality,
}

#[derive(Default)]
pub(crate) struct ClockProbeVerifier {
    pending: VecDeque<PendingProbe>,
    samples: VecDeque<ProbeSample>,
}

impl ClockProbeVerifier {
    pub(crate) fn issue(
        &mut self,
        nonce: u64,
        enqueued_at: Instant,
    ) -> Result<ClockProbeToken, ClockProbeError> {
        self.prune(enqueued_at);
        if self.pending.iter().any(|probe| probe.nonce == nonce) {
            return Err(ClockProbeError::DuplicateNonce);
        }
        if self.pending.len() >= MAX_PENDING_PROBES {
            return Err(ClockProbeError::PendingCapacity);
        }

        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(|_| ClockProbeError::EntropyUnavailable)?;
        let token = ClockProbeToken::parse(hex::encode(random))
            .map_err(|_| ClockProbeError::EntropyUnavailable)?;
        self.pending.push_back(PendingProbe {
            nonce,
            token: token.clone(),
            enqueued_at,
        });
        Ok(token)
    }

    pub(crate) fn acknowledge(
        &mut self,
        receipt: &TimeSyncReceipt,
        observed_at: Instant,
        clock: &ProcessClock,
    ) -> Result<AcknowledgedClockProbe, ClockProbeError> {
        self.prune(observed_at);
        let Some(index) = self
            .pending
            .iter()
            .position(|probe| probe.nonce == receipt.nonce)
        else {
            return Err(ClockProbeError::UnknownNonce);
        };
        let pending = self
            .pending
            .remove(index)
            .expect("clock probe index came from the same queue");
        if pending.token != receipt.probe_token {
            return Err(ClockProbeError::TokenMismatch);
        }

        let spaced = self.samples.back().is_none_or(|sample| {
            observed_at.saturating_duration_since(sample.observed_at) >= MIN_SAMPLE_SPACING
        });
        if spaced {
            if self.samples.len() == MAX_CLOCK_SAMPLES {
                self.samples.pop_front();
            }
            self.samples.push_back(ProbeSample {
                observed_at,
                round_trip: observed_at.saturating_duration_since(pending.enqueued_at),
            });
        }
        Ok(self.ack(receipt.nonce, observed_at, clock))
    }

    pub(crate) fn verified_quality(&mut self, now: Instant) -> VerifiedClockQuality {
        self.prune(now);
        let quality = self.quality();
        let valid_until = self.evidence_valid_until(now, quality);
        VerifiedClockQuality {
            quality,
            valid_until,
        }
    }

    fn ack(&mut self, nonce: u64, now: Instant, clock: &ProcessClock) -> AcknowledgedClockProbe {
        let verified = self.verified_quality(now);
        AcknowledgedClockProbe {
            ack: ClockProbeAck {
                nonce,
                quality: verified.quality,
                ready: verified.is_ready_at(now),
                valid_until_server_us: clock.server_us(verified.valid_until),
            },
            verified,
        }
    }

    fn quality(&self) -> ClockQuality {
        quality_for(self.samples.iter().copied())
    }

    fn evidence_valid_until(&self, now: Instant, current: ClockQuality) -> Instant {
        if !quality_is_ready(current) {
            return now;
        }
        for expired_count in 1..=self.samples.len() {
            let expires_at = self.samples[expired_count - 1].observed_at + CLOCK_EVIDENCE_WINDOW;
            let remaining = quality_for(self.samples.iter().copied().skip(expired_count));
            if !quality_is_ready(remaining) {
                return expires_at;
            }
        }
        now
    }

    fn prune(&mut self, now: Instant) {
        self.pending
            .retain(|probe| now.saturating_duration_since(probe.enqueued_at) <= PENDING_PROBE_TTL);
        self.samples.retain(|sample| {
            now.saturating_duration_since(sample.observed_at) <= CLOCK_EVIDENCE_WINDOW
        });
    }
}

fn quality_for(samples: impl IntoIterator<Item = ProbeSample>) -> ClockQuality {
    let samples = samples.into_iter().collect::<Vec<_>>();
    let p95_rtt_ms = percentile(
        samples
            .iter()
            .map(|sample| duration_millis_ceil(sample.round_trip)),
        95,
    )
    .unwrap_or_default();
    let mut previous = None;
    let jitter = samples.iter().filter_map(|sample| {
        let current = duration_millis_ceil(sample.round_trip);
        let value = previous.map(|prior: u32| prior.abs_diff(current));
        previous = Some(current);
        value
    });
    ClockQuality {
        accepted_samples: u16::try_from(samples.len()).unwrap_or(u16::MAX),
        p95_rtt_ms,
        jitter_ms: percentile(jitter, 95).unwrap_or_default(),
    }
}

fn quality_is_ready(quality: ClockQuality) -> bool {
    quality.accepted_samples >= MIN_CLOCK_SAMPLES
        && quality.p95_rtt_ms <= MAX_CLOCK_RTT_MS
        && quality.jitter_ms <= MAX_CLOCK_JITTER_MS
}

fn duration_millis_ceil(duration: Duration) -> u32 {
    duration
        .as_micros()
        .div_ceil(1_000)
        .min(u128::from(u32::MAX)) as u32
}

fn percentile(values: impl IntoIterator<Item = u32>, percentile: usize) -> Option<u32> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let index = (values.len() - 1).saturating_mul(percentile).div_ceil(100);
    values.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue_and_ack(
        verifier: &mut ClockProbeVerifier,
        clock: &ProcessClock,
        nonce: u64,
        issued_at: Instant,
        round_trip: Duration,
    ) -> ClockProbeAck {
        let token = verifier.issue(nonce, issued_at).expect("issue probe");
        verifier
            .acknowledge(
                &TimeSyncReceipt {
                    nonce,
                    probe_token: token,
                },
                issued_at + round_trip,
                clock,
            )
            .expect("acknowledge probe")
            .ack
    }

    #[test]
    fn stable_server_timed_samples_become_ready() {
        let start = Instant::now();
        let clock = ProcessClock::from_epoch(start, 1_000_000);
        let mut verifier = ClockProbeVerifier::default();
        let mut ack = None;
        for index in 0..4 {
            ack = Some(issue_and_ack(
                &mut verifier,
                &clock,
                index,
                start + Duration::from_millis(index * 250),
                Duration::from_millis(20),
            ));
        }
        let ack = ack.expect("last acknowledgement");
        assert!(ack.ready);
        assert_eq!(ack.quality.accepted_samples, 4);
        assert_eq!(ack.quality.p95_rtt_ms, 20);
        assert_eq!(ack.quality.jitter_ms, 0);
    }

    #[test]
    fn unknown_forged_and_replayed_receipts_are_rejected() {
        let start = Instant::now();
        let clock = ProcessClock::from_epoch(start, 1_000_000);
        let mut verifier = ClockProbeVerifier::default();
        let token = verifier.issue(7, start).expect("issue probe");
        let forged = ClockProbeToken::parse("f".repeat(64)).expect("forged token");
        assert_eq!(
            verifier
                .acknowledge(
                    &TimeSyncReceipt {
                        nonce: 7,
                        probe_token: forged,
                    },
                    start + Duration::from_millis(10),
                    &clock,
                )
                .expect_err("forged token"),
            ClockProbeError::TokenMismatch
        );
        assert_eq!(
            verifier
                .acknowledge(
                    &TimeSyncReceipt {
                        nonce: 7,
                        probe_token: token,
                    },
                    start + Duration::from_millis(20),
                    &clock,
                )
                .expect_err("consumed challenge cannot be replayed"),
            ClockProbeError::UnknownNonce
        );
    }

    #[test]
    fn poor_or_expired_evidence_is_not_ready() {
        let start = Instant::now();
        let clock = ProcessClock::from_epoch(start, 1_000_000);
        let mut verifier = ClockProbeVerifier::default();
        for index in 0..4 {
            issue_and_ack(
                &mut verifier,
                &clock,
                index,
                start + Duration::from_millis(index * 400),
                Duration::from_millis(300),
            );
        }
        assert!(!verifier
            .verified_quality(start + Duration::from_secs(2))
            .is_ready_at(start + Duration::from_secs(2)));
        assert!(!verifier
            .verified_quality(start + Duration::from_secs(12))
            .is_ready_at(start + Duration::from_secs(12)));
    }

    #[test]
    fn high_server_observed_jitter_is_not_ready() {
        let start = Instant::now();
        let clock = ProcessClock::from_epoch(start, 1_000_000);
        let mut verifier = ClockProbeVerifier::default();
        for (index, round_trip_ms) in [10, 200, 10, 200].into_iter().enumerate() {
            issue_and_ack(
                &mut verifier,
                &clock,
                index as u64,
                start + Duration::from_millis(index as u64 * 250),
                Duration::from_millis(round_trip_ms),
            );
        }
        let now = start + Duration::from_secs(2);
        let verified = verifier.verified_quality(now);
        assert!(!verified.is_ready_at(now));
        assert_eq!(verified.quality.p95_rtt_ms, 200);
        assert_eq!(verified.quality.jitter_ms, 190);
    }

    #[test]
    fn pending_and_sample_collections_are_hard_bounded() {
        let start = Instant::now();
        let clock = ProcessClock::from_epoch(start, 1_000_000);
        let mut verifier = ClockProbeVerifier::default();
        for nonce in 0..MAX_PENDING_PROBES {
            verifier
                .issue(nonce as u64, start)
                .expect("pending capacity");
        }
        assert!(!verifier.verified_quality(start).is_ready_at(start));
        assert_eq!(
            verifier
                .issue(MAX_PENDING_PROBES as u64, start)
                .expect_err("capacity must be hard"),
            ClockProbeError::PendingCapacity
        );

        let mut samples = ClockProbeVerifier::default();
        for nonce in 0..(MAX_CLOCK_SAMPLES + 8) {
            issue_and_ack(
                &mut samples,
                &clock,
                nonce as u64,
                start + Duration::from_millis(nonce as u64 * 100),
                Duration::from_millis(1),
            );
        }
        assert_eq!(samples.samples.len(), MAX_CLOCK_SAMPLES);
    }

    #[test]
    fn evidence_lease_uses_the_newest_still_ready_suffix() {
        let start = Instant::now();
        let clock = ProcessClock::from_epoch(start, 1_000_000);
        let mut verifier = ClockProbeVerifier::default();
        for (index, seconds) in [0, 2, 4, 6, 8].into_iter().enumerate() {
            issue_and_ack(
                &mut verifier,
                &clock,
                index as u64,
                start + Duration::from_secs(seconds),
                Duration::from_millis(20),
            );
        }

        let now = start + Duration::from_secs(9);
        let verified = verifier.verified_quality(now);
        assert!(verified.is_ready_at(now));
        assert_eq!(
            verified.valid_until(),
            start + Duration::from_secs(12) + Duration::from_millis(20),
            "dropping the oldest sample still leaves four fresh ready samples"
        );
    }
}
