use rtrb::Consumer;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct RecordingConfig {
    pub output_dir: PathBuf,
    pub session_id: String,
    pub song_id: String,
    pub start_offset_us: u64,
    pub backing_gain_db: f32,
    pub vocal_gain_db: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct RawSample {
    pub sample: f32,
    pub qpc_100ns: i64,
    pub discontinuity: bool,
    pub sample_rate: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GapRange {
    pub start_frame: u64,
    pub end_frame: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingArtifact {
    pub schema_version: String,
    pub session_id: String,
    pub song_id: String,
    pub status: String,
    pub raw_wav_path: String,
    pub wet_wav_path: Option<String>,
    pub master_wav_path: Option<String>,
    pub mix_status: String,
    pub mix_error: Option<String>,
    pub metadata_path: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub sample_frames: u64,
    pub first_timestamp_100ns: Option<i64>,
    pub last_timestamp_100ns: Option<i64>,
    pub render_start_timestamp_100ns: Option<i64>,
    pub raw_start_relative_us: Option<i64>,
    pub start_offset_us: u64,
    #[serde(default)]
    pub backing_gain_db: f32,
    #[serde(default)]
    pub vocal_gain_db: f32,
    pub capture_overflow: bool,
    pub gap_ranges: Vec<GapRange>,
}

#[derive(Debug, Error)]
pub enum RecordingError {
    #[error("recording I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("WAV error: {0}")]
    Wav(#[from] hound::Error),
    #[error("recording worker panicked")]
    WorkerPanicked,
}

pub struct RawRecorder {
    running: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<RecordingArtifact, RecordingError>>>,
}

impl RawRecorder {
    pub fn start(
        config: RecordingConfig,
        mut consumer: Consumer<RawSample>,
    ) -> (Self, Arc<AtomicBool>) {
        let running = Arc::new(AtomicBool::new(true));
        let overflow = Arc::new(AtomicBool::new(false));
        let worker_running = running.clone();
        let worker_overflow = overflow.clone();
        let producer_overflow = overflow.clone();

        let worker = thread::spawn(move || {
            record_worker(config, &mut consumer, worker_running, worker_overflow)
        });
        (
            Self {
                running,
                overflow,
                worker: Some(worker),
            },
            producer_overflow,
        )
    }

    pub fn stop(&mut self) -> Result<RecordingArtifact, RecordingError> {
        self.running.store(false, Ordering::Release);
        self.worker
            .take()
            .ok_or(RecordingError::WorkerPanicked)?
            .join()
            .map_err(|_| RecordingError::WorkerPanicked)?
    }

    pub fn mark_overflow(&self) {
        self.overflow.store(true, Ordering::Release);
    }
}

impl Drop for RawRecorder {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.stop();
        }
    }
}

fn record_worker(
    config: RecordingConfig,
    consumer: &mut Consumer<RawSample>,
    running: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
) -> Result<RecordingArtifact, RecordingError> {
    fs::create_dir_all(&config.output_dir)?;
    let partial_path = config.output_dir.join("raw.partial.wav");
    let final_path = config.output_dir.join("raw.wav");
    let metadata_path = config.output_dir.join("recording.json");

    let mut writer: Option<hound::WavWriter<std::io::BufWriter<fs::File>>> = None;
    let mut sample_rate = 0u32;
    let mut sample_frames = 0u64;
    let mut first_timestamp = None;
    let mut last_timestamp = None;
    let mut gaps = Vec::new();
    let mut incompatible_rate = false;

    while running.load(Ordering::Acquire) || consumer.slots() > 0 {
        match consumer.pop() {
            Ok(packet) => {
                if writer.is_none() {
                    sample_rate = packet.sample_rate.max(1);
                    writer = Some(hound::WavWriter::create(
                        &partial_path,
                        hound::WavSpec {
                            channels: 1,
                            sample_rate,
                            bits_per_sample: 24,
                            sample_format: hound::SampleFormat::Int,
                        },
                    )?);
                    first_timestamp = Some(packet.qpc_100ns);
                }
                if packet.sample_rate != sample_rate {
                    incompatible_rate = true;
                    gaps.push(GapRange {
                        start_frame: sample_frames,
                        end_frame: sample_frames + 1,
                        reason: "sample_rate_changed".into(),
                    });
                    continue;
                }
                if packet.discontinuity
                    && gaps
                        .last()
                        .map(|gap| gap.end_frame != sample_frames)
                        .unwrap_or(true)
                {
                    gaps.push(GapRange {
                        start_frame: sample_frames,
                        end_frame: sample_frames + 1,
                        reason: "capture_discontinuity".into(),
                    });
                }
                let pcm24 = (packet.sample.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                if let Some(output) = writer.as_mut() {
                    output.write_sample(pcm24)?;
                }
                sample_frames += 1;
                last_timestamp = Some(packet.qpc_100ns);
            }
            Err(_) => thread::sleep(std::time::Duration::from_millis(2)),
        }
    }

    if let Some(output) = writer {
        output.finalize()?;
        fs::rename(&partial_path, &final_path)?;
    }

    let capture_overflow = overflow.load(Ordering::Acquire);
    let status = if writer_is_empty(sample_frames) {
        "empty"
    } else if capture_overflow || incompatible_rate || !gaps.is_empty() {
        "incomplete"
    } else {
        "complete"
    };
    let artifact = RecordingArtifact {
        schema_version: "1.0.0".into(),
        session_id: config.session_id,
        song_id: config.song_id,
        status: status.into(),
        raw_wav_path: final_path.to_string_lossy().into_owned(),
        wet_wav_path: None,
        master_wav_path: None,
        mix_status: "pending".into(),
        mix_error: None,
        metadata_path: metadata_path.to_string_lossy().into_owned(),
        sample_rate,
        channels: 1,
        bits_per_sample: 24,
        sample_frames,
        first_timestamp_100ns: first_timestamp,
        last_timestamp_100ns: last_timestamp,
        render_start_timestamp_100ns: None,
        raw_start_relative_us: None,
        start_offset_us: config.start_offset_us,
        backing_gain_db: config.backing_gain_db.clamp(-24.0, 6.0),
        vocal_gain_db: config.vocal_gain_db.clamp(-24.0, 6.0),
        capture_overflow,
        gap_ranges: gaps,
    };
    atomic_json_write(&metadata_path, &artifact)?;
    Ok(artifact)
}

pub fn read_mono_wav(path: &Path) -> Result<(Vec<f32>, u32), RecordingError> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let mut interleaved = Vec::new();
    match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = (1u64 << spec.bits_per_sample.saturating_sub(1)) as f32;
            for sample in reader.samples::<i32>() {
                interleaved.push(sample? as f32 / scale);
            }
        }
        hound::SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                interleaved.push(sample?);
            }
        }
    }
    let mono = interleaved
        .chunks(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / frame.len() as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

#[allow(clippy::too_many_arguments)]
pub fn write_aligned_mix(
    artifact: &mut RecordingArtifact,
    render_start_timestamp_100ns: i64,
    wet_mono: &[f32],
    output_sample_rate: u32,
    backing: &[Vec<f32>],
    backing_sample_rate: u32,
    backing_start_us: u64,
    limiter_ceiling_db: f32,
) -> Result<(), RecordingError> {
    let output_dir = Path::new(&artifact.metadata_path)
        .parent()
        .ok_or_else(|| std::io::Error::other("recording metadata has no parent"))?;
    let wet_path = output_dir.join("wet.wav");
    let master_path = output_dir.join("master.wav");
    let raw_start_relative_us = artifact
        .first_timestamp_100ns
        .map(|value| (value - render_start_timestamp_100ns) / 10)
        .unwrap_or(0);
    let leading_frames =
        ((raw_start_relative_us.max(0) as i128 * output_sample_rate as i128) / 1_000_000) as usize;
    let trimmed_frames = ((raw_start_relative_us.saturating_neg().max(0) as i128
        * output_sample_rate as i128)
        / 1_000_000) as usize;
    let wet_audio = wet_mono.get(trimmed_frames..).unwrap_or(&[]);
    let output_frames = leading_frames.saturating_add(wet_audio.len());

    let spec_mono = hound::WavSpec {
        channels: 1,
        sample_rate: output_sample_rate,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wet_writer = hound::WavWriter::create(&wet_path, spec_mono)?;
    for _ in 0..leading_frames {
        wet_writer.write_sample(0i32)?;
    }
    for &sample in wet_audio {
        wet_writer.write_sample(to_pcm24(sample))?;
    }
    wet_writer.finalize()?;

    let spec_stereo = hound::WavSpec {
        channels: 2,
        sample_rate: output_sample_rate,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut master_writer = hound::WavWriter::create(&master_path, spec_stereo)?;
    let ceiling = 10.0f32.powf(limiter_ceiling_db.clamp(-12.0, 0.0) / 20.0);
    let backing_gain = 10.0f32.powf(artifact.backing_gain_db.clamp(-24.0, 6.0) / 20.0);
    let vocal_gain = 10.0f32.powf(artifact.vocal_gain_db.clamp(-24.0, 6.0) / 20.0);
    let backing_start = backing_start_us as f64 * backing_sample_rate as f64 / 1_000_000.0;
    for frame in 0..output_frames {
        let wet = if frame >= leading_frames {
            wet_audio
                .get(frame - leading_frames)
                .copied()
                .unwrap_or(0.0)
        } else {
            0.0
        } * vocal_gain;
        let source_position =
            backing_start + frame as f64 * backing_sample_rate as f64 / output_sample_rate as f64;
        for channel in 0..2 {
            let backing_sample =
                sample_planar_linear(backing, channel, source_position) * backing_gain;
            master_writer
                .write_sample(to_pcm24((backing_sample + wet).clamp(-ceiling, ceiling)))?;
        }
    }
    master_writer.finalize()?;

    artifact.render_start_timestamp_100ns = Some(render_start_timestamp_100ns);
    artifact.raw_start_relative_us = Some(raw_start_relative_us);
    artifact.wet_wav_path = Some(wet_path.to_string_lossy().into_owned());
    artifact.master_wav_path = Some(master_path.to_string_lossy().into_owned());
    artifact.mix_status = "complete".into();
    artifact.mix_error = None;
    atomic_json_write(Path::new(&artifact.metadata_path), artifact)?;
    Ok(())
}

pub fn mark_mix_failed(
    artifact: &mut RecordingArtifact,
    error: impl Into<String>,
) -> Result<(), RecordingError> {
    artifact.mix_status = "failed".into();
    artifact.mix_error = Some(error.into());
    atomic_json_write(Path::new(&artifact.metadata_path), artifact)
}

fn sample_planar_linear(channels: &[Vec<f32>], channel: usize, position: f64) -> f32 {
    if channels.is_empty() {
        return 0.0;
    }
    let data = &channels[channel.min(channels.len() - 1)];
    let frame = position.floor() as usize;
    let Some(&current) = data.get(frame) else {
        return 0.0;
    };
    let next = data.get(frame + 1).copied().unwrap_or(current);
    current + (next - current) * (position - frame as f64) as f32
}

fn to_pcm24(sample: f32) -> i32 {
    (sample.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32
}

fn writer_is_empty(frames: u64) -> bool {
    frames == 0
}

fn atomic_json_write(path: &Path, value: &RecordingArtifact) -> Result<(), RecordingError> {
    let temporary = path.with_extension("json.tmp");
    let backup = path.with_extension("json.bak");
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    fs::write(&temporary, bytes)?;
    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup)?;
        }
        fs::rename(path, &backup)?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        return Err(error.into());
    }
    if backup.exists() {
        fs::remove_file(backup)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;

    #[test]
    fn writes_24_bit_raw_wav_and_metadata() {
        let root =
            std::env::temp_dir().join(format!("karaoke-recording-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (mut producer, consumer) = RingBuffer::new(128);
        let (mut recorder, _) = RawRecorder::start(
            RecordingConfig {
                output_dir: root.clone(),
                session_id: "session-test".into(),
                song_id: "song-test".into(),
                start_offset_us: 0,
                backing_gain_db: -6.0,
                vocal_gain_db: -3.0,
            },
            consumer,
        );
        for index in 0..100 {
            producer
                .push(RawSample {
                    sample: if index % 2 == 0 { 0.25 } else { -0.25 },
                    qpc_100ns: index * 208,
                    discontinuity: false,
                    sample_rate: 48_000,
                })
                .unwrap();
        }
        drop(producer);
        let mut artifact = recorder.stop().unwrap();
        assert_eq!(artifact.status, "complete");
        assert_eq!(artifact.sample_frames, 100);
        assert_eq!(artifact.backing_gain_db, -6.0);
        assert_eq!(artifact.vocal_gain_db, -3.0);
        assert!(root.join("raw.wav").is_file());
        assert!(root.join("recording.json").is_file());
        let reader = hound::WavReader::open(root.join("raw.wav")).unwrap();
        assert_eq!(reader.spec().bits_per_sample, 24);
        assert_eq!(reader.duration(), 100);
        drop(reader);

        let wet = vec![0.1; 100];
        let backing = vec![vec![0.2; 100], vec![-0.2; 100]];
        write_aligned_mix(&mut artifact, 0, &wet, 48_000, &backing, 48_000, 0, -1.0).unwrap();
        assert_eq!(artifact.mix_status, "complete");
        assert!(root.join("wet.wav").is_file());
        assert!(root.join("master.wav").is_file());
        let mut wet_reader = hound::WavReader::open(root.join("wet.wav")).unwrap();
        let wet_first = wet_reader.samples::<i32>().next().unwrap().unwrap() as f32 / 8_388_607.0;
        assert!(
            (wet_first - 0.1).abs() < 0.001,
            "Wet must not receive MIX gain"
        );
        let mut master_reader = hound::WavReader::open(root.join("master.wav")).unwrap();
        let master_samples: Vec<i32> = master_reader
            .samples::<i32>()
            .take(2)
            .map(Result::unwrap)
            .collect();
        let left = master_samples[0] as f32 / 8_388_607.0;
        let right = master_samples[1] as f32 / 8_388_607.0;
        assert!((left - 0.171).abs() < 0.002);
        assert!((right + 0.029).abs() < 0.002);
        let master = hound::WavReader::open(root.join("master.wav")).unwrap();
        assert_eq!(master.spec().channels, 2);
        assert_eq!(master.duration(), 100);
        drop(master);
        fs::remove_dir_all(root).unwrap();
    }
}
