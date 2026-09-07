use karaoke_audio_win::WasapiCaptureStream;
use karaoke_pitch::YinDetector;
use karaoke_protocol::{DisplayPacket, MicDiagnostic, ScoreSnapshot, SessionStatus};
use karaoke_song_format::Note;
use rtrb::RingBuffer;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

pub struct AudioPipeline {
    running: Arc<AtomicBool>,
    capture_stream: Option<WasapiCaptureStream>,
    pitch_thread: Option<JoinHandle<()>>,
    latest_display: Arc<Mutex<Option<DisplayPacket>>>,
    latest_diagnostic: Arc<Mutex<MicDiagnostic>>,
}

impl AudioPipeline {
    /// Starts the real-time Capture -> Pitch -> Score pipeline (§5.1, §5.2)
    pub fn start(
        device_id: Option<&str>,
        _notes: Vec<Note>,
        _calibration_delta_song_us: i64,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let running = Arc::new(AtomicBool::new(true));
        let running_pitch = running.clone();

        // RingBuffer for 500ms audio at 48kHz = 24,000 samples (§5.2)
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(24_000);

        let latest_display = Arc::new(Mutex::new(None));
        let latest_display_writer = latest_display.clone();

        let total_pcm_frames = Arc::new(AtomicU64::new(0));
        let total_packets = Arc::new(AtomicU64::new(0));
        let frames_tracker = total_pcm_frames.clone();
        let packets_tracker = total_packets.clone();

        let device_name_str = device_id.unwrap_or("Default Capture").to_string();
        let dev_for_worker = device_name_str.clone();

        let latest_diagnostic = Arc::new(Mutex::new(MicDiagnostic {
            device_name: device_name_str,
            total_pcm_frames: 0,
            total_packets: 0,
            rms_dbfs: -96.0,
            peak_dbfs: -96.0,
            f0_hz: None,
            midi_note: None,
            periodicity: 0.0,
            state: "NO_PCM".to_string(),
        }));
        let diagnostic_writer = latest_diagnostic.clone();

        // 1. Pitch & Scoring Worker Thread (§5.1)
        let pitch_thread = thread::spawn(move || {
            let mut yin = YinDetector::new(48000, 2048, 0.15);
            let mut audio_window = Vec::with_capacity(4096);
            let mut frame_seq = 0u64;

            // Telemetry smoothing & hysteresis state (§18.2)
            let mut smoothed_rms = 0.0f32;
            let mut current_state = "SILENCE".to_string();
            let mut state_stability_frames = 0usize;
            let mut smoothed_midi: Option<f32> = None;

            while running_pitch.load(Ordering::Relaxed) {
                // Read from SPSC ring buffer
                while let Ok(sample) = consumer.pop() {
                    audio_window.push(sample);
                }

                if audio_window.len() >= 2048 {
                    let window = &audio_window[..2048];

                    // Compute RMS and Peak dBFS with EMA smoothing
                    let peak = window.iter().map(|&x| x.abs()).fold(0.0f32, f32::max);
                    let raw_rms = (window.iter().map(|&x| x * x).sum::<f32>() / 2048.0).sqrt();
                    // EMA: alpha=0.18 gives snappy response while eliminating single-frame flicker
                    smoothed_rms = 0.18 * raw_rms + 0.82 * smoothed_rms;

                    let rms_dbfs = if smoothed_rms > 1e-5 {
                        20.0 * smoothed_rms.log10()
                    } else {
                        -96.0
                    };
                    let peak_dbfs = if peak > 1e-5 {
                        20.0 * peak.log10()
                    } else {
                        -96.0
                    };
                    let mic_level = (smoothed_rms * 6.0).min(1.0);

                    // Extract hop 256 frames
                    let frame = yin.process(window, "live_session", 1, 1, frame_seq, 0, 2048, 0, 0);

                    // Determine candidate diagnostic state with hysteresis (§18.2)
                    let total_f = frames_tracker.load(Ordering::Relaxed);
                    let total_p = packets_tracker.load(Ordering::Relaxed);

                    let candidate_state = if total_f == 0 {
                        "NO_PCM"
                    } else if smoothed_rms < 0.0035 {
                        "SILENCE"
                    } else if frame.f0_hz.is_some()
                        && frame.periodicity > 0.55
                        && smoothed_rms >= 0.0040
                    {
                        "VOICED"
                    } else {
                        "UNVOICED"
                    };

                    // Hysteresis / Debounce:
                    // Returning to SILENCE requires 8 consecutive frames (~42ms) to bridge micro-pauses
                    // Entering VOICED requires 3 consecutive frames (~16ms)
                    // Entering UNVOICED requires 3 consecutive frames (~16ms)
                    let required_frames = match candidate_state {
                        "SILENCE" => 8,
                        "VOICED" => 3,
                        "UNVOICED" => 3,
                        _ => 1,
                    };

                    if candidate_state == current_state {
                        state_stability_frames = 0;
                    } else {
                        state_stability_frames += 1;
                        if state_stability_frames >= required_frames {
                            current_state = candidate_state.to_string();
                            state_stability_frames = 0;
                        }
                    }

                    let midi_note = if current_state == "VOICED" {
                        if let Some(f0) = frame.f0_hz {
                            let raw_m = 69.0 + 12.0 * (f0 as f64 / 440.0).log2();
                            let target_m = raw_m as f32;
                            let sm = match smoothed_midi {
                                Some(prev) => 0.35 * target_m + 0.65 * prev,
                                None => target_m,
                            };
                            smoothed_midi = Some(sm);
                            Some(sm)
                        } else {
                            smoothed_midi
                        }
                    } else {
                        smoothed_midi = None;
                        None
                    };

                    {
                        let mut diag_lock = diagnostic_writer.lock().unwrap();
                        *diag_lock = MicDiagnostic {
                            device_name: dev_for_worker.clone(),
                            total_pcm_frames: total_f,
                            total_packets: total_p,
                            rms_dbfs,
                            peak_dbfs,
                            f0_hz: frame.f0_hz,
                            midi_note,
                            periodicity: frame.periodicity,
                            state: current_state.clone(),
                        };
                    }

                    let snapshot = ScoreSnapshot {
                        session_status: SessionStatus::NotScorable,
                        current_song_time_us: 0,
                        interim_score_percent: 0.0,
                        raw_pitch_accuracy: 0.0,
                        voicing_coverage: 0.0,
                        scored_duration_us: 0,
                        active_note_id: None,
                    };

                    let display = DisplayPacket {
                        protocol_version: "2.0.0".to_string(),
                        session_id: "live_session".to_string(),
                        epoch_id: 1,
                        sequence: frame_seq,
                        song_time_us: 0,
                        anchor_qpc: 0,
                        mic_level,
                        pitch_frames: vec![frame],
                        score_snapshot: snapshot,
                    };

                    {
                        let mut lock = latest_display_writer.lock().unwrap();
                        *lock = Some(display);
                    }

                    audio_window.drain(..256); // hop 256
                    frame_seq += 1;
                } else {
                    thread::sleep(std::time::Duration::from_millis(2));
                }
            }
        });

        // 2. Start WASAPI Capture stream feeding SPSC producer
        let p_track = total_packets.clone();
        let f_track = total_pcm_frames.clone();

        let capture_stream =
            WasapiCaptureStream::start(device_id, move |samples, _, _, _, _sample_rate| {
                p_track.fetch_add(1, Ordering::Relaxed);
                f_track.fetch_add(samples.len() as u64, Ordering::Relaxed);
                for &s in samples {
                    let _ = producer.push(s);
                }
            })?;

        Ok(Self {
            running,
            capture_stream: Some(capture_stream),
            pitch_thread: Some(pitch_thread),
            latest_display,
            latest_diagnostic,
        })
    }

    /// Gets the latest DisplayPacket for streaming to UI (§5.3)
    pub fn get_latest_display(&self) -> Option<DisplayPacket> {
        let lock = self.latest_display.lock().unwrap();
        lock.clone()
    }

    /// Gets the latest diagnostic telemetry (§18.2)
    pub fn get_latest_diagnostic(&self) -> MicDiagnostic {
        let lock = self.latest_diagnostic.lock().unwrap();
        lock.clone()
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(mut stream) = self.capture_stream.take() {
            stream.stop();
        }
        if let Some(h) = self.pitch_thread.take() {
            let _ = h.join();
        }
    }
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.stop();
    }
}
