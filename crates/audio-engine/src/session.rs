use crate::dsp::{MonitorConfig, RealtimeVocalDsp};
use karaoke_audio_win::{WasapiCaptureStream, WasapiRenderStream};
use karaoke_media::CanonicalAudio;
use karaoke_pitch::YinDetector;
use karaoke_protocol::{DisplayPacket, MicDiagnostic, PitchAnalysisFrame, ScoreSnapshot};
use karaoke_recording::{
    mark_mix_failed, read_mono_wav, write_aligned_mix, RawRecorder, RawSample, RecordingArtifact,
    RecordingConfig,
};
use karaoke_scoring::{ScoreResult, ScoringConfig, ScoringEngine};
use karaoke_song_format::Chart;
use karaoke_timeline::{QpcClock, Timeline};
use rtrb::RingBuffer;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("Audio device error: {0}")]
    Device(String),
    #[error("Session already running")]
    AlreadyRunning,
    #[error("No backing track loaded")]
    NoBacking,
}

pub struct KaraokeSession {
    running: Arc<AtomicBool>,
    capture_stream: Option<WasapiCaptureStream>,
    render_stream: Option<WasapiRenderStream>,
    worker_thread: Option<JoinHandle<ScoreResult>>,
    latest_display: Arc<Mutex<Option<DisplayPacket>>>,
    latest_diagnostic: Arc<Mutex<Option<MicDiagnostic>>>,
    start_qpc: Arc<AtomicU64>,
    song_duration_us: u64,
    raw_recorder: Option<RawRecorder>,
    recording_artifact: Option<RecordingArtifact>,
    backing: Arc<CanonicalAudio>,
    monitor_config: MonitorConfig,
    start_offset_us: u64,
    speed_ratio: f64,
}

