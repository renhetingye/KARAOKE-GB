/// Clock Drift and Effective Sample Rate Estimator (§6.1, §6.4)
pub struct ClockDriftEstimator {
    nominal_rate: f64,
    qpc_frequency_hz: f64,
    start_qpc_tick: Option<i64>,
    start_frame_pos: Option<u64>,
    last_qpc_tick: i64,
    last_frame_pos: u64,
}

impl ClockDriftEstimator {
    pub fn new(nominal_rate: f64, qpc_frequency_hz: f64) -> Self {
        Self {
            nominal_rate,
            qpc_frequency_hz,
            start_qpc_tick: None,
            start_frame_pos: None,
            last_qpc_tick: 0,
            last_frame_pos: 0,
        }
    }

    /// Records a new buffer anchor: device frame position and raw QPC tick (or 100ns converted to ticks)
    pub fn update(&mut self, frame_pos: u64, qpc_tick: i64, _frames_in_buffer: usize) {
        if self.start_qpc_tick.is_none() {
            self.start_qpc_tick = Some(qpc_tick);
            self.start_frame_pos = Some(frame_pos);
        }
        self.last_qpc_tick = qpc_tick;
        self.last_frame_pos = frame_pos;
    }

    /// Computes effective sample rate and drift in parts-per-million (ppm)
    pub fn compute_drift(&self) -> Option<DriftReport> {
        let start_tick = self.start_qpc_tick?;
        let elapsed_ticks = self.last_qpc_tick.saturating_sub(start_tick);
        let start_frame = self.start_frame_pos?;
        let elapsed_frames = self.last_frame_pos.saturating_sub(start_frame);
        if elapsed_ticks <= 0 || elapsed_frames == 0 {
            return None;
        }

        let elapsed_seconds = elapsed_ticks as f64 / self.qpc_frequency_hz;
        if elapsed_seconds < 0.1 {
            return None; // Wait for at least 100ms
        }

        let effective_rate = elapsed_frames as f64 / elapsed_seconds;
        let drift_ppm = ((effective_rate - self.nominal_rate) / self.nominal_rate) * 1_000_000.0;

        Some(DriftReport {
            elapsed_seconds,
            total_frames: elapsed_frames,
            effective_sample_rate: effective_rate,
            nominal_sample_rate: self.nominal_rate,
            drift_ppm,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DriftReport {
    pub elapsed_seconds: f64,
    pub total_frames: u64,
    pub effective_sample_rate: f64,
    pub nominal_sample_rate: f64,
    pub drift_ppm: f64, // e.g. +50 ppm or -120 ppm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drift_calculation() {
        let mut est = ClockDriftEstimator::new(48000.0, 10_000_000.0);
        // Simulate exactly 1.0 second with 48024 frames (+500 ppm)
        est.update(0, 10_000_000, 0);
        est.update(48024, 20_000_000, 48024);

        let report = est.compute_drift().unwrap();
        assert!((report.elapsed_seconds - 1.0).abs() < 1e-4);
        assert!((report.effective_sample_rate - 48024.0).abs() < 0.1);
        assert!((report.drift_ppm - 500.0).abs() < 1.0);
    }

    #[test]
    fn first_packet_size_does_not_bias_rate() {
        let mut est = ClockDriftEstimator::new(48_000.0, 10_000_000.0);
        est.update(960, 10_000_000, 960);
        est.update(48_960, 20_000_000, 960);
        let report = est.compute_drift().unwrap();
        assert!((report.effective_sample_rate - 48_000.0).abs() < 0.001);
        assert!(report.drift_ppm.abs() < 0.001);
    }

    #[test]
    fn t01_simulates_plus_and_minus_300ppm_for_30_minutes() {
        const DURATION_SECONDS: f64 = 30.0 * 60.0;
        for drift_ppm in [-300.0f64, 300.0] {
            let effective_rate = 48_000.0 * (1.0 + drift_ppm / 1_000_000.0);
            let frames = (effective_rate * DURATION_SECONDS).round() as u64;
            let ticks = (10_000_000.0 * DURATION_SECONDS) as i64;
            let mut est = ClockDriftEstimator::new(48_000.0, 10_000_000.0);
            est.update(0, 0, 0);
            est.update(frames, ticks, frames as usize);
            let report = est.compute_drift().unwrap();
            assert!((report.elapsed_seconds - DURATION_SECONDS).abs() < 0.001);
            assert!((report.drift_ppm - drift_ppm).abs() < 0.01);
        }
    }
}
