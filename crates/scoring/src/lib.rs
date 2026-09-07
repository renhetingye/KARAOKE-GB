pub mod engine;
pub mod lut;

pub use engine::{ScoreResult, ScoringConfig, ScoringEngine};
pub use lut::GaussianLut;

#[cfg(test)]
mod tests {
    use super::*;
    use karaoke_protocol::{PitchAnalysisFrame, QualityFlags, SessionStatus, Voicing};
    use karaoke_song_format::Note;

    fn make_test_note(id: &str, start_us: u64, end_us: u64, pitch_midi: u8) -> Note {
        Note {
            id: id.to_string(),
            track_id: "lead".to_string(),
            start_us,
            end_us,
            pitch_midi,
            tuning_cents: 0.0,
            scorable: true,
        }
    }

    fn make_timed_frame(
        song_time_us: i64,
        sequence: u64,
        f0_hz: Option<f32>,
        quality_flags: QualityFlags,
    ) -> (i64, PitchAnalysisFrame) {
        (
            song_time_us,
            PitchAnalysisFrame {
                session_id: "test".to_string(),
                epoch_id: 1,
                capture_stream_id: 1,
                sequence,
                window_start_frame: sequence * 256,
                window_end_frame_exclusive: sequence * 256 + 2048,
                analysis_rate: 48_000,
                reference_qpc: 90_000_000 + song_time_us * 10,
                generated_qpc: 90_000_000 + song_time_us * 10,
                f0_hz,
                periodicity: if f0_hz.is_some() { 0.99 } else { 0.0 },
                voicing: if f0_hz.is_some() {
                    Voicing::Voiced
                } else {
                    Voicing::Silence
                },
                quality_flags,
                algorithm_version: "yin-test".to_string(),
            },
        )
    }

    #[test]
    fn test_t06_silence_gives_zero_score() {
        let engine = ScoringEngine::new(ScoringConfig::default());
        let notes = vec![make_test_note("n1", 100_000, 600_000, 69)]; // 500ms A4
        let frames = vec![]; // No vocal input

        let result = engine.evaluate(&notes, &frames, 1.0);
        assert_eq!(result.status, SessionStatus::Valid);
        assert_eq!(result.total_score, 0.0);
        assert_eq!(result.pitch_accuracy_score, 0.0);
        assert_eq!(result.voicing_coverage_score, 0.0);
    }

    #[test]
    fn test_t06_perfect_match_gives_100_score() {
        let engine = ScoringEngine::new(ScoringConfig::default());
        let notes = vec![make_test_note("n1", 0, 500_000, 69)]; // 500ms A4 (440Hz)

        // Generate pitch frames perfectly matching 440Hz every 5000us
        let mut frames = Vec::new();
        let mut t = 0;
        let mut seq = 0;
        while t <= 500_000 {
            frames.push(PitchAnalysisFrame {
                session_id: "test".to_string(),
                epoch_id: 1,
                capture_stream_id: 1,
                sequence: seq,
                window_start_frame: 0,
                window_end_frame_exclusive: 2048,
                analysis_rate: 48000,
                reference_qpc: t,
                generated_qpc: t,
                f0_hz: Some(440.0),
                periodicity: 0.99,
                voicing: Voicing::Voiced,
                quality_flags: QualityFlags::default(),
                algorithm_version: "1.0.0".to_string(),
            });
            t += 5000;
            seq += 1;
        }

        let result = engine.evaluate(&notes, &frames, 1.0);
        assert_eq!(result.status, SessionStatus::Valid);
        assert_eq!(result.total_score, 100.0);
        assert_eq!(result.pitch_accuracy_score, 100.0);
        assert_eq!(result.voicing_coverage_score, 100.0);
    }

