use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChartValidationError {
    #[error("Unsupported schema version: {0} (expected 2.0.0)")]
    UnsupportedSchemaVersion(String),
    #[error("Duplicate ID '{id}' found in {collection}")]
    DuplicateId {
        id: String,
        collection: &'static str,
    },
    #[error(
        "Invalid time interval for {item_type} '{id}': startUs ({start_us}) >= endUs ({end_us})"
    )]
    InvalidInterval {
        id: String,
        item_type: &'static str,
        start_us: u64,
        end_us: u64,
    },
    #[error(
        "{item_type} '{id}' exceeds song duration: endUs ({end_us}) > durationUs ({duration_us})"
    )]
    ExceedsDuration {
        id: String,
        item_type: &'static str,
        end_us: u64,
        duration_us: u64,
    },
    #[error("Overlapping notes found in track '{track_id}': note '{id1}' [{s1}..{e1}) overlaps note '{id2}' [{s2}..{e2})")]
    OverlappingNotes {
        track_id: String,
        id1: String,
        s1: u64,
        e1: u64,
        id2: String,
        s2: u64,
        e2: u64,
    },
    #[error("Tempo map must start at timeUs=0, found: {0}")]
    TempoMapInvalidStart(u64),
    #[error("Tempo map events not strictly monotonic in time: {0} followed by {1}")]
    TempoMapNonMonotonic(u64, u64),
    #[error("BPM must be strictly positive, found: {0}")]
    InvalidBpm(f64),
    #[error("Backing media missing or multiple found: expected exactly 1, found {0}")]
    InvalidBackingCount(usize),
    #[error("Backing media sample rate must be 48000, found: {0}")]
    InvalidSampleRate(u32),
    #[error("Backing media does not cover full song duration: media covers [{start_us}..{end_us}), duration is {duration_us}")]
    BackingCoverageInsufficient {
        start_us: i64,
        end_us: i64,
        duration_us: u64,
    },
    #[error("Missing referenced {target_type} '{target_id}' in {source_type} '{source_id}'")]
    DanglingReference {
        source_id: String,
        source_type: &'static str,
        target_id: String,
        target_type: &'static str,
    },
    #[error("Token '{token_id}' does not intersect with referenced note '{note_id}'")]
    TokenNoteNoIntersection { token_id: String, note_id: String },
    #[error("Phrase '{phrase_id}' does not envelop referenced token '{token_id}'")]
    PhraseTokenNotEnveloped { phrase_id: String, token_id: String },
    #[error("Track count must be exactly 1 with role 'lead', found: {0}")]
    InvalidTrackCount(usize),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequiredFeature {
    SingleMelody,
    StepTempo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credit {
    pub text: String,
    #[serde(default)]
    pub source_url: String,
    pub license_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaRole {
    Backing,
    Guide,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaEntry {
    pub id: String,
    pub path: String,
    pub role: MediaRole,
    pub sha256: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub frames: u64,
    pub media_offset_us: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: String,
    pub role: String, // "lead"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoEvent {
    pub time_us: u64,
    pub bpm: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterEvent {
    pub quarter_beat: f64,
    pub numerator: u8,
    pub denominator: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoMap {
    pub anchor_time_us: u64,
    pub anchor_quarter_beat: f64,
    pub events: Vec<TempoEvent>,
    pub meters: Vec<MeterEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: String,
    pub track_id: String,
    pub start_us: u64,
    pub end_us: u64,
    pub pitch_midi: u8,
    pub tuning_cents: f64,
    pub scorable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricToken {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub ruby: String,
    pub note_ids: Vec<String>,
    pub start_us: u64,
    pub end_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Phrase {
    pub id: String,
    pub start_us: u64,
    pub end_us: u64,
    pub token_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chart {
    pub schema_version: String,
    pub required_features: Vec<RequiredFeature>,
    pub song_id: String,
    pub chart_id: String,
    pub chart_revision: u32,
    pub title: String,
    pub artist: String,
    pub duration_us: u64,
    pub credits: Vec<Credit>,
    pub media: Vec<MediaEntry>,
    pub tracks: Vec<Track>,
    pub tempo_map: TempoMap,
    pub notes: Vec<Note>,
    pub lyric_tokens: Vec<LyricToken>,
    pub phrases: Vec<Phrase>,
    #[serde(default)]
    pub extensions: serde_json::Map<String, serde_json::Value>,
}

impl Chart {
    /// Perform full semantic validation as defined in §11.4 and Appendix A.1
    pub fn validate_semantics(&self) -> Result<(), ChartValidationError> {
        if self.schema_version != "2.0.0" {
            return Err(ChartValidationError::UnsupportedSchemaVersion(
                self.schema_version.clone(),
            ));
        }

        if self.tracks.len() != 1 || self.tracks[0].role != "lead" {
            return Err(ChartValidationError::InvalidTrackCount(self.tracks.len()));
        }
        let lead_track_id = &self.tracks[0].id;

        // Check Media
        let mut backing_count = 0;
        let mut media_ids = HashSet::new();
        for m in &self.media {
            if !media_ids.insert(&m.id) {
                return Err(ChartValidationError::DuplicateId {
                    id: m.id.clone(),
                    collection: "media",
                });
            }
            if m.role == MediaRole::Backing {
                backing_count += 1;
                if m.sample_rate != 48000 {
                    return Err(ChartValidationError::InvalidSampleRate(m.sample_rate));
                }
                // Check coverage
                let media_len_us =
                    (m.frames as f64 / m.sample_rate as f64 * 1_000_000.0).round() as i64;
                let start_cov = m.media_offset_us;
                let end_cov = m.media_offset_us + media_len_us;
                if start_cov > 0 || end_cov < self.duration_us as i64 {
                    return Err(ChartValidationError::BackingCoverageInsufficient {
                        start_us: start_cov,
                        end_us: end_cov,
                        duration_us: self.duration_us,
                    });
                }
            }
        }
        if backing_count != 1 {
            return Err(ChartValidationError::InvalidBackingCount(backing_count));
        }

        // Check TempoMap
        if self.tempo_map.events.is_empty() {
            return Err(ChartValidationError::TempoMapInvalidStart(0));
        }
        if self.tempo_map.events[0].time_us != 0 {
            return Err(ChartValidationError::TempoMapInvalidStart(
                self.tempo_map.events[0].time_us,
            ));
        }
        let mut prev_time = 0;
        for (i, ev) in self.tempo_map.events.iter().enumerate() {
            if ev.bpm <= 0.0 {
                return Err(ChartValidationError::InvalidBpm(ev.bpm));
            }
            if i > 0 && ev.time_us <= prev_time {
                return Err(ChartValidationError::TempoMapNonMonotonic(
                    prev_time, ev.time_us,
                ));
            }
            prev_time = ev.time_us;
        }

        // Check Notes
        let mut note_map = HashMap::new();
        let mut sorted_notes: Vec<&Note> = self.notes.iter().collect();
        sorted_notes.sort_by_key(|n| (n.start_us, n.end_us));

        for n in &self.notes {
            if n.start_us >= n.end_us {
                return Err(ChartValidationError::InvalidInterval {
                    id: n.id.clone(),
                    item_type: "note",
                    start_us: n.start_us,
                    end_us: n.end_us,
                });
            }
            if n.end_us > self.duration_us {
                return Err(ChartValidationError::ExceedsDuration {
                    id: n.id.clone(),
                    item_type: "note",
                    end_us: n.end_us,
                    duration_us: self.duration_us,
                });
            }
            if &n.track_id != lead_track_id {
                return Err(ChartValidationError::DanglingReference {
                    source_id: n.id.clone(),
                    source_type: "note",
                    target_id: n.track_id.clone(),
                    target_type: "track",
                });
            }
            if note_map.insert(&n.id, n).is_some() {
                return Err(ChartValidationError::DuplicateId {
                    id: n.id.clone(),
                    collection: "notes",
                });
            }
        }

        // Check Note overlaps in single-melody track
        for i in 0..sorted_notes.len() {
            if i + 1 < sorted_notes.len() {
                let curr = sorted_notes[i];
                let next = sorted_notes[i + 1];
                if curr.end_us > next.start_us {
                    return Err(ChartValidationError::OverlappingNotes {
                        track_id: curr.track_id.clone(),
                        id1: curr.id.clone(),
                        s1: curr.start_us,
                        e1: curr.end_us,
                        id2: next.id.clone(),
                        s2: next.start_us,
                        e2: next.end_us,
                    });
                }
            }
        }

        // Check LyricTokens
        let mut token_map = HashMap::new();
        for t in &self.lyric_tokens {
            if t.start_us >= t.end_us {
                return Err(ChartValidationError::InvalidInterval {
                    id: t.id.clone(),
                    item_type: "token",
                    start_us: t.start_us,
                    end_us: t.end_us,
                });
            }
            if t.end_us > self.duration_us {
                return Err(ChartValidationError::ExceedsDuration {
                    id: t.id.clone(),
                    item_type: "token",
                    end_us: t.end_us,
                    duration_us: self.duration_us,
                });
            }
            if token_map.insert(&t.id, t).is_some() {
                return Err(ChartValidationError::DuplicateId {
                    id: t.id.clone(),
                    collection: "lyricTokens",
                });
            }
            // Check note intersection
            for nid in &t.note_ids {
                let note =
                    note_map
                        .get(nid)
                        .ok_or_else(|| ChartValidationError::DanglingReference {
                            source_id: t.id.clone(),
                            source_type: "token",
                            target_id: nid.clone(),
                            target_type: "note",
                        })?;
                // Must intersect: !(token.end <= note.start || token.start >= note.end)
                if t.end_us <= note.start_us || t.start_us >= note.end_us {
                    return Err(ChartValidationError::TokenNoteNoIntersection {
                        token_id: t.id.clone(),
                        note_id: nid.clone(),
                    });
                }
            }
        }

        // Check Phrases
        let mut phrase_map = HashMap::new();
        for p in &self.phrases {
            if p.start_us >= p.end_us {
                return Err(ChartValidationError::InvalidInterval {
                    id: p.id.clone(),
                    item_type: "phrase",
                    start_us: p.start_us,
                    end_us: p.end_us,
                });
            }
            if p.end_us > self.duration_us {
                return Err(ChartValidationError::ExceedsDuration {
                    id: p.id.clone(),
                    item_type: "phrase",
                    end_us: p.end_us,
                    duration_us: self.duration_us,
                });
            }
            if phrase_map.insert(&p.id, p).is_some() {
                return Err(ChartValidationError::DuplicateId {
                    id: p.id.clone(),
                    collection: "phrases",
                });
            }
            for tid in &p.token_ids {
                let tok =
                    token_map
                        .get(tid)
                        .ok_or_else(|| ChartValidationError::DanglingReference {
                            source_id: p.id.clone(),
                            source_type: "phrase",
                            target_id: tid.clone(),
                            target_type: "token",
                        })?;
                // Phrase must envelop token: p.start <= tok.start && p.end >= tok.end
                if p.start_us > tok.start_us || p.end_us < tok.end_us {
                    return Err(ChartValidationError::PhraseTokenNotEnveloped {
                        phrase_id: p.id.clone(),
                        token_id: tid.clone(),
                    });
                }
            }
        }

        Ok(())
    }
}
