use hound::{WavSpec, WavWriter};
use std::f32::consts::PI;
use std::path::Path;

pub struct SyntheticAudioGenerator;

impl SyntheticAudioGenerator {
    /// Generates 48kHz mono PCM audio for v4 Appendix A.2 timing test:
    /// - 0.0s .. 0.5s: Silence
    /// - 0.5s .. 0.55s: 440Hz Pure Sine (50ms)
    /// - 0.55s .. 1.0s: Silence
    /// - 1.0s .. 1.5s: 440Hz Pure Sine (500ms)
    /// - 1.5s .. 2.0s: Silence
    pub fn generate_a4_timing_fixture(sample_rate: u32) -> Vec<f32> {
        let total_frames = (sample_rate as f64 * 2.0) as usize; // 2.0s
        let mut samples = vec![0.0f32; total_frames];

        let n1_start = (sample_rate as f64 * 0.5) as usize;
        let n1_end = (sample_rate as f64 * 0.55) as usize;

        let n2_start = (sample_rate as f64 * 1.0) as usize;
        let n2_end = (sample_rate as f64 * 1.5) as usize;

        let freq = 440.0f32;

        for (i, sample) in samples.iter_mut().enumerate().take(n1_end).skip(n1_start) {
            let t = (i - n1_start) as f32 / sample_rate as f32;
            *sample = 0.5 * (2.0 * PI * freq * t).sin();
        }

        for (i, sample) in samples.iter_mut().enumerate().take(n2_end).skip(n2_start) {
            let t = (i - n2_start) as f32 / sample_rate as f32;
            *sample = 0.5 * (2.0 * PI * freq * t).sin();
        }

        samples
    }

    /// Saves mono float32 samples to 16-bit PCM WAV
    pub fn write_wav_file<P: AsRef<Path>>(
        path: P,
        samples: &[f32],
        sample_rate: u32,
    ) -> Result<(), hound::Error> {
        let spec = WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = WavWriter::create(path, spec)?;
        for &s in samples {
            let clamped = s.clamp(-1.0, 1.0);
            let int_sample = (clamped * 32767.0) as i16;
            writer.write_sample(int_sample)?;
        }
        writer.finalize()?;
        Ok(())
    }
}
