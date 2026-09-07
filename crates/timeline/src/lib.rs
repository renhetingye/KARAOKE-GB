pub mod drift;

pub use drift::{ClockDriftEstimator, DriftReport};
use serde::{Deserialize, Serialize};

/// Helper for Windows QPC time calculations (§6.1)
#[derive(Debug, Clone, Copy)]
pub struct QpcClock {
    pub frequency_hz: i64,
}

impl QpcClock {
    pub fn new(frequency_hz: i64) -> Self {
        Self { frequency_hz }
    }

    /// Converts raw QPC ticks to microseconds relative to a base tick
    pub fn ticks_to_relative_us(&self, raw_ticks: i64, base_ticks: i64) -> i64 {
        let diff = raw_ticks.saturating_sub(base_ticks);
        ((diff as i128 * 1_000_000) / self.frequency_hz as i128) as i64
    }

    /// Converts Windows 100ns units (e.g. from IAudioCaptureClient::GetBuffer) to microseconds
    pub fn hundred_ns_to_us(hundred_ns: i64) -> i64 {
        hundred_ns / 10
    }
}

/// §6.2 Transport Anchor
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TransportAnchor {
    pub epoch_id: u64,
    pub s0_us: i64,       // Song time in microseconds
    pub q0_us: i64,       // QPC reference time in microseconds
    pub speed_ratio: f64, // Typically 1.0 (0.70 ..= 1.30)
}

impl TransportAnchor {
    pub fn new(epoch_id: u64, s0_us: i64, q0_us: i64, speed_ratio: f64) -> Self {
        Self {
            epoch_id,
            s0_us,
            q0_us,
            speed_ratio: if speed_ratio <= 0.0 { 1.0 } else { speed_ratio },
        }
    }

    /// Calculates song time s(q) = s0 + r * (q - q0) (§6.2)
    pub fn song_time_at(&self, q_us: i64) -> i64 {
        let dq = q_us - self.q0_us;
        self.s0_us + (self.speed_ratio * dq as f64).round() as i64
    }

    /// Calculates QPC time q(s) = q0 + (s - s0) / r
    pub fn qpc_at_song_time(&self, s_us: i64) -> i64 {
        let ds = s_us - self.s0_us;
        self.q0_us + (ds as f64 / self.speed_ratio).round() as i64
    }
}

/// Manages song timeline, clock anchors, and calibration offsets (§6.1, §6.2, §6.5)
pub struct Timeline {
    current_epoch: u64,
    anchor: Option<TransportAnchor>,
    calibration_delta_song_us: i64,
}

impl Timeline {
    pub fn new(calibration_delta_song_us: i64) -> Self {
        Self {
            current_epoch: 1,
            anchor: None,
            calibration_delta_song_us,
        }
    }

    pub fn current_epoch(&self) -> u64 {
        self.current_epoch
    }

    pub fn set_anchor(&mut self, s0_us: i64, q0_us: i64, speed_ratio: f64) {
        self.anchor = Some(TransportAnchor::new(
            self.current_epoch,
            s0_us,
            q0_us,
            speed_ratio,
        ));
    }

    /// Advances epoch on Seek / Pause / Resume / Device change (§6.5)
    pub fn advance_epoch(&mut self) -> u64 {
        self.current_epoch += 1;
        self.anchor = None;
        self.current_epoch
    }

    pub fn current_anchor(&self) -> Option<&TransportAnchor> {
        self.anchor.as_ref()
    }

    /// Maps capture QPC time to evaluation song time: s_eval = s(q_capture) + delta_song_us (§6.2)
    pub fn map_capture_to_song_time(&self, capture_qpc_us: i64, epoch_id: u64) -> Option<i64> {
        let anchor = self.anchor.as_ref()?;
        if anchor.epoch_id != epoch_id {
            // Discard old epoch (§6.5)
            return None;
        }
        let base_song_time = anchor.song_time_at(capture_qpc_us);
        Some(base_song_time + self.calibration_delta_song_us)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transport_anchor_mapping() {
        let anchor = TransportAnchor::new(1, 1_000_000, 2_000_000, 1.0);
        // At q = 2_000_000 -> s = 1_000_000
        assert_eq!(anchor.song_time_at(2_000_000), 1_000_000);
        // At q = 3_000_000 (+1s) -> s = 2_000_000 (+1s)
        assert_eq!(anchor.song_time_at(3_000_000), 2_000_000);
        // Inverse
        assert_eq!(anchor.qpc_at_song_time(2_000_000), 3_000_000);
    }

    #[test]
    fn test_speed_scaling() {
        // Speed 1.25x
        let anchor = TransportAnchor::new(1, 0, 0, 1.25);
        assert_eq!(anchor.song_time_at(1_000_000), 1_250_000);
    }

    #[test]
    fn test_timeline_epoch_invalidation() {
        let mut timeline = Timeline::new(0);
        timeline.set_anchor(0, 0, 1.0);

        assert!(timeline.map_capture_to_song_time(500_000, 1).is_some());
        // Invalidate by advancing epoch
        timeline.advance_epoch();
        assert_eq!(timeline.current_epoch(), 2);
        // Old epoch request is rejected
        assert!(timeline.map_capture_to_song_time(500_000, 1).is_none());
    }
}
