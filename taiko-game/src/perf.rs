use std::time::Duration;

#[derive(Debug, Clone, Copy, Default)]
pub struct DurationStats {
    pub avg_ms: f64,
    pub p95_ms: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PerfSnapshot {
    pub frame: DurationStats,
    pub tick: DurationStats,
    pub fps: f64,
    pub tps: f64,
}

#[derive(Debug, Default, Clone)]
pub struct PerfMeter {
    tick_samples: Vec<f64>,
    frame_samples: Vec<f64>,
}

impl PerfMeter {
    pub fn clear(&mut self) {
        self.tick_samples.clear();
        self.frame_samples.clear();
    }

    pub fn record_tick(&mut self, elapsed: Duration) {
        self.tick_samples.push(duration_ms(elapsed));
    }

    pub fn record_frame(&mut self, elapsed: Duration) {
        self.frame_samples.push(duration_ms(elapsed));
    }

    pub fn snapshot(&self) -> PerfSnapshot {
        let tick = stats(&self.tick_samples);
        let frame = stats(&self.frame_samples);

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
            fps,
            tps,
        }
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn stats(values: &[f64]) -> DurationStats {
    if values.is_empty() {
        return DurationStats::default();
    }

    let avg_ms = values.iter().copied().sum::<f64>() / values.len() as f64;

    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p95_index = ((sorted.len() - 1) as f64 * 0.95).round() as usize;
    let p95_ms = sorted[p95_index.min(sorted.len() - 1)];

    DurationStats { avg_ms, p95_ms }
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
    }

    #[test]
    fn stats_p95_is_computed() {
        let mut meter = PerfMeter::default();
        for ms in [1_u64, 2, 3, 4, 5, 6, 7, 8, 9, 20] {
            meter.record_tick(Duration::from_millis(ms));
        }

        let snapshot = meter.snapshot();
        assert!(snapshot.tick.p95_ms >= 9.0);
    }
}
