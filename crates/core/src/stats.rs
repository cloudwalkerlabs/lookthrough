//! Latency samples with percentile summaries, for the per-update numbers
//! milestone 2 measures.

use std::time::Duration;

/// Collects durations and reports percentiles. Not a streaming estimator:
/// keep it to one reporting window, then [`Samples::clear`].
#[derive(Debug, Default, Clone)]
pub struct Samples {
    d: Vec<Duration>,
}

#[derive(Debug, Clone, Copy)]
pub struct Summary {
    pub count: usize,
    pub p50: Duration,
    pub p90: Duration,
    pub p99: Duration,
    pub max: Duration,
}

impl Samples {
    pub fn record(&mut self, d: Duration) {
        self.d.push(d);
    }

    pub fn len(&self) -> usize {
        self.d.len()
    }

    pub fn is_empty(&self) -> bool {
        self.d.is_empty()
    }

    pub fn clear(&mut self) {
        self.d.clear();
    }

    pub fn summary(&self) -> Option<Summary> {
        if self.d.is_empty() {
            return None;
        }
        let mut d = self.d.clone();
        d.sort_unstable();
        let pct = |p: f64| d[((d.len() - 1) as f64 * p).round() as usize];
        Some(Summary {
            count: d.len(),
            p50: pct(0.5),
            p90: pct(0.9),
            p99: pct(0.99),
            max: d[d.len() - 1],
        })
    }
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        write!(
            f,
            "n={} p50={:.2}ms p90={:.2}ms p99={:.2}ms max={:.2}ms",
            self.count,
            ms(self.p50),
            ms(self.p90),
            ms(self.p99),
            ms(self.max)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let mut s = Samples::default();
        for ms in 1..=100 {
            s.record(Duration::from_millis(ms));
        }
        let sum = s.summary().unwrap();
        assert_eq!(sum.count, 100);
        assert_eq!(sum.p50, Duration::from_millis(51));
        assert_eq!(sum.p99, Duration::from_millis(99));
        assert_eq!(sum.max, Duration::from_millis(100));
    }
}