    #[test]
    fn singer_key_moves_the_scoring_target() {
        let engine = ScoringEngine::new(ScoringConfig {
            key_semitones: -3,
            octave_tolerance: false,
            ..ScoringConfig::default()
        });
        assert!(engine.calculate_cents_error(66.0, 69.0) < 0.001);
        assert!((engine.calculate_cents_error(69.0, 69.0) - 300.0).abs() < 0.001);

        for (key, measured) in [(-6, 63.0), (6, 75.0)] {
            let boundary_engine = ScoringEngine::new(ScoringConfig {
                key_semitones: key,
                octave_tolerance: false,
                ..ScoringConfig::default()
            });
            assert!(boundary_engine.calculate_cents_error(measured, 69.0) < 0.001);
            assert!(
                (boundary_engine.calculate_cents_error(69.0, 69.0) - 600.0).abs() < 0.001
            );
        }
    }

    #[test]
    fn normal_scoring_accepts_same_pitch_class_up_to_two_octaves() {
        let normal = ScoringEngine::new(ScoringConfig {
            octave_tolerance: true,
            ..ScoringConfig::default()
        });
        let strict = ScoringEngine::new(ScoringConfig {
            octave_tolerance: false,
            ..ScoringConfig::default()
        });

        for measured in [45.0, 57.0, 69.0, 81.0, 93.0] {
            assert!(normal.calculate_cents_error(measured, 69.0) < 0.001);
        }
        assert!((strict.calculate_cents_error(57.0, 69.0) - 1_200.0).abs() < 0.001);
        assert!((strict.calculate_cents_error(45.0, 69.0) - 2_400.0).abs() < 0.001);
    }

    #[test]
    fn voiced_frames_outside_chart_notes_do_not_affect_any_score() {
        let engine = ScoringEngine::new(ScoringConfig::default());
        let notes = vec![make_test_note("n1", 1_000_000, 1_100_000, 69)];
        let mut frames = Vec::new();
        let mut sequence = 0;

        // Talking/singing before the first bar is intentionally the wrong note.
        for time_us in (0..1_000_000).step_by(5_000) {
            frames.push(make_timed_frame(
                time_us,
                sequence,
                Some(220.0),
                QualityFlags::default(),
            ));
            sequence += 1;
        }
        for time_us in (1_000_000..=1_100_000).step_by(5_000) {
            frames.push(make_timed_frame(
                time_us,
                sequence,
                Some(440.0),
                QualityFlags::default(),
            ));
            sequence += 1;
        }

        let result = engine.evaluate_timed_until(&notes, &frames, 1.0, None);
        assert_eq!(result.total_score, 100.0);
        assert_eq!(result.pitch_accuracy_score, 100.0);
        assert_eq!(result.voicing_coverage_score, 100.0);
        assert_eq!(result.total_evaluated_duration_us, 100_000);
    }

    #[test]
    fn test_short_notes_50ms_retain_positive_denominator() {
        let engine = ScoringEngine::new(ScoringConfig::default());
        let notes = vec![
            make_test_note("n1", 0, 50_000, 69),        // 50ms
            make_test_note("n2", 100_000, 200_000, 69), // 100ms
        ];

        let result = engine.evaluate(&notes, &[], 1.0);
        assert!(result.total_evaluated_duration_us > 0);
        assert_eq!(result.total_evaluated_duration_us, 150_000);
    }

    #[test]
    fn realtime_song_time_is_not_confused_with_absolute_qpc() {
        let engine = ScoringEngine::new(ScoringConfig::default());
        let notes = vec![make_test_note("n1", 100_000, 200_000, 69)];
        let frame = PitchAnalysisFrame {
            session_id: "test".to_string(),
            epoch_id: 1,
            capture_stream_id: 1,
            sequence: 1,
            window_start_frame: 0,
            window_end_frame_exclusive: 2048,
            analysis_rate: 48_000,
            reference_qpc: 9_000_000_000_000,
            generated_qpc: 9_000_000_000_000,
            f0_hz: Some(440.0),
            periodicity: 0.99,
            voicing: Voicing::Voiced,
            quality_flags: QualityFlags::default(),
            algorithm_version: "yin-test".to_string(),
        };
        let timed = vec![(150_000, frame)];
        let result = engine.evaluate_timed_until(&notes, &timed, 1.0, Some(200_000));
        assert!(result.total_score > 0.0);
    }

