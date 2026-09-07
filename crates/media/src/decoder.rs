use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::path::Path;
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use thiserror::Error;

pub const CANONICAL_SAMPLE_RATE: u32 = 48000;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Symphonia decode error: {0}")]
    Decode(#[from] SymphoniaError),
    #[error("Resampling error: {0}")]
    Resample(String),
    #[error("No supported audio track found in file")]
    NoTrackFound,
    #[error("Unsupported channel count: {0} (only mono/stereo supported)")]
    UnsupportedChannels(usize),
}

#[derive(Debug, Clone)]
pub struct CanonicalAudio {
    pub sample_rate: u32,
    pub channels: usize,
    pub frames: usize,
    pub duration_us: u64,
    pub data: Vec<Vec<f32>>, // [channel][frame]
    pub source_sha256: String,
    pub pcm_sha256: String,
}

pub struct AudioDecoder;

impl AudioDecoder {
    /// Decodes an audio file (OGG, MP3, WAV, FLAC, etc.) into 48kHz float32 canonical PCM (§10.1)
    pub fn decode_file<P: AsRef<Path>>(path: P) -> Result<CanonicalAudio, MediaError> {
        let path_ref = path.as_ref();
        let mut file = File::open(path_ref)?;

        // Compute source file SHA-256
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher)?;
        let source_sha256 = format!("{:x}", hasher.finalize());

        // Re-open for decoding
        let file = File::open(path_ref)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path_ref.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let probed = symphonia::default::get_probe().format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )?;

        let mut format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(MediaError::NoTrackFound)?;

        let track_id = track.id;
        let orig_rate = track.codec_params.sample_rate.unwrap_or(44100);
        let orig_channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);

        if orig_channels > 2 {
            return Err(MediaError::UnsupportedChannels(orig_channels));
        }

        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())?;

        let mut decoded_channels: Vec<Vec<f32>> = vec![Vec::new(); orig_channels];

        loop {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(SymphoniaError::ResetRequired) => break,
                Err(e) => return Err(MediaError::Decode(e)),
            };

            if packet.track_id() != track_id {
                continue;
            }

            match decoder.decode(&packet) {
                Ok(audio_buf) => {
                    Self::append_audio_buffer(&audio_buf, &mut decoded_channels);
                }
                Err(SymphoniaError::DecodeError(_)) => {
                    // Skip corrupt packet
                    continue;
                }
                Err(e) => return Err(MediaError::Decode(e)),
            }
        }

        // Resample to 48000 Hz if needed
        let canonical_channels = if orig_rate == CANONICAL_SAMPLE_RATE {
            decoded_channels
        } else {
            Self::resample_channels(decoded_channels, orig_rate, CANONICAL_SAMPLE_RATE)?
        };

        let frames = canonical_channels.first().map(|c| c.len()).unwrap_or(0);
        let duration_us =
            (frames as f64 / CANONICAL_SAMPLE_RATE as f64 * 1_000_000.0).round() as u64;

        // Compute PCM SHA-256
        let mut pcm_hasher = Sha256::new();
        for ch in &canonical_channels {
            for &s in ch {
                pcm_hasher.update(s.to_le_bytes());
            }
        }
        let pcm_sha256 = format!("{:x}", pcm_hasher.finalize());

        Ok(CanonicalAudio {
            sample_rate: CANONICAL_SAMPLE_RATE,
            channels: canonical_channels.len(),
            frames,
            duration_us,
            data: canonical_channels,
            source_sha256,
            pcm_sha256,
        })
    }

    fn append_audio_buffer(audio_buf: &AudioBufferRef, channels: &mut [Vec<f32>]) {
        match audio_buf {
            AudioBufferRef::F32(buf) => {
                for (ch_idx, out) in channels.iter_mut().enumerate() {
                    if ch_idx < buf.spec().channels.count() {
                        let plane = buf.chan(ch_idx);
                        out.extend_from_slice(plane);
                    }
                }
            }
            AudioBufferRef::S16(buf) => {
                for (ch_idx, out) in channels.iter_mut().enumerate() {
                    if ch_idx < buf.spec().channels.count() {
                        let plane = buf.chan(ch_idx);
                        out.reserve(plane.len());
                        for &s in plane {
                            out.push(s as f32 / 32768.0);
                        }
                    }
                }
            }
            AudioBufferRef::S24(buf) => {
                for (ch_idx, out) in channels.iter_mut().enumerate() {
                    if ch_idx < buf.spec().channels.count() {
                        let plane = buf.chan(ch_idx);
                        out.reserve(plane.len());
                        for &s in plane {
                            out.push(s.0 as f32 / 8388608.0);
                        }
                    }
                }
            }
            AudioBufferRef::S32(buf) => {
                for (ch_idx, out) in channels.iter_mut().enumerate() {
                    if ch_idx < buf.spec().channels.count() {
                        let plane = buf.chan(ch_idx);
                        out.reserve(plane.len());
                        for &s in plane {
                            out.push(s as f32 / 2147483648.0);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn resample_channels(
        input: Vec<Vec<f32>>,
        from_rate: u32,
        to_rate: u32,
    ) -> Result<Vec<Vec<f32>>, MediaError> {
        let params = SincInterpolationParameters {
            sinc_len: 64,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };

        let chunk_size = 1024;
        let mut resampler = SincFixedIn::<f32>::new(
            to_rate as f64 / from_rate as f64,
            2.0,
            params,
            chunk_size,
            input.len(),
        )
        .map_err(|e| MediaError::Resample(e.to_string()))?;

        let num_channels = input.len();
        let total_in_frames = input[0].len();
        let mut output_channels: Vec<Vec<f32>> = vec![Vec::new(); num_channels];

        let mut offset = 0;
        while offset + chunk_size <= total_in_frames {
            let mut chunk: Vec<&[f32]> = Vec::with_capacity(num_channels);
            for ch in &input {
                chunk.push(&ch[offset..offset + chunk_size]);
            }

            let out_chunk = resampler
                .process(&chunk, None)
                .map_err(|e| MediaError::Resample(e.to_string()))?;

            for (ch_idx, out_plane) in out_chunk.iter().enumerate() {
                output_channels[ch_idx].extend_from_slice(out_plane);
            }
            offset += chunk_size;
        }

        // Process leftover frames with zero padding
        if offset < total_in_frames {
            let leftover = total_in_frames - offset;
            let mut padded_chunk: Vec<Vec<f32>> = vec![vec![0.0; chunk_size]; num_channels];
            for (ch_idx, ch) in input.iter().enumerate() {
                padded_chunk[ch_idx][..leftover].copy_from_slice(&ch[offset..]);
            }
            let chunk_refs: Vec<&[f32]> = padded_chunk.iter().map(|c| c.as_slice()).collect();
            let out_chunk = resampler
                .process(&chunk_refs, None)
                .map_err(|e| MediaError::Resample(e.to_string()))?;

            let target_out_frames =
                ((leftover as f64 * to_rate as f64) / from_rate as f64).round() as usize;
            for (ch_idx, out_plane) in out_chunk.iter().enumerate() {
                let take_len = out_plane.len().min(target_out_frames);
                output_channels[ch_idx].extend_from_slice(&out_plane[..take_len]);
            }
        }

        Ok(output_channels)
    }
}
