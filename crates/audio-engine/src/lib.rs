pub mod dsp;
pub mod monitor;
pub mod pipeline;
pub mod session;

pub use dsp::{MonitorConfig, RealtimeVocalDsp};
pub use karaoke_recording::{RecordingArtifact, RecordingConfig};
pub use monitor::MicMonitorSession;
pub use pipeline::AudioPipeline;
pub use session::{KaraokeSession, SessionError};

#[cfg(test)]
mod tests {
    use super::*;
    use karaoke_media::SyntheticAudioGenerator;
    use karaoke_song_format::Chart;
    use std::fs;
    use std::path::Path;

    #[test]
    fn test_synthetic_session_silence_gives_zero() {
        let chart_path = Path::new("../../tests/fixtures/synthetic_chart.json");
        let chart_content = fs::read_to_string(chart_path).expect("Failed to read synthetic chart");
        let chart: Chart = serde_json::from_str(&chart_content).expect("Failed to parse chart");

        // 2.0s of pure silence (96,000 frames)
        let silence = vec![0.0f32; 96_000];
        let result = KaraokeSession::run_synthetic_offline(&chart, &silence, 48000);

        assert_eq!(
            result.total_score, 0.0,
            "Silence must yield exactly 0 score"
        );
        assert_eq!(result.pitch_accuracy_score, 0.0);
        assert_eq!(result.voicing_coverage_score, 0.0);
        println!("PASS: Synthetic silence yields exactly 0 score");
    }

    #[test]
    fn test_synthetic_session_perfect_vocal_reaches_near_100() {
        let chart_path = Path::new("../../tests/fixtures/synthetic_chart.json");
        let chart_content = fs::read_to_string(chart_path).expect("Failed to read synthetic chart");
        let chart: Chart = serde_json::from_str(&chart_content).expect("Failed to parse chart");

        // A4 440Hz pure sine fixture matching notes n1 (500-550ms) and n2 (1000-1500ms)
        let vocal_audio = SyntheticAudioGenerator::generate_a4_timing_fixture(48000);
        let result = KaraokeSession::run_synthetic_offline(&chart, &vocal_audio, 48000);

        println!("Synthetic Perfect Vocal Result:");
        println!("  Total Score: {:.3}", result.total_score);
        println!("  Pitch Accuracy: {:.3}%", result.pitch_accuracy_score);
        println!("  Voicing Coverage: {:.3}%", result.voicing_coverage_score);
        println!(
            "  Evaluated Duration: {} ms",
            result.total_evaluated_duration_us / 1000
        );
        println!(
            "  Voiced Duration: {} ms",
            result.total_voiced_duration_us / 1000
        );

        // Due to windowing edges (YIN requires ~2048 window), coverage over 50ms short note + 500ms long note:
        assert!(
            result.total_score > 90.0,
            "Perfect synthetic fixture must score > 90.0, got {:.3}",
            result.total_score
        );
        assert!(
            result.pitch_accuracy_score > 95.0,
            "Pitch accuracy must be > 95%, got {:.3}%",
            result.pitch_accuracy_score
        );
    }

    #[test]
    fn test_live_audio_pipeline_diagnostics() {
        println!("[*] Starting AudioPipeline diagnostic test (2 seconds)...");
        let pipeline_res = AudioPipeline::start(None, vec![], 0);
        match pipeline_res {
            Ok(mut pipeline) => {
                let mut packets_received = 0;
                let mut voiced_frames = 0;
                let mut max_mic_level = 0.0f32;

                for _ in 0..20 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if let Some(packet) = pipeline.get_latest_display() {
                        packets_received += 1;
                        if packet.mic_level > max_mic_level {
                            max_mic_level = packet.mic_level;
                        }
                        for pf in &packet.pitch_frames {
                            if pf.f0_hz.is_some() {
                                voiced_frames += 1;
                            }
                        }
                    }
                }
                pipeline.stop();
                println!("[+] AudioPipeline live diagnostic summary:");
                println!("    Packets received: {}", packets_received);
                println!("    Max mic level: {:.4}", max_mic_level);
                println!("    Voiced frames detected: {}", voiced_frames);
                assert!(
                    packets_received > 0,
                    "AudioPipeline must produce DisplayPackets from live mic"
                );
            }
            Err(e) => {
                panic!("Failed to start live AudioPipeline: {:?}", e);
            }
        }
    }
}
