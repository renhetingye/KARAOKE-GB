use crate::CanonicalAudio;
use rustfft::{num_complex::Complex, FftPlanner};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_KEY_SEMITONES: i32 = 6;
pub const MIN_SPEED_RATIO: f32 = 0.70;
pub const MAX_SPEED_RATIO: f32 = 1.30;

#[derive(Debug, Error)]
pub enum TransposeError {
    #[error("key must be between -{MAX_KEY_SEMITONES} and +{MAX_KEY_SEMITONES}, got {0}")]
    KeyOutOfRange(i32),
    #[error("audio channel layout is invalid")]
    InvalidLayout,
    #[error("audio is too short for the pitch-shift engine")]
    AudioTooShort,
    #[error("speed must be between {MIN_SPEED_RATIO:.2} and {MAX_SPEED_RATIO:.2}, got {0}")]
    SpeedOutOfRange(f32),
}

/// Changes playback duration while preserving the perceived musical pitch.
/// This is an offline preparation step; real-time callbacks only read the
/// prepared PCM, so they never allocate or run FFT work.
pub fn stretch_preserving_pitch(
    source: &CanonicalAudio,
    speed_ratio: f32,
) -> Result<CanonicalAudio, TransposeError> {
    if !(MIN_SPEED_RATIO..=MAX_SPEED_RATIO).contains(&speed_ratio) {
        return Err(TransposeError::SpeedOutOfRange(speed_ratio));
    }
    validate_layout(source)?;
    if (speed_ratio - 1.0).abs() < f32::EPSILON {
        return Ok(source.clone());
    }

    let target_frames = ((source.frames as f64 / speed_ratio as f64).round() as usize).max(1);
    let data = source
        .data
        .iter()
        .map(|channel| {
            let resampled = resample_to_len(channel, target_frames);
            shift_channel_factor(&resampled, 1.0 / speed_ratio)
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(canonical_from_processed(source, data, target_frames))
}

/// Shifts the backing track while keeping its frame count and duration fixed.
/// The chart itself and microphone signal are deliberately left untouched.
pub fn transpose_preserving_duration(
    source: &CanonicalAudio,
    semitones: i32,
) -> Result<CanonicalAudio, TransposeError> {
    if !(-MAX_KEY_SEMITONES..=MAX_KEY_SEMITONES).contains(&semitones) {
        return Err(TransposeError::KeyOutOfRange(semitones));
    }
    validate_layout(source)?;
    if semitones == 0 {
        return Ok(source.clone());
    }

    let data = source
        .data
        .iter()
        .map(|channel| shift_channel_factor(channel, 2.0f32.powf(semitones as f32 / 12.0)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(canonical_from_processed(source, data, source.frames))
}

/// Phase-vocoder frequency remapping. Analysis and synthesis use the same hop,
/// so the backing duration and every chart timestamp remain unchanged.
fn shift_channel_factor(input: &[f32], factor: f32) -> Result<Vec<f32>, TransposeError> {
    const WINDOW: usize = 4096;
    const HOP: usize = 1024;
    if input.len() < WINDOW {
        return Err(TransposeError::AudioTooShort);
    }

    let bins = WINDOW / 2 + 1;
    let window = (0..WINDOW)
        .map(|index| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * index as f32 / WINDOW as f32).cos())
        .collect::<Vec<_>>();

    let mut planner = FftPlanner::<f32>::new();
    let forward = planner.plan_fft_forward(WINDOW);
    let inverse = planner.plan_fft_inverse(WINDOW);
    let mut spectrum = vec![Complex::new(0.0, 0.0); WINDOW];
    let mut shifted = vec![Complex::new(0.0, 0.0); WINDOW];
    let mut previous_phase = vec![0.0f32; bins];
    let mut output_phase = vec![0.0f32; bins];
    let mut remapped_magnitude = vec![0.0f32; bins];
    let mut remapped_frequency_weight = vec![0.0f32; bins];
    let mut output = vec![0.0f32; input.len() + WINDOW];
    let mut weight = vec![0.0f32; output.len()];

    let mut frame_start = 0usize;
    while frame_start < input.len() {
        for index in 0..WINDOW {
            spectrum[index] = Complex::new(
                input.get(frame_start + index).copied().unwrap_or(0.0) * window[index],
                0.0,
            );
            shifted[index] = Complex::new(0.0, 0.0);
        }
        forward.process(&mut spectrum);
        remapped_magnitude.fill(0.0);
        remapped_frequency_weight.fill(0.0);

        for bin in 0..bins {
            let magnitude = spectrum[bin].norm();
            let phase = spectrum[bin].arg();
            let expected = 2.0 * std::f32::consts::PI * HOP as f32 * bin as f32 / WINDOW as f32;
            let mut delta = phase - previous_phase[bin] - expected;
            delta -= 2.0 * std::f32::consts::PI * (delta / (2.0 * std::f32::consts::PI)).round();
            previous_phase[bin] = phase;

            let true_bin =
                bin as f32 + delta * WINDOW as f32 / (2.0 * std::f32::consts::PI * HOP as f32);
            let target_bin = (bin as f32 * factor).round() as usize;
            if target_bin < bins {
                remapped_magnitude[target_bin] += magnitude;
                remapped_frequency_weight[target_bin] += magnitude * true_bin * factor;
            }
        }

        for target_bin in 0..bins {
            let magnitude = remapped_magnitude[target_bin];
            if magnitude > 0.0 {
                let true_target_bin = remapped_frequency_weight[target_bin] / magnitude;
                output_phase[target_bin] +=
                    2.0 * std::f32::consts::PI * HOP as f32 * true_target_bin / WINDOW as f32;
                shifted[target_bin] = Complex::from_polar(magnitude, output_phase[target_bin]);
            }
        }

        for bin in 1..WINDOW / 2 {
            shifted[WINDOW - bin] = shifted[bin].conj();
        }
        inverse.process(&mut shifted);
        for index in 0..WINDOW {
            let output_index = frame_start + index;
            let gain = window[index];
            output[output_index] += shifted[index].re * gain / WINDOW as f32;
            weight[output_index] += gain * gain;
        }
        frame_start += HOP;
    }

    output.truncate(input.len());
    for (sample, normalization) in output.iter_mut().zip(weight.iter()) {
        if *normalization > 1e-6 {
            *sample /= *normalization;
        }
    }
    Ok(output)
}

fn validate_layout(source: &CanonicalAudio) -> Result<(), TransposeError> {
    if source.channels == 0
        || source.frames == 0
        || source.data.len() != source.channels
        || source
            .data
            .iter()
            .any(|channel| channel.len() != source.frames)
    {
        Err(TransposeError::InvalidLayout)
    } else {
        Ok(())
    }
}

fn resample_to_len(input: &[f32], output_len: usize) -> Vec<f32> {
    if output_len == input.len() {
        return input.to_vec();
    }
    let step = input.len() as f64 / output_len as f64;
    (0..output_len)
        .map(|index| {
            let position = index as f64 * step;
            let frame = position.floor() as usize;
            let current = input.get(frame).copied().unwrap_or(0.0);
            let next = input.get(frame + 1).copied().unwrap_or(current);
            current + (next - current) * (position - frame as f64) as f32
        })
        .collect()
}

fn canonical_from_processed(
    source: &CanonicalAudio,
    data: Vec<Vec<f32>>,
    frames: usize,
) -> CanonicalAudio {
    let mut pcm_hasher = Sha256::new();
    for channel in &data {
        for sample in channel {
            pcm_hasher.update(sample.to_le_bytes());
        }
    }
    CanonicalAudio {
        sample_rate: source.sample_rate,
        channels: source.channels,
        frames,
        duration_us: (frames as u128 * 1_000_000u128 / source.sample_rate as u128) as u64,
        data,
        source_sha256: source.source_sha256.clone(),
        pcm_sha256: format!("{:x}", pcm_hasher.finalize()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_audio(frequency_hz: f32, seconds: f32) -> CanonicalAudio {
        let sample_rate = 48_000;
        let frames = (sample_rate as f32 * seconds) as usize;
        let channel = (0..frames)
            .map(|index| {
                (2.0 * std::f32::consts::PI * frequency_hz * index as f32 / sample_rate as f32)
                    .sin()
                    * 0.25
            })
            .collect::<Vec<_>>();
        CanonicalAudio {
            sample_rate,
            channels: 1,
            frames,
            duration_us: (seconds * 1_000_000.0) as u64,
            data: vec![channel],
            source_sha256: "fixture".into(),
            pcm_sha256: "fixture".into(),
        }
    }

    #[test]
    fn transpose_keeps_duration_and_layout() {
        let source = sine_audio(440.0, 1.0);
        let shifted = transpose_preserving_duration(&source, -3).unwrap();
        assert_eq!(shifted.sample_rate, source.sample_rate);
        assert_eq!(shifted.channels, source.channels);
        assert_eq!(shifted.frames, source.frames);
        assert_eq!(shifted.duration_us, source.duration_us);
        assert_ne!(shifted.pcm_sha256, source.pcm_sha256);

        let fft_size = 16_384;
        let offset = 16_384;
        let mut spectrum = shifted.data[0][offset..offset + fft_size]
            .iter()
            .enumerate()
            .map(|(index, sample)| {
                let hann =
                    0.5 - 0.5 * (2.0 * std::f32::consts::PI * index as f32 / fft_size as f32).cos();
                Complex::new(sample * hann, 0.0)
            })
            .collect::<Vec<_>>();
        FftPlanner::<f32>::new()
            .plan_fft_forward(fft_size)
            .process(&mut spectrum);
        let dominant_bin = spectrum[..fft_size / 2]
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.norm().total_cmp(&right.norm()))
            .unwrap()
            .0;
        let dominant_hz = dominant_bin as f32 * source.sample_rate as f32 / fft_size as f32;
        let expected_hz = 440.0 * 2.0f32.powf(-3.0 / 12.0);
        assert!(
            (dominant_hz - expected_hz).abs() < 8.0,
            "expected about {expected_hz:.1} Hz, got {dominant_hz:.1} Hz"
        );
    }

    #[test]
    fn rejects_key_outside_product_range() {
        let source = sine_audio(440.0, 1.0);
        assert!(matches!(
            transpose_preserving_duration(&source, 7),
            Err(TransposeError::KeyOutOfRange(7))
        ));
    }

    #[test]
    fn accepts_karaoke_key_boundaries() {
        let source = sine_audio(220.0, 1.0);
        for (semitones, expected_hz) in [(-6, 155.56), (6, 311.13)] {
            let shifted = transpose_preserving_duration(&source, semitones).unwrap();
            assert_eq!(shifted.frames, source.frames);
            assert_eq!(shifted.duration_us, source.duration_us);
            let fft_size = 16_384;
            let offset = 16_384;
            let mut spectrum = shifted.data[0][offset..offset + fft_size]
                .iter()
                .map(|sample| Complex::new(*sample, 0.0))
                .collect::<Vec<_>>();
            FftPlanner::<f32>::new()
                .plan_fft_forward(fft_size)
                .process(&mut spectrum);
            let dominant_bin = spectrum[..fft_size / 2]
                .iter()
                .enumerate()
                .max_by(|(_, left), (_, right)| left.norm().total_cmp(&right.norm()))
                .unwrap()
                .0;
            let dominant_hz = dominant_bin as f32 * source.sample_rate as f32 / fft_size as f32;
            assert!(
                (dominant_hz - expected_hz).abs() < 8.0,
                "{semitones:+} expected {expected_hz:.1} Hz, got {dominant_hz:.1} Hz"
            );
        }
    }

    #[test]
    fn speed_change_preserves_pitch_and_changes_duration() {
        let source = sine_audio(440.0, 1.0);
        let faster = stretch_preserving_pitch(&source, 1.25).unwrap();
        assert_eq!(faster.frames, 38_400);
        assert_eq!(faster.duration_us, 800_000);

        let fft_size = 16_384;
        let offset = 8_192;
        let mut spectrum = faster.data[0][offset..offset + fft_size]
            .iter()
            .map(|sample| Complex::new(*sample, 0.0))
            .collect::<Vec<_>>();
        FftPlanner::<f32>::new()
            .plan_fft_forward(fft_size)
            .process(&mut spectrum);
        let dominant_bin = spectrum[..fft_size / 2]
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.norm().total_cmp(&right.norm()))
            .unwrap()
            .0;
        let dominant_hz = dominant_bin as f32 * source.sample_rate as f32 / fft_size as f32;
        assert!((dominant_hz - 440.0).abs() < 8.0, "got {dominant_hz:.1} Hz");
    }

    #[test]
    fn rejects_speed_outside_product_range() {
        let source = sine_audio(440.0, 1.0);
        assert!(matches!(
            stretch_preserving_pitch(&source, 0.69),
            Err(TransposeError::SpeedOutOfRange(_))
        ));
    }
}