impl KaraokeSession {
    /// Starts a real-time full-duplex singing session (§4, §5, §6, §7, §9)
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        chart: Chart,
        backing: CanonicalAudio,
        mic_device_id: Option<&str>,
        render_device_id: Option<&str>,
        calibration_delta_song_us: i64,
        start_offset_us: u64,
        key_semitones: i32,
        vocal_octave_offset: i32,
        speed_ratio: f64,
        octave_tolerance: bool,
        monitor_config: MonitorConfig,
        recording_config: Option<RecordingConfig>,
    ) -> Result<Self, SessionError> {
        let running = Arc::new(AtomicBool::new(true));
        let speed_ratio = speed_ratio.clamp(0.70, 1.30);
        let running_render = running.clone();
        let running_worker = running.clone();

        let song_duration_us = chart.duration_us;
        let start_qpc = Arc::new(AtomicU64::new(0));
        let start_qpc_render = start_qpc.clone();
        let start_qpc_worker = start_qpc.clone();

        // Capture fans out into independent SPSC buffers. Scoring always sees
        // raw mic samples; monitoring consumes a separate copy and can never
        // stall or modify pitch/scoring input.
        let (mut score_producer, mut consumer) = RingBuffer::<(f32, i64, bool, u32)>::new(24_000);
        let (mut monitor_producer, mut monitor_consumer) = RingBuffer::<f32>::new(96_000);
        let (mut recording_producer, raw_recorder, recording_overflow) =
            if let Some(config) = recording_config {
                let (producer, recording_consumer) = RingBuffer::<RawSample>::new(96_000);
                let (recorder, overflow) = RawRecorder::start(config, recording_consumer);
                (Some(producer), Some(recorder), Some(overflow))
            } else {
                (None, None, None)
            };
        let capture_sample_rate = Arc::new(AtomicU32::new(48_000));
        let capture_sample_rate_render = capture_sample_rate.clone();
        let monitor_config = monitor_config.sanitized();
        let session_monitor_config = monitor_config.clone();
        let monitor_enabled = monitor_config.enabled;
        let mut vocal_dsp = RealtimeVocalDsp::new(monitor_config);
        let mut monitor_started = false;
        let mut monitor_current = 0.0f32;
        let mut monitor_next = 0.0f32;
        let mut monitor_phase = 0.0f64;

        // 1. Backing Render Setup
        let backing = Arc::new(backing);
        let source_rate = backing.sample_rate;
        let source_channels = backing.channels;
        let total_source_frames = backing.frames;
        let pcm_ref = backing.clone();
        let mut source_position = ((start_offset_us as f64 / speed_ratio / 1_000_000.0)
            * source_rate as f64)
            .min(total_source_frames as f64);

        let render_stream = WasapiRenderStream::start(
            render_device_id,
            move |buf,
                  frames_needed,
                  _clock_pos,
                  qpc_pos,
                  output_rate,
                  output_channels,
                  queued_frames| {
                if !running_render.load(Ordering::Relaxed) {
                    buf.fill(0.0);
                    return;
                }

                let first_output_qpc = qpc_pos.saturating_add(
                    ((queued_frames as u128 * 10_000_000u128) / output_rate.max(1) as u128) as i64,
                );
                let _ = start_qpc_render.compare_exchange(
                    0,
                    first_output_qpc as u64,
                    Ordering::Release,
                    Ordering::Relaxed,
                );
                let source_step = source_rate as f64 / output_rate.max(1) as f64;
                vocal_dsp.set_sample_rate(output_rate);
                let input_rate = capture_sample_rate_render.load(Ordering::Relaxed).max(1);
                let monitor_target = (input_rate as usize / 100).max(80); // ~10 ms
                if monitor_enabled && !monitor_started && monitor_consumer.slots() >= monitor_target
                {
                    monitor_current = monitor_consumer.pop().unwrap_or(0.0);
                    monitor_next = monitor_consumer.pop().unwrap_or(monitor_current);
                    monitor_phase = 0.0;
                    monitor_started = true;
                }
                for output_frame in 0..frames_needed {
                    let source_frame = source_position.floor() as usize;
                    let fraction = (source_position - source_frame as f64) as f32;
                    for output_channel in 0..output_channels {
                        let output_index = output_frame * output_channels + output_channel;
                        if source_frame < total_source_frames {
                            let source_channel =
                                output_channel.min(source_channels.saturating_sub(1));
                            let current = pcm_ref.data[source_channel][source_frame];
                            let next = pcm_ref.data[source_channel]
                                .get(source_frame + 1)
                                .copied()
                                .unwrap_or(current);
                            buf[output_index] = current + (next - current) * fraction;
                        } else {
                            buf[output_index] = 0.0;
                        }
                    }

                    let monitor_sample = if monitor_enabled && monitor_started {
                        let interpolated = monitor_current
                            + (monitor_next - monitor_current) * monitor_phase as f32;
                        let occupancy_error =
                            monitor_consumer.slots() as f64 / monitor_target as f64 - 1.0;
                        let drift_correction = (occupancy_error * 0.001).clamp(-0.005, 0.005);
                        monitor_phase += input_rate as f64 / output_rate.max(1) as f64
                            * (1.0 + drift_correction);
                        while monitor_phase >= 1.0 {
                            monitor_phase -= 1.0;
                            monitor_current = monitor_next;
                            match monitor_consumer.pop() {
                                Ok(sample) => monitor_next = sample,
                                Err(_) => {
                                    monitor_started = false;
                                    monitor_current = 0.0;
                                    monitor_next = 0.0;
                                    break;
                                }
                            }
                        }
                        vocal_dsp.process_sample(interpolated)
                    } else {
                        0.0
                    };
                    if monitor_enabled {
                        for output_channel in 0..output_channels {
                            let output_index = output_frame * output_channels + output_channel;
                            buf[output_index] =
                                vocal_dsp.limit_master(buf[output_index] + monitor_sample);
                        }
                    }
                    source_position += source_step;
                }
            },
        )
        .map_err(|e| SessionError::Device(format!("Render stream init failed: {:?}", e)))?;

        // 2. Microphone Capture Setup (raw scoring + independent monitor feed)
        let capture_overflow = Arc::new(AtomicBool::new(false));
        let capture_overflow_cb = capture_overflow.clone();
        let total_pcm_frames = Arc::new(AtomicUsize::new(0));
        let total_pcm_frames_cb = total_pcm_frames.clone();
        let total_packets = Arc::new(AtomicUsize::new(0));
        let total_packets_cb = total_packets.clone();
        let capture_sample_rate_cb = capture_sample_rate.clone();

        let capture_stream = WasapiCaptureStream::start(
            mic_device_id,
            move |samples, _dev_pos, qpc_pos, discontinuity, sample_rate| {
                total_pcm_frames_cb.fetch_add(samples.len(), Ordering::Relaxed);
                total_packets_cb.fetch_add(1, Ordering::Relaxed);
                capture_sample_rate_cb.store(sample_rate, Ordering::Relaxed);
                for (index, &sample) in samples.iter().enumerate() {
                    let sample_qpc = qpc_pos.saturating_add(
                        ((index as u128 * 10_000_000u128) / sample_rate.max(1) as u128) as i64,
                    );
                    if score_producer
                        .push((sample, sample_qpc, discontinuity, sample_rate))
                        .is_err()
                    {
                        capture_overflow_cb.store(true, Ordering::Release);
                    }
                    if monitor_enabled {
                        let _ = monitor_producer.push(sample);
                    }
                    if let Some(producer) = recording_producer.as_mut() {
                        if producer
                            .push(RawSample {
                                sample,
                                qpc_100ns: sample_qpc,
                                discontinuity,
                                sample_rate,
                            })
                            .is_err()
                        {
                            if let Some(overflow) = recording_overflow.as_ref() {
                                overflow.store(true, Ordering::Release);
                            }
                        }
                    }
                }
            },
        )
        .map_err(|e| SessionError::Device(format!("Capture stream init failed: {:?}", e)))?;

        let latest_display = Arc::new(Mutex::new(None));
        let display_writer = latest_display.clone();

        let latest_diagnostic = Arc::new(Mutex::new(Some(MicDiagnostic {
            device_name: mic_device_id.unwrap_or("Default Capture").to_string(),
            total_pcm_frames: 0,
            total_packets: 0,
            rms_dbfs: -96.0,
            peak_dbfs: -96.0,
            f0_hz: None,
            midi_note: None,
            periodicity: 0.0,
            state: "WAITING_PCM".to_string(),
        })));
        let diagnostic_writer = latest_diagnostic.clone();
        let device_display_name = mic_device_id.unwrap_or("Default Capture").to_string();

        // 3. Worker Thread: Pitch & Scoring (§5.1, §5.2, §9)
        let worker_thread = thread::spawn(move || {
            let mut analysis_rate = 48_000u32;
            let mut yin = YinDetector::new(analysis_rate, 2048, 0.15);
            let scoring_engine = ScoringEngine::new(ScoringConfig {
                key_semitones: key_semitones + vocal_octave_offset * 12,
                octave_tolerance,
                ..ScoringConfig::default()
            });
            let mut timeline = Timeline::new(calibration_delta_song_us);
            timeline.set_anchor(0, 0, speed_ratio);

            let mut audio_window = Vec::with_capacity(4096);
            let mut qpc_window = Vec::with_capacity(4096);
            let mut gap_window = Vec::with_capacity(4096);
            let mut recorded_frames: Vec<(i64, PitchAnalysisFrame)> = Vec::with_capacity(10000);
            let mut frame_seq = 0u64;
            let mut window_start_frame = 0u64;
            let mut latest_song_time_us = start_offset_us as i64;
            let mut last_scored_song_time_us = i64::MIN;
            let mut latest_score = scoring_engine.evaluate_timed_until(
                &chart.notes,
                &recorded_frames,
                speed_ratio,
                Some(start_offset_us),
            );

            while running_worker.load(Ordering::Relaxed) {
                while let Ok((sample, qpc, gap, sample_rate)) = consumer.pop() {
                    if sample_rate != analysis_rate {
                        analysis_rate = sample_rate;
                        yin = YinDetector::new(analysis_rate, 2048, 0.15);
                        audio_window.clear();
                        qpc_window.clear();
                        gap_window.clear();
                    }
                    audio_window.push(sample);
                    qpc_window.push(qpc);
                    gap_window.push(gap);
                }

                if audio_window.len() >= 2048 {
                    let window_samples = &audio_window[..2048];
                    let center_qpc = qpc_window[1024];

                    let base_qpc = start_qpc_worker.load(Ordering::Acquire) as i64;
                    let song_time_us = if base_qpc > 0 {
                        // WASAPI's qpc positions are explicitly expressed in
                        // 100 ns units, independent of QueryPerformanceFrequency.
                        let rel_us = QpcClock::hundred_ns_to_us(center_qpc - base_qpc);
                        let mapped = timeline
                            .map_capture_to_song_time(rel_us, timeline.current_epoch())
                            .unwrap_or(0);
                        mapped + (start_offset_us as i64)
                    } else {
                        start_offset_us as i64
                    };
                    latest_song_time_us = song_time_us.max(start_offset_us as i64);

                    let frame = yin.process(
                        window_samples,
                        &chart.song_id,
                        timeline.current_epoch(),
                        1,
                        frame_seq,
                        window_start_frame,
                        window_start_frame + 2048,
                        center_qpc,
                        center_qpc,
                    );
                    let mut frame = frame;
                    if gap_window[..2048].iter().any(|gap| *gap)
                        || capture_overflow.swap(false, Ordering::AcqRel)
                    {
                        frame.quality_flags.input_gap = true;
                    }
                    if recorded_frames
                        .last()
                        .map(|(previous_time, _)| song_time_us <= *previous_time)
                        .unwrap_or(false)
                    {
                        frame.quality_flags.timestamp_invalid = true;
                    }
                    recorded_frames.push((song_time_us, frame.clone()));

                    // Score snapshots are intentionally limited to 10 Hz. The
                    // old implementation rescanned every frame for every 5 ms
                    // cell and grew quadratically with session length.
                    if song_time_us.saturating_sub(last_scored_song_time_us) >= 100_000 {
                        latest_score = scoring_engine.evaluate_timed_until(
                            &chart.notes,
                            &recorded_frames,
                            speed_ratio,
                            Some(song_time_us.max(0) as u64),
                        );
                        last_scored_song_time_us = song_time_us;
                    }

                    let snapshot = ScoreSnapshot {
                        session_status: latest_score.status,
                        current_song_time_us: song_time_us,
                        interim_score_percent: latest_score.total_score,
                        raw_pitch_accuracy: latest_score.pitch_accuracy_score,
                        voicing_coverage: latest_score.voicing_coverage_score,
                        scored_duration_us: latest_score.total_evaluated_duration_us as i64,
                        active_note_id: None,
                    };

                    // Calculate input levels & diagnostics
                    let sum_sq: f32 = window_samples.iter().map(|&s| s * s).sum();
                    let peak: f32 = window_samples
                        .iter()
                        .map(|&s| s.abs())
                        .fold(0.0f32, f32::max);
                    let rms = (sum_sq / 2048.0).sqrt();
                    let rms_dbfs = if rms > 1e-9 {
                        20.0 * rms.log10()
                    } else {
                        -96.0
                    };
                    let peak_dbfs = if peak > 1e-9 {
                        20.0 * peak.log10()
                    } else {
                        -96.0
                    };

                    let mic_level = (rms * 6.0).min(1.0);

                    let state_label = if total_pcm_frames.load(Ordering::Relaxed) == 0 {
                        "NO_PCM".to_string()
                    } else if rms < 0.005 {
                        "SILENCE".to_string()
                    } else if frame.periodicity < 0.60 {
                        "UNVOICED".to_string()
                    } else {
                        "VOICED".to_string()
                    };

                    let midi_note = frame
                        .f0_hz
                        .map(|f0| (69.0 + 12.0 * (f0 as f64 / 440.0).log2()) as f32);
                    let diag = MicDiagnostic {
                        device_name: device_display_name.clone(),
                        total_pcm_frames: total_pcm_frames.load(Ordering::Relaxed) as u64,
                        total_packets: total_packets.load(Ordering::Relaxed) as u64,
                        rms_dbfs,
                        peak_dbfs,
                        f0_hz: frame.f0_hz,
                        midi_note,
                        periodicity: frame.periodicity,
                        state: state_label,
                    };

                    {
                        let mut dlock = diagnostic_writer.lock().unwrap();
                        *dlock = Some(diag);
                    }

                    let packet = DisplayPacket {
                        protocol_version: "2.0.0".to_string(),
                        session_id: chart.song_id.clone(),
                        epoch_id: timeline.current_epoch(),
                        sequence: frame_seq,
                        song_time_us,
                        anchor_qpc: base_qpc,
                        mic_level,
                        pitch_frames: vec![frame],
                        score_snapshot: snapshot,
                    };

                    {
                        let mut lock = display_writer.lock().unwrap();
                        *lock = Some(packet);
                    }

                    audio_window.drain(..256); // hop 256
                    qpc_window.drain(..256);
                    gap_window.drain(..256);
                    window_start_frame += 256;
                    frame_seq += 1;
                } else {
                    thread::sleep(std::time::Duration::from_millis(2));
                }
            }

            let mut final_score = scoring_engine.evaluate_timed_until(
                &chart.notes,
                &recorded_frames,
                speed_ratio,
                Some(latest_song_time_us.max(0) as u64),
            );
            if final_score.status == karaoke_protocol::SessionStatus::Valid
                && (start_offset_us > 0 || latest_song_time_us < chart.duration_us as i64 - 10_000)
            {
                final_score.status = karaoke_protocol::SessionStatus::Practice;
            }
            final_score
        });

        Ok(Self {
            running,
            capture_stream: Some(capture_stream),
            render_stream: Some(render_stream),
            worker_thread: Some(worker_thread),
            latest_display,
            latest_diagnostic,
            start_qpc,
            song_duration_us,
            raw_recorder,
            recording_artifact: None,
            backing,
            monitor_config: session_monitor_config,
            start_offset_us,
            speed_ratio,
        })
    }

    /// Returns the session start QPC tick (0 if not started yet)
    pub fn start_qpc(&self) -> u64 {
        self.start_qpc.load(Ordering::Relaxed)
    }

    /// Returns the song duration in microseconds
    pub fn song_duration_us(&self) -> u64 {
        self.song_duration_us
    }

    /// Polls latest DisplayPacket for 60fps UI rendering (§5.3)
    pub fn poll_display(&self) -> Option<DisplayPacket> {
        let lock = self.latest_display.lock().ok()?;
        lock.clone()
    }

    /// Polls latest MicDiagnostic for real-time telemetry
    pub fn poll_diagnostic(&self) -> Option<MicDiagnostic> {
        let lock = self.latest_diagnostic.lock().ok()?;
        lock.clone()
    }

    /// Stops session and returns final certified ScoreResult (§9)
    pub fn stop(&mut self) -> Option<ScoreResult> {
        self.running.store(false, Ordering::Relaxed);
        if let Some(mut cap) = self.capture_stream.take() {
            cap.stop();
        }
        if let Some(mut ren) = self.render_stream.take() {
            ren.stop();
        }
        if let Some(mut recorder) = self.raw_recorder.take() {
            match recorder.stop() {
                Ok(mut artifact) => {
                    if artifact.sample_frames > 0 {
                        if let Err(error) = self.export_mix(&mut artifact) {
                            tracing::error!("Failed to export wet/master recording: {error}");
                            let _ = mark_mix_failed(&mut artifact, error.to_string());
                        }
                    }
                    self.recording_artifact = Some(artifact);
                }
                Err(error) => tracing::error!("Failed to finalize raw recording: {error}"),
            }
        }
        if let Some(handle) = self.worker_thread.take() {
            handle.join().ok()
        } else {
            None
        }
    }

    pub fn take_recording_artifact(&mut self) -> Option<RecordingArtifact> {
        self.recording_artifact.take()
    }

    fn export_mix(
        &self,
        artifact: &mut RecordingArtifact,
    ) -> Result<(), karaoke_recording::RecordingError> {
        let render_start = self.start_qpc.load(Ordering::Acquire) as i64;
        if render_start <= 0 {
            return Err(karaoke_recording::RecordingError::Io(
                std::io::Error::other("render clock never established an audio start timestamp"),
            ));
        }
        let (raw, raw_rate) = read_mono_wav(std::path::Path::new(&artifact.raw_wav_path))?;
        let mut config = self.monitor_config.clone();
        config.enabled = true;
        let limiter_ceiling_db = config.limiter_ceiling_db;
        let mut dsp = RealtimeVocalDsp::new(config);
        dsp.set_sample_rate(raw_rate);
        let processed: Vec<f32> = raw
            .into_iter()
            .map(|sample| dsp.process_sample(sample))
            .collect();
        let wet_48k = resample_linear_mono(&processed, raw_rate, 48_000);
        write_aligned_mix(
            artifact,
            render_start,
            &wet_48k,
            48_000,
            &self.backing.data,
            self.backing.sample_rate,
            (self.start_offset_us as f64 / self.speed_ratio).round() as u64,
            limiter_ceiling_db,
        )
    }

    /// Deterministic offline execution for synthetic fixtures (v4 §18.2 T01, T06)
    pub fn run_synthetic_offline(
        chart: &Chart,
        vocal_samples: &[f32],
        sample_rate: u32,
    ) -> ScoreResult {
        let mut yin = YinDetector::new(sample_rate, 2048, 0.15);
        let scoring_engine = ScoringEngine::new(ScoringConfig::default());

        let hop_size = 256;
        let window_size = 2048;
        let mut recorded_frames = Vec::new();
        let mut frame_seq = 0u64;

        let total_frames = vocal_samples.len();
        let mut start_idx = 0;

        while start_idx + window_size <= total_frames {
            let window = &vocal_samples[start_idx..start_idx + window_size];
            let center_sample = start_idx + window_size / 2;
            let song_time_us = (center_sample as f64 / sample_rate as f64 * 1_000_000.0) as i64;

            let frame = yin.process(
                window,
                &chart.song_id,
                1,
                1,
                frame_seq,
                start_idx as u64,
                (start_idx + window_size) as u64,
                song_time_us,
                song_time_us,
            );
            recorded_frames.push(frame);

            start_idx += hop_size;
            frame_seq += 1;
        }

        scoring_engine.evaluate(&chart.notes, &recorded_frames, 1.0)
    }
}

fn resample_linear_mono(input: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if input.is_empty() || source_rate == 0 || target_rate == 0 {
        return Vec::new();
    }
    if source_rate == target_rate {
        return input.to_vec();
    }
    let output_len = ((input.len() as u128 * target_rate as u128) / source_rate as u128) as usize;
    let step = source_rate as f64 / target_rate as f64;
    (0..output_len)
        .map(|index| {
            let position = index as f64 * step;
            let frame = position.floor() as usize;
            let fraction = (position - frame as f64) as f32;
            let current = input.get(frame).copied().unwrap_or(0.0);
            let next = input.get(frame + 1).copied().unwrap_or(current);
            current + (next - current) * fraction
        })
        .collect()
}

impl Drop for KaraokeSession {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