    #[test]
    fn interim_score_excludes_future_notes_from_denominator() {
        let engine = ScoringEngine::new(ScoringConfig::default());
        let notes = vec![
            make_test_note("now", 0, 100_000, 69),
            make_test_note("future", 1_000_000, 1_100_000, 69),
        ];
        let frames: Vec<_> = (0..=20)
            .map(|sequence| {
                let song_time = sequence * 5_000;
                (
                    song_time,
                    PitchAnalysisFrame {
                        session_id: "test".to_string(),
                        epoch_id: 1,
                        capture_stream_id: 1,
                        sequence: sequence as u64,
                        window_start_frame: 0,
                        window_end_frame_exclusive: 2048,
                        analysis_rate: 48_000,
                        reference_qpc: 99_000_000_000,
                        generated_qpc: 99_000_000_000,
                        f0_hz: Some(440.0),
                        periodicity: 0.99,
                        voicing: Voicing::Voiced,
                        quality_flags: QualityFlags::default(),
                        algorithm_version: "yin-test".to_string(),
                    },
                )
            })
            .collect();
        let result = engine.evaluate_timed_until(&notes, &frames, 1.0, Some(100_000));
        assert_eq!(result.total_score, 100.0);
        assert_eq!(result.total_evaluated_duration_us, 100_000);
    }

    #[test]
    fn t07_score_is_independent_of_ui_polling_rate() {
        let engine = ScoringEngine::new(ScoringConfig::default());
        let notes = vec![make_test_note("n1", 0, 500_000, 69)];
        let frames = (0..=100)
            .map(|sequence| {
                make_timed_frame(
                    sequence * 5_000,
                    sequence as u64,
                    Some(440.0),
                    QualityFlags::default(),
                )
            })
            .collect::<Vec<_>>();
        let expected = engine.evaluate_timed_until(&notes, &frames, 1.0, None);
        for display_fps in [30, 60, 120] {
            let result = engine.evaluate_timed_until(&notes, &frames, 1.0, None);
            assert_eq!(
                result.total_score, expected.total_score,
                "fps={display_fps}"
            );
            assert_eq!(
                result.pitch_accuracy_score, expected.pitch_accuracy_score,
                "fps={display_fps}"
            );
        }
    }

    #[test]
    fn t08_system_gap_cannot_be_certified_valid() {
        let engine = ScoringEngine::new(ScoringConfig::default());
        let notes = vec![make_test_note("n1", 0, 500_000, 69)];
        let flags = QualityFlags {
            input_gap: true,
            ..QualityFlags::default()
        };
        let frames = (0..=100)
            .map(|sequence| make_timed_frame(sequence * 5_000, sequence as u64, None, flags))
            .collect::<Vec<_>>();
        let result = engine.evaluate_timed_until(&notes, &frames, 1.0, None);
        assert_eq!(result.status, SessionStatus::Interrupted);
        assert!(result.raw_system_gap_us > 20_000);
    }

    #[test]
    fn t08_sustained_clipping_is_invalid_input() {
        let engine = ScoringEngine::new(ScoringConfig::default());
        let notes = vec![make_test_note("n1", 0, 500_000, 69)];
        let flags = QualityFlags {
            clipping: true,
            ..QualityFlags::default()
        };
        let frames = (0..=100)
            .map(|sequence| make_timed_frame(sequence * 5_000, sequence as u64, Some(440.0), flags))
            .collect::<Vec<_>>();
        let result = engine.evaluate_timed_until(&notes, &frames, 1.0, None);
        assert_eq!(result.status, SessionStatus::InvalidInput);
    }
}
