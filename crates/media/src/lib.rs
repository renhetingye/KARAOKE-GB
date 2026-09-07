pub mod decoder;
pub mod pitch_shift;
pub mod synthetic;

pub use decoder::{AudioDecoder, CanonicalAudio, MediaError, CANONICAL_SAMPLE_RATE};
pub use pitch_shift::{
    stretch_preserving_pitch, transpose_preserving_duration, TransposeError, MAX_KEY_SEMITONES,
    MAX_SPEED_RATIO, MIN_SPEED_RATIO,
};
pub use synthetic::SyntheticAudioGenerator;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn workspace_fixture(relative: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative)
    }

    #[test]
    fn test_decode_shining_star_if_exists() {
        let path = workspace_fixture("tmpmusic/maou_14_shining_star.ogg");
        if !path.exists() {
            eprintln!("File not found at {:?}, skipping", path);
            return;
        }

        let audio = AudioDecoder::decode_file(&path).expect("Failed to decode Shining Star OGG");
        assert_eq!(audio.sample_rate, CANONICAL_SAMPLE_RATE);
        assert!(audio.channels >= 1);
        assert!(audio.frames > 0);
        assert!(audio.duration_us > 0);
        println!(
            "Decoded Shining Star: channels={}, frames={}, duration={:.2}s, sha256={}",
            audio.channels,
            audio.frames,
            audio.duration_us as f64 / 1_000_000.0,
            audio.source_sha256
        );
    }

    #[test]
    #[ignore = "full-track performance smoke; run explicitly"]
    fn test_transpose_shining_star_full_track_if_exists() {
        let path = workspace_fixture("tmpmusic/maou_14_shining_star.ogg");
        if !path.exists() {
            return;
        }
        let audio = AudioDecoder::decode_file(&path).expect("decode failed");
        let started = std::time::Instant::now();
        let shifted = transpose_preserving_duration(&audio, -3).expect("transpose failed");
        assert_eq!(shifted.frames, audio.frames);
        assert_eq!(shifted.duration_us, audio.duration_us);
        eprintln!("full-track -3 preparation: {:.2?}", started.elapsed());
    }

    #[test]
    #[ignore = "full-track performance smoke; run explicitly"]
    fn test_speed_shining_star_full_track_if_exists() {
        let path = workspace_fixture("tmpmusic/maou_14_shining_star.ogg");
        if !path.exists() {
            return;
        }
        let audio = AudioDecoder::decode_file(&path).expect("decode failed");
        let started = std::time::Instant::now();
        let prepared = stretch_preserving_pitch(&audio, 1.25).expect("speed preparation failed");
        let expected = (audio.frames as f64 / 1.25).round() as usize;
        assert_eq!(prepared.frames, expected);
        assert_eq!(prepared.channels, audio.channels);
        eprintln!("full-track 1.25x preparation: {:.2?}", started.elapsed());
    }
}
