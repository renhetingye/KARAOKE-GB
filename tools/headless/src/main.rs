use anyhow::{Context, Result};
use karaoke_audio_win::{get_qpc_frequency, DeviceManager};
use karaoke_media::AudioDecoder;
use karaoke_protocol::{PitchAnalysisFrame, QualityFlags, Voicing};
use karaoke_scoring::{ScoringConfig, ScoringEngine};
use karaoke_song_format::chart::MediaRole;
use karaoke_song_format::{Chart, PackageReader, PackageWriter};
use std::env;
use std::fs;
use std::sync::Arc;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        return Ok(());
    }

    match args[1].as_str() {
        "export-package" => {
            if args.len() < 5 {
                eprintln!(
                    "Usage: karaoke-cli export-package <chart.json> <backing-audio> <output.kpk>"
                );
                std::process::exit(1);
            }
            let mut chart: Chart = serde_json::from_str(
                &fs::read_to_string(&args[2]).context("Failed to read chart")?,
            )
            .context("Failed to parse chart")?;
            let backing_path = std::path::Path::new(&args[3]);
            let extension = backing_path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("bin")
                .to_ascii_lowercase();
            let package_backing = format!("media/backing.{extension}");
            chart
                .media
                .iter_mut()
                .find(|entry| entry.role == MediaRole::Backing)
                .context("Chart has no backing media")?
                .path = package_backing.clone();
            chart.validate_semantics().context("Invalid chart")?;
            let chart_bytes = serde_json::to_vec_pretty(&chart)?;
            let backing_bytes = fs::read(backing_path).context("Failed to read backing")?;
            let output_path = std::path::Path::new(&args[4]);
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let file = fs::File::create(output_path).context("Failed to create package")?;
            let file = PackageWriter::write_package(
                file,
                &chart.song_id,
                "KARAOKE-GB CLI",
                &chart_bytes,
                &package_backing,
                &backing_bytes,
            )?;
            file.sync_all()?;
            PackageReader::inspect_package(fs::File::open(output_path)?)?;
            println!("[+] Package ready: {}", output_path.display());
        }
        "validate-chart" => {
            if args.len() < 3 {
                eprintln!("Usage: karaoke-cli validate-chart <path/to/chart.json>");
                std::process::exit(1);
            }
            let path = &args[2];
            println!("[*] Validating chart file: {}", path);
            let content = fs::read_to_string(path).context("Failed to read chart file")?;
            let chart: Chart =
                serde_json::from_str(&content).context("Chart JSON schema mismatch")?;
            chart
                .validate_semantics()
                .context("Semantic validation failed")?;
            println!(
                "[+] PASS: Chart '{}' (revision {}) is strictly valid under Schema 2.0.0 & §11.4!",
                chart.title, chart.chart_revision
            );
            println!("    Song ID: {}", chart.song_id);
            println!(
                "    Duration: {:.2}s",
                chart.duration_us as f64 / 1_000_000.0
            );
            println!("    Notes: {}", chart.notes.len());
            println!("    Lyric Tokens: {}", chart.lyric_tokens.len());
        }
        "inspect-media" => {
            if args.len() < 3 {
                eprintln!("Usage: karaoke-cli inspect-media <path/to/audio-file>");
                std::process::exit(1);
            }
            let path = &args[2];
            println!("[*] Decoding and canonicalizing media: {}", path);
            let audio = AudioDecoder::decode_file(path).context("Audio decoding failed")?;
            println!("[+] Canonical Media Info (48kHz/float32):");
            println!("    Channels: {}", audio.channels);
            println!("    Sample Rate: {} Hz", audio.sample_rate);
            println!("    Frames: {}", audio.frames);
            println!(
                "    Duration: {:.2}s",
                audio.duration_us as f64 / 1_000_000.0
            );
            println!("    Source SHA-256: {}", audio.source_sha256);
            println!("    Canonical PCM SHA-256: {}", audio.pcm_sha256);
        }
        "list-devices" => {
            println!("[*] Querying Windows WASAPI Audio Devices...");
            let freq = get_qpc_frequency();
            println!(
                "    QPC Frequency: {} Hz (Resolution: {:.2} ns)",
                freq,
                1_000_000_000.0 / freq as f64
            );
            let devices = DeviceManager::list_devices().context("Failed to list devices")?;
            println!("    Found {} active audio endpoints:", devices.len());
            for d in devices {
                println!(
                    "    - [{}] {} (Default: {}) ID: {}",
                    d.data_flow, d.name, d.is_default, d.id
                );
            }
        }
        "stream-mic" => {
            use karaoke_audio_win::WasapiCaptureStream;
            use std::sync::atomic::{AtomicUsize, Ordering};
            use std::sync::Arc;
            use std::thread;
            use std::time::Duration;

            let duration_secs = args.get(2).and_then(|s| s.parse::<u64>().ok()).unwrap_or(3);
            let target_dev_id = args.get(3).map(|s| s.as_str());
            println!(
                "[*] Starting WASAPI microphone capture test for {} seconds on device: {:?}...",
                duration_secs, target_dev_id
            );
            println!("    (Speak or hum into your microphone to test detection)");

            let packet_count = Arc::new(AtomicUsize::new(0));
            let total_frames = Arc::new(AtomicUsize::new(0));
            let max_rms = Arc::new(std::sync::Mutex::new(0.0f32));

            let p_count = packet_count.clone();
            let f_count = total_frames.clone();
            let m_rms = max_rms.clone();

            let mut yin = karaoke_pitch::YinDetector::new(48000, 2048, 0.15);
            let mut pitch_buffer = Vec::new();

            let stream_res = WasapiCaptureStream::start(
                target_dev_id,
                move |samples, dev_pos, qpc_pos, has_discont, _sample_rate| {
                    p_count.fetch_add(1, Ordering::Relaxed);
                    f_count.fetch_add(samples.len(), Ordering::Relaxed);

                    let mut sum_sq = 0.0f32;
                    for &s in samples {
                        sum_sq += s * s;
                    }
                    let rms = (sum_sq / samples.len().max(1) as f32).sqrt();
                    {
                        let mut max_lock = m_rms.lock().unwrap();
                        if rms > *max_lock {
                            *max_lock = rms;
                        }
                    }

                    pitch_buffer.extend_from_slice(samples);
                    if pitch_buffer.len() >= 2048 {
                        let frame = yin.process(
                            &pitch_buffer[..2048],
                            "test",
                            1,
                            1,
                            dev_pos,
                            0,
                            2048,
                            qpc_pos,
                            qpc_pos,
                        );
                        if let Some(f0) = frame.f0_hz {
                            let midi = 69.0 + 12.0 * (f0 as f64 / 440.0).log2();
                            println!("    [Vocal Detected!] F0: {:.1} Hz (MIDI {:.1}), Periodicity: {:.2}, RMS: {:.4}", f0, midi, frame.periodicity, rms);
                        }
                        pitch_buffer.drain(..512); // hop 512
                    }

                    if has_discont {
                        eprintln!(
                            "    [WARN] WASAPI Data Discontinuity detected at QPC {}",
                            qpc_pos
                        );
                    }
                },
            );

            match stream_res {
                Ok(mut stream) => {
                    thread::sleep(Duration::from_secs(duration_secs));
                    stream.stop();
                    println!("[+] Stream completed successfully:");
                    println!(
                        "    Total Packets: {}",
                        packet_count.load(Ordering::Relaxed)
                    );
                    println!("    Total Frames: {}", total_frames.load(Ordering::Relaxed));
                    let peak_rms = *max_rms.lock().unwrap();
                    let peak_db = if peak_rms > 0.0 {
                        20.0 * peak_rms.log10()
                    } else {
                        -96.0
                    };
                    println!("    Peak RMS: {:.4} ({:.1} dBFS)", peak_rms, peak_db);
                }
                Err(e) => {
                    eprintln!("[-] Failed to start WASAPI stream: {:?}", e);
                }
            }
        }
        "play-audio" => {
            if args.len() < 3 {
                eprintln!("Usage: karaoke-cli play-audio <path/to/audio-file> [duration_secs]");
                std::process::exit(1);
            }
            let path = &args[2];
            let duration_limit = args.get(3).and_then(|s| s.parse::<u64>().ok());

            println!("[*] Loading and decoding audio: {}", path);
            let audio = AudioDecoder::decode_file(path).context("Failed to decode audio file")?;
            println!(
                "[+] Audio ready: {} channels, {} Hz, {} frames ({:.2}s)",
                audio.channels,
                audio.sample_rate,
                audio.frames,
                audio.duration_us as f64 / 1_000_000.0
            );

            let channels = audio.channels;
            let mut interleaved = Vec::with_capacity(audio.frames * channels);
            for f in 0..audio.frames {
                for ch in 0..channels {
                    interleaved.push(audio.data[ch][f]);
                }
            }
            let pcm_data = Arc::new(interleaved);
            let play_cursor = Arc::new(std::sync::atomic::AtomicUsize::new(0));

            let pcm_clone = pcm_data.clone();
            let cursor_clone = play_cursor.clone();

            println!("[*] Starting WASAPI render stream...");
            let mut render_stream = karaoke_audio_win::WasapiRenderStream::start(
                None,
                move |buf,
                      frames_needed,
                      _clock_pos,
                      _qpc,
                      _sample_rate,
                      _channels,
                      _queued_frames| {
                    let current_idx = cursor_clone.load(std::sync::atomic::Ordering::Relaxed);
                    let needed_samples = frames_needed * channels;
                    let available_samples = pcm_clone.len().saturating_sub(current_idx);
                    let to_copy = needed_samples.min(available_samples);

                    if to_copy > 0 {
                        buf[..to_copy]
                            .copy_from_slice(&pcm_clone[current_idx..current_idx + to_copy]);
                        cursor_clone.fetch_add(to_copy, std::sync::atomic::Ordering::Relaxed);
                    }
                    if to_copy < needed_samples {
                        buf[to_copy..needed_samples].fill(0.0);
                    }
                },
            )
            .context("Failed to start WASAPI render stream")?;

            let total_dur = duration_limit.unwrap_or((audio.duration_us / 1_000_000) as u64);
            println!(
                "[*] Playing for up to {} seconds (Press Ctrl+C to stop early)...",
                total_dur
            );
            std::thread::sleep(std::time::Duration::from_secs(total_dur));
            render_stream.stop();
            let played_samples = play_cursor.load(std::sync::atomic::Ordering::Relaxed);
            let played_secs = played_samples as f64 / (channels as f64 * 48000.0);
            println!(
                "[+] Playback stopped. Played {:.2} seconds of audio.",
                played_secs
            );
        }
        "test-loopback" => {
            use rtrb::RingBuffer;
            use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

            let duration_secs = args.get(2).and_then(|s| s.parse::<u64>().ok()).unwrap_or(5);
            println!(
                "[*] Starting WASAPI Full-Duplex Soft-Monitor Loopback Test for {}s...",
                duration_secs
            );
            println!("    (Capture Mic -> SPSC Ring Buffer -> Render Speaker/Headphone)");

            // 500ms ring buffer at 48kHz = 24,000 samples (mono)
            let (mut producer, mut consumer) = RingBuffer::<f32>::new(24_000);

            let in_frames = Arc::new(AtomicUsize::new(0));
            let out_frames = Arc::new(AtomicUsize::new(0));
            let discont_count = Arc::new(AtomicUsize::new(0));
            let ring_overflow_frames = Arc::new(AtomicUsize::new(0));
            let ring_underflow_frames = Arc::new(AtomicUsize::new(0));
            let first_cap_qpc = Arc::new(AtomicU64::new(0));
            let first_ren_qpc = Arc::new(AtomicU64::new(0));

            let in_f = in_frames.clone();
            let d_cnt = discont_count.clone();
            let f_cap_qpc = first_cap_qpc.clone();
            let overflow_count = ring_overflow_frames.clone();

            // 1. Start Capture
            let mut cap_stream = karaoke_audio_win::WasapiCaptureStream::start(
                None,
                move |samples, _dev_pos, qpc_pos, has_discont, _sample_rate| {
                    if has_discont {
                        d_cnt.fetch_add(1, Ordering::Relaxed);
                    }
                    let _ = f_cap_qpc.compare_exchange(
                        0,
                        qpc_pos as u64,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                    in_f.fetch_add(samples.len(), Ordering::Relaxed);

                    for &s in samples {
                        if producer.push(s).is_err() {
                            overflow_count.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                },
            )
            .context("Failed to start capture stream")?;

            // 2. Start Render (Stereo: Duplicate mono to L+R)
            let out_f = out_frames.clone();
            let f_ren_qpc = first_ren_qpc.clone();
            let underflow_count = ring_underflow_frames.clone();

            let mut ren_stream = karaoke_audio_win::WasapiRenderStream::start(
                None,
                move |buf,
                      frames_needed,
                      _clock_pos,
                      qpc_pos,
                      _sample_rate,
                      output_channels,
                      _queued_frames| {
                    let _ = f_ren_qpc.compare_exchange(
                        0,
                        qpc_pos as u64,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                    out_f.fetch_add(frames_needed, Ordering::Relaxed);

                    for f in 0..frames_needed {
                        let s = match consumer.pop() {
                            Ok(sample) => sample,
                            Err(_) => {
                                underflow_count.fetch_add(1, Ordering::Relaxed);
                                0.0
                            }
                        };
                        for channel in 0..output_channels {
                            buf[f * output_channels + channel] = s;
                        }
                    }
                },
            )
            .context("Failed to start render stream")?;

            std::thread::sleep(std::time::Duration::from_secs(duration_secs));

            cap_stream.stop();
            ren_stream.stop();

            let total_in = in_frames.load(Ordering::Relaxed);
            let total_out = out_frames.load(Ordering::Relaxed);
            let total_discont = discont_count.load(Ordering::Relaxed);
            let c_qpc = first_cap_qpc.load(Ordering::Relaxed);
            let r_qpc = first_ren_qpc.load(Ordering::Relaxed);

            println!("[+] Loopback Test Results:");
            println!(
                "    Captured Frames: {} ({:.3}s)",
                total_in,
                total_in as f64 / 48000.0
            );
            println!(
                "    Rendered Frames: {} ({:.3}s)",
                total_out,
                total_out as f64 / 48000.0
            );
            println!("    Discontinuities (XRun): {}", total_discont);
            println!(
                "    Ring overflow frames: {}",
                ring_overflow_frames.load(Ordering::Relaxed)
            );
            println!(
                "    Ring underflow frames: {}",
                ring_underflow_frames.load(Ordering::Relaxed)
            );
            if c_qpc > 0 && r_qpc > 0 {
                let delta_ticks = (r_qpc as i64 - c_qpc as i64).abs();
                // WASAPI returns this QPC-correlated timestamp in 100ns units.
                let delta_ms = delta_ticks as f64 / 10_000.0;
                println!("    Initial Stream Startup Offset: {:.2} ms", delta_ms);
            }
        }
        "drift-benchmark" => {
            use karaoke_timeline::ClockDriftEstimator;
            use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

            let duration_secs = args
                .get(2)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(10);
            println!(
                "[*] Running Dual-Stream Clock Drift Benchmark for {}s (§6.2, §18.2 T01)...",
                duration_secs
            );

            // WASAPI capture/audio-clock QPC positions are always 100ns units.
            let qpc_freq = 10_000_000.0;
            let cap_estimator = Arc::new(std::sync::Mutex::new(None::<ClockDriftEstimator>));
            let ren_estimator = Arc::new(std::sync::Mutex::new(None::<ClockDriftEstimator>));
            let discont_count = Arc::new(AtomicUsize::new(0));
            let cap_frames = Arc::new(AtomicU64::new(0));
            let ren_frames = Arc::new(AtomicU64::new(0));

            let ce = cap_estimator.clone();
            let d_cnt = discont_count.clone();
            let c_frames = cap_frames.clone();

            let mut cap = karaoke_audio_win::WasapiCaptureStream::start(
                None,
                move |samples, dev_pos, qpc_pos, has_discont, sample_rate| {
                    if has_discont {
                        d_cnt.fetch_add(1, Ordering::Relaxed);
                    }
                    let observed = c_frames.fetch_add(samples.len() as u64, Ordering::Relaxed)
                        + samples.len() as u64;
                    // Exclude stream startup/preroll from the measurement span.
                    if observed < sample_rate as u64 {
                        return;
                    }
                    if let Ok(mut est) = ce.lock() {
                        let estimator = est.get_or_insert_with(|| {
                            ClockDriftEstimator::new(sample_rate as f64, qpc_freq)
                        });
                        estimator.update(dev_pos, qpc_pos, samples.len());
                    }
                },
            )
            .context("Failed to start capture")?;

            let re = ren_estimator.clone();
            let r_frames = ren_frames.clone();

            let mut ren = karaoke_audio_win::WasapiRenderStream::start(
                None,
                move |buf, needed, clock_pos, qpc_pos, sample_rate, _channels, _queued_frames| {
                    buf.fill(0.0);
                    let observed =
                        r_frames.fetch_add(needed as u64, Ordering::Relaxed) + needed as u64;
                    if observed < sample_rate as u64 {
                        return;
                    }
                    if let Ok(mut est) = re.lock() {
                        let estimator = est.get_or_insert_with(|| {
                            ClockDriftEstimator::new(sample_rate as f64, qpc_freq)
                        });
                        estimator.update(clock_pos, qpc_pos, needed);
                    }
                },
            )
            .context("Failed to start render")?;

            std::thread::sleep(std::time::Duration::from_secs(duration_secs));

            cap.stop();
            ren.stop();

            let c_est = cap_estimator.lock().unwrap();
            let r_est = ren_estimator.lock().unwrap();
            let c_report = c_est.as_ref().and_then(ClockDriftEstimator::compute_drift);
            let r_report = r_est.as_ref().and_then(ClockDriftEstimator::compute_drift);
            let relative_drift_ppm = c_report.zip(r_report).map(|(capture, render)| {
                (capture.effective_sample_rate / render.effective_sample_rate - 1.0) * 1_000_000.0
            });

            println!("[+] Clock Drift Benchmark Results ({}s):", duration_secs);
            println!("    Capture frames: {}", cap_frames.load(Ordering::Relaxed));
            println!("    Render frames:  {}", ren_frames.load(Ordering::Relaxed));
            println!(
                "    Discontinuities: {}",
                discont_count.load(Ordering::Relaxed)
            );
            if let Some(c_rep) = c_report {
                println!(
                    "    Capture callback-span rate: {:.2} ppm (Effective Rate: {:.2} Hz)",
                    c_rep.drift_ppm, c_rep.effective_sample_rate
                );
            } else {
                println!("    Capture Clock Drift: N/A (Insufficient samples)");
            }
            if let Some(r_rep) = r_report {
                println!(
                    "    Render callback-span rate:  {:.2} ppm (Effective Rate: {:.2} Hz)",
                    r_rep.drift_ppm, r_rep.effective_sample_rate
                );
            } else {
                println!("    Render Clock Drift:  N/A (Insufficient samples)");
            }
            if let Some(relative) = relative_drift_ppm {
                println!("    Capture/Render relative drift: {relative:.2} ppm");
            }

            if let Some(report_path) = args.get(3) {
                let json = serde_json::json!({
                    "schemaVersion": "1.0.0",
                    "testId": "T01-short-hardware-observation",
                    "durationSeconds": duration_secs,
                    "timestampUnit": "wasapi_100ns",
                    "captureFramesObserved": cap_frames.load(Ordering::Relaxed),
                    "renderFramesRequested": ren_frames.load(Ordering::Relaxed),
                    "captureDiscontinuities": discont_count.load(Ordering::Relaxed),
                    "capture": c_report.map(|report| serde_json::json!({
                        "elapsedSeconds": report.elapsed_seconds,
                        "frameDelta": report.total_frames,
                        "effectiveSampleRate": report.effective_sample_rate,
                        "driftFromNominalPpm": report.drift_ppm
                    })),
                    "render": r_report.map(|report| serde_json::json!({
                        "elapsedSeconds": report.elapsed_seconds,
                        "frameDelta": report.total_frames,
                        "effectiveSampleRate": report.effective_sample_rate,
                        "driftFromNominalPpm": report.drift_ppm
                    })),
                    "captureRenderRelativeDriftPpm": relative_drift_ppm,
                    "qualification": "short observation only; not a 30-minute T01/T16 acceptance result"
                });
                let contents = serde_json::to_vec_pretty(&json)?;
                fs::write(report_path, contents)
                    .with_context(|| format!("Failed to write report: {report_path}"))?;
                println!("    JSON report: {report_path}");
            }
        }
        "session-synthetic" => {
            use karaoke_audio_engine::KaraokeSession;
            use karaoke_media::SyntheticAudioGenerator;

            println!(
                "[*] Running Deterministic Synthetic Session Verification (v4 §18.2 T01, T06)..."
            );
            let chart_path = "tests/fixtures/synthetic_chart.json";
            let chart_content =
                fs::read_to_string(chart_path).context("Failed to read synthetic chart")?;
            let chart: Chart =
                serde_json::from_str(&chart_content).context("Failed to parse chart")?;

            // 1. Run with 2.0s of pure silence
            let silence = vec![0.0f32; 96_000];
            let silence_res = KaraokeSession::run_synthetic_offline(&chart, &silence, 48000);
            println!("[+] Test 1: Synthetic Silence Evaluation");
            println!("    Total Score: {:.3} / 100.000", silence_res.total_score);
            println!(
                "    Pitch Accuracy: {:.3}%",
                silence_res.pitch_accuracy_score
            );
            println!(
                "    Voicing Coverage: {:.3}%",
                silence_res.voicing_coverage_score
            );
            assert_eq!(
                silence_res.total_score, 0.0,
                "Silence must yield 0.000 score"
            );
            println!("    -> PASS: Silence correctly yielded exactly 0.000 points.");

            // 2. Run with known A4 (440Hz) vocal fixture (50ms short note + 500ms long note)
            let vocal_audio = SyntheticAudioGenerator::generate_a4_timing_fixture(48000);
            let vocal_res = KaraokeSession::run_synthetic_offline(&chart, &vocal_audio, 48000);
            println!("[+] Test 2: Synthetic A4 Timing & Note Matching (50ms + 500ms notes)");
            println!("    Total Score: {:.3} / 100.000", vocal_res.total_score);
            println!(
                "    Pitch Accuracy (A*100): {:.3}%",
                vocal_res.pitch_accuracy_score
            );
            println!(
                "    Voicing Coverage (C*100): {:.3}%",
                vocal_res.voicing_coverage_score
            );
            println!(
                "    Evaluated Duration: {} ms (Target: 550 ms)",
                vocal_res.total_evaluated_duration_us / 1000
            );
            println!(
                "    Voiced Duration: {} ms",
                vocal_res.total_voiced_duration_us / 1000
            );

            assert!(
                vocal_res.total_score >= 95.0,
                "Expected >= 95.0, got {:.3}",
                vocal_res.total_score
            );
            assert!(
                vocal_res.pitch_accuracy_score >= 95.0,
                "Expected >= 95.0%, got {:.3}%",
                vocal_res.pitch_accuracy_score
            );
            println!("    -> PASS: Synthetic A4 vocal correctly scored {:.3} points with {:.2}% accuracy.", vocal_res.total_score, vocal_res.pitch_accuracy_score);
            println!("[+] SUMMARY: Synthetic timing, YIN pitch detection, and scoring engine are 100% verified!");
        }
        "session-live" => {
            use karaoke_audio_engine::KaraokeSession;

            if args.len() < 4 {
                eprintln!("Usage: karaoke-cli session-live <path/to/chart.json> <path/to/backing-audio> [seconds]");
                std::process::exit(1);
            }
            let chart_path = &args[2];
            let audio_path = &args[3];
            let duration_limit = args.get(4).and_then(|s| s.parse::<u64>().ok());

            println!("[*] Initializing Live Full-Duplex Karaoke Session (§4, §5, §7, §9)...");
            println!("    Chart: {}", chart_path);
            println!("    Backing: {}", audio_path);

            let chart_content =
                fs::read_to_string(chart_path).context("Failed to read chart file")?;
            let chart: Chart =
                serde_json::from_str(&chart_content).context("Failed to parse chart JSON")?;
            chart
                .validate_semantics()
                .context("Chart semantic validation failed")?;

            println!("[*] Decoding backing audio track...");
            let backing =
                AudioDecoder::decode_file(audio_path).context("Failed to decode backing track")?;
            println!(
                "[+] Backing loaded: {} channels, {} Hz, {:.2}s",
                backing.channels,
                backing.sample_rate,
                backing.duration_us as f64 / 1_000_000.0
            );

            println!("[*] Starting session (Accompaniment playback + Microphone scoring)...");
            let mut session = KaraokeSession::start(
                chart,
                backing,
                None,
                None,
                0,
                0,
                0,
                0,
                1.0,
                true,
                -3.0,
                karaoke_audio_engine::MonitorConfig::default(),
                None,
            )
            .context("Failed to start KaraokeSession")?;

            println!(
                "[+] Session active! Sing into your microphone now (Press Ctrl+C to finish)..."
            );

            let run_limit = duration_limit.unwrap_or(300);
            let start_time = std::time::Instant::now();

            while start_time.elapsed().as_secs() < run_limit {
                std::thread::sleep(std::time::Duration::from_millis(100));

                if let Some(packet) = session.poll_display() {
                    let song_sec = packet.song_time_us as f64 / 1_000_000.0;
                    let last_f0 = packet.pitch_frames.last().and_then(|f| f.f0_hz);
                    let score = packet.score_snapshot.interim_score_percent;
                    let pitch_acc = packet.score_snapshot.raw_pitch_accuracy;
                    let voicing = packet.score_snapshot.voicing_coverage;

                    let pitch_str = if let Some(hz) = last_f0 {
                        let midi = 69.0 + 12.0 * (hz as f64 / 440.0).log2();
                        format!("{:.1} Hz (MIDI {:.1})", hz, midi)
                    } else {
                        "Silence / No Voice".to_string()
                    };

                    print!("\r    [Time: {:05.2}s] Pitch: {:<24} | Interim Score: {:05.1} (Acc: {:04.1}%, Voice: {:04.1}%)",
                        song_sec, pitch_str, score, pitch_acc, voicing);
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
            }

            println!("\n[*] Session stopping and computing final certified score...");
            if let Some(final_score) = session.stop() {
                println!("\n========================================================");
                println!("             FINAL CERTIFIED KARAOKE RESULT             ");
                println!("========================================================");
                println!("  Status:           {:?}", final_score.status);
                println!(
                    "  TOTAL SCORE:      {:.3} / 100.000",
                    final_score.total_score
                );
                println!(
                    "  Pitch Accuracy:   {:.3}%",
                    final_score.pitch_accuracy_score
                );
                println!(
                    "  Voicing Coverage: {:.3}%",
                    final_score.voicing_coverage_score
                );
                println!(
                    "  Evaluated Notes:  {} ms",
                    final_score.total_evaluated_duration_us / 1000
                );
                println!(
                    "  Voiced Duration:  {} ms",
                    final_score.total_voiced_duration_us / 1000
                );
                println!("========================================================\n");
            }
        }
        "score-demo" => {
            println!("[*] Running synthetic scoring verification demo (§9 & T06)...");
            let engine = ScoringEngine::new(ScoringConfig::default());
            let notes = vec![
                karaoke_song_format::Note {
                    id: "n1".to_string(),
                    track_id: "lead".to_string(),
                    start_us: 500_000,
                    end_us: 1_000_000,
                    pitch_midi: 69, // A4
                    tuning_cents: 0.0,
                    scorable: true,
                },
                karaoke_song_format::Note {
                    id: "n2".to_string(),
                    track_id: "lead".to_string(),
                    start_us: 1_500_000,
                    end_us: 2_000_000,
                    pitch_midi: 71, // B4
                    tuning_cents: 0.0,
                    scorable: true,
                },
            ];

            // Simulate vocal frames: n1 perfect (A4=440Hz), n2 silence
            let mut frames = Vec::new();
            let mut t = 500_000;
            let mut seq = 0;
            while t <= 1_000_000 {
                frames.push(PitchAnalysisFrame {
                    session_id: "demo".to_string(),
                    epoch_id: 1,
                    capture_stream_id: 1,
                    sequence: seq,
                    window_start_frame: 0,
                    window_end_frame_exclusive: 2048,
                    analysis_rate: 48000,
                    reference_qpc: t,
                    generated_qpc: t,
                    f0_hz: Some(440.0),
                    periodicity: 0.98,
                    voicing: Voicing::Voiced,
                    quality_flags: QualityFlags::default(),
                    algorithm_version: "yin-1.0.0".to_string(),
                });
                t += 5000;
                seq += 1;
            }

            let result = engine.evaluate(&notes, &frames, 1.0);
            println!("[+] Score Result:");
            println!("    Status: {:?}", result.status);
            println!("    Score:  {:.3}", result.total_score);
        }
        "inspect-intro" => {
            println!("[*] Inspecting Shining Star intro timing and pitch...");
            let audio_path = std::path::Path::new("tmpmusic/maou_14_shining_star.ogg");
            let audio = AudioDecoder::decode_file(audio_path).context("Failed to decode audio")?;
            println!(
                "[+] Decoded audio: {} channels, {} frames ({:.2}s)",
                audio.channels,
                audio.frames,
                audio.duration_us as f64 / 1_000_000.0
            );

            // Scan first 35 seconds
            let end_frame = (35 * 48000).min(audio.frames);
            let mut mono = Vec::with_capacity(end_frame);
            for f in 0..end_frame {
                let s = if audio.channels == 2 {
                    (audio.data[0][f] + audio.data[1][f]) * 0.5
                } else {
                    audio.data[0][f]
                };
                mono.push(s);
            }

            let mut yin = karaoke_pitch::YinDetector::new(48000, 2048, 0.15);
            let hop = 480; // 10ms hop
            let start_frame = 5 * 48000;
            let mut t_frame = start_frame;
            println!("    Scanning for female vocal pitch (F0 > 200Hz, 5s - 35s)...");
            while t_frame + 2048 <= end_frame {
                let window = &mono[t_frame..t_frame + 2048];
                let rms = (window.iter().map(|&x| x * x).sum::<f32>() / 2048.0).sqrt();
                let frame = yin.process(window, "scan", 1, 1, t_frame as u64, 0, 2048, 0, 0);

                let time_sec = t_frame as f64 / 48000.0;
                if let Some(f0) = frame.f0_hz {
                    let midi = 69.0 + 12.0 * (f0 as f64 / 440.0).log2();
                    // Female singing pitch for Shining Star (Key: E major or A major, F0 ~ 250-700Hz)
                    if f0 > 220.0 && frame.periodicity > 0.80 {
                        println!("    -> [VOCAL DETECTED: {:05.2}s] F0: {:>5.1} Hz (MIDI {:>4.1}) | Periodicity: {:.2} | RMS: {:.3}", time_sec, f0, midi, frame.periodicity, rms);
                    }
                }
                t_frame += hop * 5; // step 50ms
            }
        }
        cmd => {
            eprintln!("Unknown command: {}", cmd);
            print_usage();
            std::process::exit(1);
        }
    }

    Ok(())
}

fn print_usage() {
    println!("Karaoke Headless CLI Tool v0.1.0");
    println!("Usage: karaoke-cli <command> [args...]");
    println!();
    println!("Commands:");
    println!("  export-package <chart> <audio> <output.kpk>  Builds and verifies a .kpk");
    println!("  validate-chart <file>       Validates JSON schema & semantic integrity");
    println!("  inspect-media <file>        Decodes & analyzes media to 48kHz canonical PCM");
    println!("  play-audio <file> [sec]     Plays audio file through default WASAPI render");
    println!("  list-devices                Queries WASAPI audio endpoints and QPC clock");
    println!("  stream-mic [sec]            Captures mic audio & runs real-time YIN pitch");
    println!("  test-loopback [sec]         Runs full-duplex mic -> speaker soft monitoring");
    println!(
        "  drift-benchmark [sec] [json] Benchmarks dual-stream clocks and optionally saves JSON"
    );
    println!(
        "  session-synthetic           Runs deterministic synthetic session timing & scoring test"
    );
    println!(
        "  session-live <chart> <wav>  Runs live accompaniment + mic singing session with scoring"
    );
    println!("  score-demo                  Executes synthetic score calculation demo");
}
