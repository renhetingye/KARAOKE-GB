use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "2.0.0";

/// Standard Error Codes defined in §19
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedFormat,
    InvalidChart,
    RevisionConflict,
    DeviceUnavailable,
    CalibrationRequired,
    AudioGap,
    ModelUnavailable,
    OutOfMemory,
    DiskFull,
    Cancelled,
    InternalError,
    NotFound,
    ValidationFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandEnvelope<T> {
    pub protocol_version: String,
    pub request_id: String,
    pub command: String,
    pub payload: T,
    pub expected_revision: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseEnvelope<T> {
    pub request_id: String,
    pub ok: bool,
    pub revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl<T> ResponseEnvelope<T> {
    pub fn success(request_id: impl Into<String>, revision: Option<u64>, result: T) -> Self {
        Self {
            request_id: request_id.into(),
            ok: true,
            revision,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(
        request_id: impl Into<String>,
        revision: Option<u64>,
        code: ErrorCode,
        message: impl Into<String>,
        details: Option<serde_json::Value>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            ok: false,
            revision,
            result: None,
            error: Some(ProtocolError {
                code,
                message: message.into(),
                details,
            }),
        }
    }
}

/// §8.2 Voicing State
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Voicing {
    #[default]
    Silence,
    Unvoiced,
    Voiced,
    Unknown,
}

/// §8.2 Quality Flags bitflags representation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QualityFlags {
    pub low_confidence: bool,
    pub harmonic_ambiguity: bool,
    pub clipping: bool,
    pub input_gap: bool,
    pub timestamp_invalid: bool,
    pub warmup: bool,
    pub out_of_range: bool,
}

impl QualityFlags {
    pub fn is_clean(&self) -> bool {
        !self.low_confidence
            && !self.harmonic_ambiguity
            && !self.clipping
            && !self.input_gap
            && !self.timestamp_invalid
            && !self.warmup
            && !self.out_of_range
    }

    pub fn has_system_gap(&self) -> bool {
        self.input_gap || self.timestamp_invalid
    }
}

/// §6.3 Pitch Frame
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PitchAnalysisFrame {
    pub session_id: String,
    pub epoch_id: u64,
    pub capture_stream_id: u64,
    pub sequence: u64,
    pub window_start_frame: u64,
    pub window_end_frame_exclusive: u64,
    pub analysis_rate: u32,
    pub reference_qpc: i64,
    pub generated_qpc: i64,
    pub f0_hz: Option<f32>,
    pub periodicity: f32,
    pub voicing: Voicing,
    pub quality_flags: QualityFlags,
    pub algorithm_version: String,
}

/// §9.4 Session State
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Ready,
    Playing,
    Paused,
    Valid,
    Practice,
    Interrupted,
    InvalidInput,
    NotScorable,
}

/// Score Snapshot for UI streaming (§5.3)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScoreSnapshot {
    pub session_status: SessionStatus,
    pub current_song_time_us: i64,
    pub interim_score_percent: f64,
    pub raw_pitch_accuracy: f64,
    pub voicing_coverage: f64,
    pub scored_duration_us: i64,
    pub active_note_id: Option<String>,
}

/// §5.3 Display Packet
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayPacket {
    pub protocol_version: String,
    pub session_id: String,
    pub epoch_id: u64,
    pub sequence: u64,
    pub song_time_us: i64,
    pub anchor_qpc: i64,
    pub mic_level: f32,
    pub pitch_frames: Vec<PitchAnalysisFrame>,
    pub score_snapshot: ScoreSnapshot,
}

/// Microphone Diagnostic Telemetry (§18.2)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicDiagnostic {
    pub device_name: String,
    pub total_pcm_frames: u64,
    pub total_packets: u64,
    pub rms_dbfs: f32,
    pub peak_dbfs: f32,
    pub f0_hz: Option<f32>,
    pub midi_note: Option<f32>,
    pub periodicity: f32,
    pub state: String, // "NO_PCM" | "SILENCE" | "UNVOICED" | "VOICED"
}
