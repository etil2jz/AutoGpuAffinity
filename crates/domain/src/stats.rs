/// Frame rate statistics computed exactly like the original Python implementation.
#[derive(Debug, Clone)]
pub struct FrameStats {
    /// Slowest frame first.
    sorted_frametimes: Vec<f64>,
    total: f64,
    mean_fps: f64,
    slowest: f64,
    fastest: f64,
}

impl FrameStats {
    /// Non-positive or non-finite samples (first frame at 0, `NA`...) would divide by zero and are dropped.
    pub fn new(frametimes: &[f64]) -> Option<Self> {
        let mut sorted_frametimes: Vec<f64> = frametimes
            .iter()
            .copied()
            .filter(|frametime| frametime.is_finite() && *frametime > 0.0)
            .collect();
        sorted_frametimes.sort_by(|a, b| b.total_cmp(a));
        let slowest = *sorted_frametimes.first()?;
        let fastest = *sorted_frametimes.last()?;
        let total: f64 = sorted_frametimes.iter().sum();
        let mean_fps = 1000.0 / (total / sorted_frametimes.len() as f64);
        Some(Self {
            sorted_frametimes,
            total,
            mean_fps,
            slowest,
            fastest,
        })
    }

    pub fn len(&self) -> usize {
        self.sorted_frametimes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sorted_frametimes.is_empty()
    }

    pub fn maximum(&self) -> f64 {
        1000.0 / self.fastest
    }

    pub fn minimum(&self) -> f64 {
        1000.0 / self.slowest
    }

    pub fn average(&self) -> f64 {
        self.mean_fps
    }

    /// Bessel-corrected deviation of per-frame FPS around the mean.
    pub fn stdev(&self) -> f64 {
        if self.len() < 2 {
            return 0.0;
        }
        let squared_deviations: f64 = self
            .sorted_frametimes
            .iter()
            .map(|frametime| (1000.0 / frametime - self.mean_fps).powi(2))
            .sum();
        (squared_deviations / (self.len() - 1) as f64).sqrt()
    }

    pub fn percentile(&self, value: f64) -> f64 {
        let rank = (value / 100.0 * self.len() as f64).ceil() as usize;
        let index = rank.saturating_sub(1);
        1000.0 / self.sorted_frametimes.get(index).copied().unwrap_or(self.fastest)
    }

    /// FPS of the frame at which the slowest frames add up to `value` % of the total time.
    pub fn low(&self, value: f64) -> f64 {
        let threshold = value / 100.0 * self.total;
        let mut current_total = 0.0;
        for frametime in &self.sorted_frametimes {
            current_total += frametime;
            if current_total >= threshold {
                return 1000.0 / frametime;
            }
        }
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn basic_metrics() {
        let stats = FrameStats::new(&[20.0, 10.0, 40.0]).unwrap();
        let mean: f64 = 3000.0 / 70.0;
        assert!(close(stats.maximum(), 100.0));
        assert!(close(stats.minimum(), 25.0));
        assert!(close(stats.average(), mean));
        let expected = (((100.0 - mean).powi(2) + (50.0 - mean).powi(2) + (25.0 - mean).powi(2)) / 2.0).sqrt();
        assert!(close(stats.stdev(), expected));
    }

    #[test]
    fn percentiles_and_lows_follow_the_python_implementation() {
        // 99 frames at 10 ms and one at 50 ms, 1040 ms in total.
        let mut frametimes = vec![10.0; 99];
        frametimes.push(50.0);
        let stats = FrameStats::new(&frametimes).unwrap();
        assert!(close(stats.percentile(1.0), 20.0));
        assert!(close(stats.percentile(10.0), 100.0));
        assert!(close(stats.percentile(0.005), 20.0));
        assert!(close(stats.low(1.0), 20.0));
        // 10 % of 1040 ms is reached at the 7th frame (50 + 6 x 10 = 110).
        assert!(close(stats.low(10.0), 100.0));
    }

    #[test]
    fn invalid_samples_are_ignored() {
        assert!(FrameStats::new(&[]).is_none());
        assert!(FrameStats::new(&[0.0, f64::NAN, -1.0]).is_none());
        let stats = FrameStats::new(&[0.0, 10.0]).unwrap();
        assert_eq!(stats.len(), 1);
        assert!(close(stats.maximum(), 100.0));
        assert_eq!(stats.stdev(), 0.0);
    }
}
