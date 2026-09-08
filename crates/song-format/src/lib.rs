pub mod chart;
pub mod package;

pub use chart::{
    Chart, ChartValidationError, LyricCue, LyricToken, MediaEntry, Note, Phrase, TempoMap, Track,
};
pub use package::{
    FileRole, ManifestFileEntry, PackageError, PackageManifest, PackageReader, PackageWriter,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_appendix_synthetic_chart_semantics() {
        // Load synthetic chart example from Appendix A.2
        let synthetic_json = r#"{
          "schemaVersion": "2.0.0",
          "requiredFeatures": [
            "single-melody",
            "step-tempo"
          ],
          "songId": "synthetic_test",
          "chartId": "synthetic_chart",
          "chartRevision": 0,
          "title": "Synthetic A4 Timing Test",
          "artist": "Test fixture",
          "durationUs": 2000000,
          "credits": [
            {
              "text": "Generated synthetic test; not Shining Star",
              "sourceUrl": "",
              "licenseName": "Project test fixture"
            }
          ],
          "media": [
            {
              "id": "backing",
              "path": "media/backing.wav",
              "role": "backing",
              "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
              "sampleRate": 48000,
              "channels": 1,
              "frames": 96000,
              "mediaOffsetUs": 0
            }
          ],
          "tracks": [
            {
              "id": "lead",
              "role": "lead"
            }
          ],
          "tempoMap": {
            "anchorTimeUs": 0,
            "anchorQuarterBeat": 0,
            "events": [
              {
                "timeUs": 0,
                "bpm": 120
              }
            ],
            "meters": [
              {
                "quarterBeat": 0,
                "numerator": 4,
                "denominator": 4
              }
            ]
          },
          "notes": [
            {
              "id": "n1",
              "trackId": "lead",
              "startUs": 500000,
              "endUs": 550000,
              "pitchMidi": 69,
              "tuningCents": 0,
              "scorable": true
            },
            {
              "id": "n2",
              "trackId": "lead",
              "startUs": 1000000,
              "endUs": 1500000,
              "pitchMidi": 69,
              "tuningCents": 0,
              "scorable": true
            }
          ],
          "lyricTokens": [
            {
              "id": "t1",
              "text": "あ",
              "ruby": "",
              "noteIds": [
                "n1"
              ],
              "startUs": 500000,
              "endUs": 550000
            },
            {
              "id": "t2",
              "text": "あ",
              "ruby": "",
              "noteIds": [
                "n2"
              ],
              "startUs": 1000000,
              "endUs": 1500000
            }
          ],
          "phrases": [
            {
              "id": "p1",
              "startUs": 500000,
              "endUs": 1500000,
              "tokenIds": [
                "t1",
                "t2"
              ]
            }
          ],
          "extensions": {}
        }"#;

        let mut chart: Chart =
            serde_json::from_str(synthetic_json).expect("Deserialization failed");
        assert_eq!(chart.schema_version, "2.0.0");
        assert!(
            chart.lyric_cues.is_empty(),
            "legacy charts default to no timed lyric cues"
        );
        chart
            .validate_semantics()
            .expect("Semantic validation should pass");

        chart.lyric_cues.push(LyricCue {
            id: "cue1".to_string(),
            text: "A whole lyric line".to_string(),
            start_us: Some(500_000),
            end_us: Some(1_500_000),
        });
        chart
            .validate_semantics()
            .expect("timed lyric cue should validate");
        let encoded = serde_json::to_string(&chart).expect("timed lyric cue should serialize");
        let decoded: Chart = serde_json::from_str(&encoded).expect("timed lyric cue should reload");
        assert_eq!(decoded.lyric_cues.len(), 1);
        assert_eq!(decoded.lyric_cues[0].text, "A whole lyric line");
    }

    #[test]
    fn test_reject_overlapping_notes() {
        let chart: Chart = serde_json::from_str(r#"{
          "schemaVersion": "2.0.0",
          "requiredFeatures": ["single-melody", "step-tempo"],
          "songId": "test",
          "chartId": "test",
          "chartRevision": 0,
          "title": "Test",
          "artist": "Test",
          "durationUs": 2000000,
          "credits": [{"text": "t", "licenseName": "l"}],
          "media": [{
            "id": "m1", "path": "media/backing.wav", "role": "backing",
            "sha256": "00", "sampleRate": 48000, "channels": 1, "frames": 96000, "mediaOffsetUs": 0
          }],
          "tracks": [{"id": "lead", "role": "lead"}],
          "tempoMap": {"anchorTimeUs": 0, "anchorQuarterBeat": 0, "events": [{"timeUs": 0, "bpm": 120}], "meters": []},
          "notes": [
            {"id": "n1", "trackId": "lead", "startUs": 100000, "endUs": 300000, "pitchMidi": 60, "tuningCents": 0, "scorable": true},
            {"id": "n2", "trackId": "lead", "startUs": 250000, "endUs": 400000, "pitchMidi": 62, "tuningCents": 0, "scorable": true}
          ],
          "lyricTokens": [],
          "phrases": [],
          "extensions": {}
        }"#).unwrap();

        let res = chart.validate_semantics();
        assert!(matches!(
            res,
            Err(ChartValidationError::OverlappingNotes { .. })
        ));
    }
}
