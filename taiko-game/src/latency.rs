use std::time::{Duration, Instant};

pub struct LatencyMeter {
    count: u64,
    ticks: Vec<Instant>,
}

impl LatencyMeter {
    pub fn new() -> Self {
        Self {
            count: 0,
            ticks: Vec::new(),
        }
    }

    pub fn tick(&mut self) {
        self.count += 1;

        let now = Instant::now();
        self.ticks.push(now);
        if self.ticks.len() > 100 {
            self.ticks.remove(0);
        }
    }

    pub fn latency_ms(&self) -> f64 {
        if self.ticks.len() < 2 {
            return 0.0;
        }

        let duration_sum = self
            .ticks
            .last()
            .unwrap()
            .duration_since(*self.ticks.first().unwrap());

        duration_sum.as_secs_f64() * 1000.0 / self.ticks.len() as f64
    }
}
