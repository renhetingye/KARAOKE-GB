pub mod yin;

pub use yin::YinDetector;

#[cfg(test)]
mod tests {
    use super::*;
    use karaoke_protocol::Voicing;
    use std::f32::consts::PI;

    #[test]
    fn test_yin_detects_440hz_pure_sine() {
        let sample_rate = 48000;
        let window_size = 2048;
        let mut detector = YinDetector::new(sample_rate, window_size, 0.15);

        // Generate 440 Hz pure sine wave
        let mut samples = Vec::with_capacity(window_size);
        for i in 0..window_size {
            let t = i as f32 / sample_rate as f32;
            samples.push((2.0 * PI * 440.0 * t).sin() * 0.8);
        }

        let frame = detector.process(
            &samples,
            "test_session",
            1,
            1,
            0,
            0,
            window_size as u64,
            0,
            0,
        );

        assert_eq!(frame.voicing, Voicing::Voiced);
        assert!(frame.f0_hz.is_some());
        let f0 = frame.f0_hz.unwrap();
        // Should be within ±1 Hz of 440 Hz
        assert!((f0 - 440.0).abs() < 1.0, "Expected ~440 Hz, got {}", f0);
        assert!(frame.periodicity > 0.95);
        assert!(frame.quality_flags.is_clean());
    }

    #[test]
    fn test_yin_detects_silence() {
        let sample_rate = 48000;
        let window_size = 2048;
        let mut detector = YinDetector::new(sample_rate, window_size, 0.15);

        let samples = vec![0.0f32; window_size];
        let frame = detector.process(
            &samples,
            "test_session",
            1,
            1,
            0,
            0,
            window_size as u64,
            0,
            0,
        );

        assert_eq!(frame.voicing, Voicing::Silence);
        assert!(frame.f0_hz.is_none());
    }
}
