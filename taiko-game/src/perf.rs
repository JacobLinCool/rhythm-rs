use std::time::Duration;

#[derive(Debug, Clone, Copy, Default)]
pub struct DurationStats {
    pub avg_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PerfSnapshot {
    pub frame: DurationStats,
    pub tick: DurationStats,
    pub input_dispatch: DurationStats,
    pub fps: f64,
    pub tps: f64,
}

#[derive(Debug, Default, Clone)]
pub struct PerfMeter {
    tick_samples: DurationHistogram,
    frame_samples: DurationHistogram,
    input_dispatch_samples: DurationHistogram,
}

impl PerfMeter {
    pub fn clear(&mut self) {
        self.tick_samples.clear();
        self.frame_samples.clear();
        self.input_dispatch_samples.clear();
    }

    pub fn record_tick(&mut self, elapsed: Duration) {
        self.tick_samples.record(elapsed);
    }

    pub fn record_frame(&mut self, elapsed: Duration) {
        self.frame_samples.record(elapsed);
    }

    pub fn record_input_dispatch(&mut self, elapsed: Duration) {
        self.input_dispatch_samples.record(elapsed);
    }

    pub fn snapshot(&self) -> PerfSnapshot {
        let tick = self.tick_samples.stats();
        let frame = self.frame_samples.stats();
        let input_dispatch = self.input_dispatch_samples.stats();

        let fps = if frame.avg_ms <= f64::EPSILON {
            0.0
        } else {
            1_000.0 / frame.avg_ms
        };
        let tps = if tick.avg_ms <= f64::EPSILON {
            0.0
        } else {
            1_000.0 / tick.avg_ms
        };

        PerfSnapshot {
            frame,
            tick,
            input_dispatch,
            fps,
            tps,
        }
    }
}

const HISTOGRAM_RESOLUTION_NS: u64 = 50_000;
const HISTOGRAM_BUCKETS: usize = 2_049;

#[derive(Debug, Clone)]
struct DurationHistogram {
    buckets: Vec<u64>,
    count: u64,
    sum_ns: u128,
    max_ns: u64,
}

impl Default for DurationHistogram {
    fn default() -> Self {
        Self {
            buckets: vec![0; HISTOGRAM_BUCKETS],
            count: 0,
            sum_ns: 0,
            max_ns: 0,
        }
    }
}

impl DurationHistogram {
    fn clear(&mut self) {
        self.buckets.fill(0);
        self.count = 0;
        self.sum_ns = 0;
        self.max_ns = 0;
    }

    fn record(&mut self, duration: Duration) {
        let nanos = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        let bucket = (nanos / HISTOGRAM_RESOLUTION_NS).min((HISTOGRAM_BUCKETS - 1) as u64) as usize;
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        self.count = self.count.saturating_add(1);
        self.sum_ns = self.sum_ns.saturating_add(u128::from(nanos));
        self.max_ns = self.max_ns.max(nanos);
    }

    fn stats(&self) -> DurationStats {
        if self.count == 0 {
            return DurationStats::default();
        }
        DurationStats {
            avg_ms: self.sum_ns as f64 / self.count as f64 / 1_000_000.0,
            p95_ms: self.quantile_ms(95, 100),
            p99_ms: self.quantile_ms(99, 100),
            max_ms: self.max_ns as f64 / 1_000_000.0,
        }
    }

    fn quantile_ms(&self, numerator: u64, denominator: u64) -> f64 {
        let target = self
            .count
            .saturating_mul(numerator)
            .saturating_add(denominator - 1)
            / denominator;
        let mut cumulative = 0_u64;
        for (index, count) in self.buckets.iter().copied().enumerate() {
            cumulative = cumulative.saturating_add(count);
            if cumulative >= target {
                if index == HISTOGRAM_BUCKETS - 1 {
                    return self.max_ns as f64 / 1_000_000.0;
                }
                return ((index as u64 + 1) * HISTOGRAM_RESOLUTION_NS) as f64 / 1_000_000.0;
            }
        }
        self.max_ns as f64 / 1_000_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_empty_are_zero() {
        let meter = PerfMeter::default();
        let snapshot = meter.snapshot();
        assert_eq!(snapshot.frame.avg_ms, 0.0);
        assert_eq!(snapshot.tick.avg_ms, 0.0);
        assert_eq!(snapshot.input_dispatch.avg_ms, 0.0);
    }

    #[test]
    fn stats_p95_is_computed() {
        let mut meter = PerfMeter::default();
        for ms in [1_u64, 2, 3, 4, 5, 6, 7, 8, 9, 20] {
            meter.record_tick(Duration::from_millis(ms));
        }

        let snapshot = meter.snapshot();
        assert!(snapshot.tick.p95_ms >= 9.0);
        assert_eq!(snapshot.tick.max_ms, 20.0);
    }

    #[test]
    fn sample_storage_is_bounded_for_long_sessions() {
        let mut meter = PerfMeter::default();
        for _ in 0..1_000_000 {
            meter.record_frame(Duration::from_micros(250));
        }
        assert_eq!(meter.frame_samples.buckets.len(), HISTOGRAM_BUCKETS);
        assert_eq!(meter.frame_samples.count, 1_000_000);
    }

    #[test]
    fn average_preserves_sub_microsecond_measurements() {
        let mut meter = PerfMeter::default();
        meter.record_tick(Duration::from_nanos(500));
        assert_eq!(meter.snapshot().tick.avg_ms, 0.000_5);
    }
}
