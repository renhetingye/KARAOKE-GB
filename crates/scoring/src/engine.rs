use crate::lut::GaussianLut;
use karaoke_protocol::{PitchAnalysisFrame, SessionStatus, Voicing};
use karaoke_song_format::Note;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringConfig {
    pub key_semitones: i32,
    pub octave_tolerance: bool,
    pub tolerance_cents: f64,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            key_semitones: 0,
            octave_tolerance: true,
            tolerance_cents: 35.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreResult {
    pub status: SessionStatus,
    pub total_score: f64,            // 0.0 ..= 100.0
    pub pitch_accuracy_score: f64,   // A * 100
    pub voicing_coverage_score: f64, // C * 100
    pub total_evaluated_duration_us: u64,
    pub total_voiced_duration_us: u64,
    pub raw_system_gap_us: u64,
}

pub struct ScoringEngine {
    config: ScoringConfig,
    lut: GaussianLut,
}

impl ScoringEngine {
    pub fn new(config: ScoringConfig) -> Self {
        let lut = GaussianLut::new(config.tolerance_cents);
        Self { config, lut }
    }

    /// Frequency (Hz) to continuous MIDI pitch number
    pub fn hz_to_midi(hz: f32) -> f64 {
        if hz <= 0.0 {
            return 0.0;
        }
        69.0 + 12.0 * (hz as f64 / 440.0).log2()
    }

    /// Calculate pitch error in cents considering key and octave tolerance (§9.3)
    pub fn calculate_cents_error(&self, measured_midi: f64, target_midi: f64) -> f64 {
        let base_target = target_midi + self.config.key_semitones as f64;
        if !self.config.octave_tolerance {
            return ((measured_midi - base_target) * 100.0).abs();
        }

        // Octave tolerance: evaluate o in {-1, 0, 1}
        // Commercial-style pitch-class scoring: accept the same note name in
        // either direction up to two octaves without changing backing audio or
        // the captured microphone signal.
        let octaves = [0, -1, 1, -2, 2];
        let mut min_diff = f64::MAX;
        for oct in octaves {
            let candidate_target = base_target + (oct as f64 * 12.0);
            let diff = (measured_midi - candidate_target).abs();
            if diff < min_diff {
                min_diff = diff;
            }
        }
        min_diff * 100.0
    }

    /// Evaluates a session against scorable notes and recorded pitch frames (§9.1 - §9.4)
    pub fn evaluate(
        &self,
        notes: &[Note],
        frames: &[PitchAnalysisFrame],
        song_speed: f64,
    ) -> ScoreResult {
        let timed_frames: Vec<(i64, PitchAnalysisFrame)> = frames
            .iter()
            .cloned()
            .map(|frame| (frame.reference_qpc, frame))
            .collect();
        self.evaluate_timed_until(notes, &timed_frames, song_speed, None)
    }

    /// Evaluates frames whose song-time has already been established by the
    /// transport/timeline layer. `PitchAnalysisFrame::reference_qpc` remains a
    /// QPC timestamp and is deliberately not reinterpreted as song time here.
    ///
    /// `end_song_time_us` is used for an interim/practice result. Notes after
    /// that point do not enter the denominator.
    pub fn evaluate_timed_until(
        &self,
        notes: &[Note],
        timed_frames: &[(i64, PitchAnalysisFrame)],
        song_speed: f64,
        end_song_time_us: Option<u64>,
    ) -> ScoreResult {
        let scorable_notes: Vec<&Note> = notes.iter().filter(|n| n.scorable).collect();
        if scorable_notes.is_empty() {
            return ScoreResult {
                status: SessionStatus::NotScorable,
                total_score: 0.0,
                pitch_accuracy_score: 0.0,
                voicing_coverage_score: 0.0,
                total_evaluated_duration_us: 0,
                total_voiced_duration_us: 0,
                raw_system_gap_us: 0,
            };
        }

        // Integration in 5ms (5000us) cells (§9.2)
        let cell_step_us: u64 = 5000;
        let mut d_sum: u128 = 0; // Total weighted denominator duration * weight_scale (weight 1.0=2, 0.5=1)
        let mut a_sum: u128 = 0; // Sum of duration * weight * p(t) (p in 0..1_000_000)
        let mut c_sum: u128 = 0; // Sum of duration * weight * v(t) (v in 0..1_000_000)

        let mut total_eval_us: u64 = 0;
        let mut total_voiced_us: u64 = 0;
        let mut total_gap_us: u64 = 0;
        let mut consecutive_gap_us: u64 = 0;
        let mut max_consecutive_gap_us: u64 = 0;
        let mut clipping_us: u64 = 0;
        let mut consecutive_clipping_us: u64 = 0;
        let mut max_consecutive_clipping_us: u64 = 0;

        // Assumes frames are sorted by reference_qpc / song_time
        for note in scorable_notes {
            let evaluation_end = end_song_time_us
                .map(|end| note.end_us.min(end))
                .unwrap_or(note.end_us);
            if evaluation_end <= note.start_us {
                continue;
            }
            let note_len = note.end_us.saturating_sub(note.start_us);
            if note_len == 0 {
                continue;
            }
            // Boundary width g = min(30000us, floor(d / 10)) (§9.1)
            let g = (note_len / 10).min(30000);
            let boundary_start_end = note.start_us + g;
            let boundary_end_start = note.end_us.saturating_sub(g);

            let mut cell_start = note.start_us;
            while cell_start < evaluation_end {
                // Split not only at 5 ms grid lines but also exactly at both
                // boundary-weight transitions (§9.2).
                let next_grid = ((cell_start / cell_step_us) + 1) * cell_step_us;
                let mut cell_end = next_grid.min(evaluation_end);
                for boundary in [boundary_start_end, boundary_end_start] {
                    if boundary > cell_start && boundary < cell_end {
                        cell_end = boundary;
                    }
                }
                let cell_len = cell_end - cell_start;
                let cell_mid = (cell_start + cell_end) / 2;

                // Weight: 0.5 in boundary g, 1.0 in center (§9.1)
                // We use integer weight: 1 for 0.5, 2 for 1.0
                let weight: u128 =
                    if cell_mid < boundary_start_end || cell_mid >= boundary_end_start {
                        1
                    } else {
                        2
                    };

                let weighted_cell_len = cell_len as u128 * weight;
                d_sum += weighted_cell_len;
                total_eval_us += cell_len;

                let closest = closest_timed_frame(timed_frames, cell_mid as i64);

                let mut p_val: u32 = 0;
                let mut v_val: u32 = 0;

                let mut system_gap = false;
                let mut clipping = false;
                if let Some((frame_song_time_us, frame)) = closest {
                    let frame_dist = (frame_song_time_us - cell_mid as i64).abs();
                    // 1 hop allowed distance (default 256 frames at 48kHz is ~5.33ms = 5333us)
                    let max_dist = (256.0 / frame.analysis_rate.max(1) as f64
                        * 1_000_000.0
                        * song_speed.max(0.01))
                    .ceil() as i64;

                    if frame_dist <= max_dist {
                        if frame.quality_flags.has_system_gap() {
                            total_gap_us += cell_len;
                            system_gap = true;
                        } else if frame.quality_flags.clipping {
                            clipping_us += cell_len;
                            clipping = true;
                        } else if frame.voicing == Voicing::Voiced && frame.quality_flags.is_clean()
                        {
                            if let Some(f0) = frame.f0_hz {
                                if f0 > 0.0 {
                                    let measured_midi = Self::hz_to_midi(f0);
                                    let target_midi =
                                        note.pitch_midi as f64 + (note.tuning_cents / 100.0);
                                    let err_cents =
                                        self.calculate_cents_error(measured_midi, target_midi);
                                    p_val = self.lut.lookup(err_cents);
                                    v_val = 1_000_000;
                                    total_voiced_us += cell_len;
                                }
                            }
                        }
                    }
                }

                if system_gap {
                    consecutive_gap_us += cell_len;
                    max_consecutive_gap_us = max_consecutive_gap_us.max(consecutive_gap_us);
                } else {
                    consecutive_gap_us = 0;
                }
                if clipping {
                    consecutive_clipping_us += cell_len;
                    max_consecutive_clipping_us =
                        max_consecutive_clipping_us.max(consecutive_clipping_us);
                } else {
                    consecutive_clipping_us = 0;
                }

                a_sum += weighted_cell_len * p_val as u128;
                c_sum += weighted_cell_len * v_val as u128;

                cell_start = cell_end;
            }
        }

        if d_sum == 0 {
            return ScoreResult {
                status: SessionStatus::NotScorable,
                total_score: 0.0,
                pitch_accuracy_score: 0.0,
                voicing_coverage_score: 0.0,
                total_evaluated_duration_us: 0,
                total_voiced_duration_us: 0,
                raw_system_gap_us: 0,
            };
        }

        // System-caused loss interrupts certification; microphone clipping is
        // invalid input rather than a system fault (§9.4).
        let status = if total_gap_us > 20_000 || max_consecutive_gap_us > 10_000 {
            SessionStatus::Interrupted
        } else if max_consecutive_clipping_us >= 250_000
            || clipping_us.saturating_mul(100) > total_eval_us
        {
            SessionStatus::InvalidInput
        } else {
            SessionStatus::Valid
        };

        let a = (a_sum as f64) / (d_sum as f64 * 1_000_000.0);
        let c = (c_sum as f64) / (d_sum as f64 * 1_000_000.0);

        // §9.1: S = 100 * (0.90 * A + 0.10 * C)
        let s = 100.0 * (0.90 * a + 0.10 * c);

        // Round to 3 decimal places (0.001 points)
        let rounded_s = (s * 1000.0).round() / 1000.0;
        let rounded_a = ((a * 100.0) * 1000.0).round() / 1000.0;
        let rounded_c = ((c * 100.0) * 1000.0).round() / 1000.0;

        ScoreResult {
            status,
            total_score: rounded_s,
            pitch_accuracy_score: rounded_a,
            voicing_coverage_score: rounded_c,
            total_evaluated_duration_us: total_eval_us,
            total_voiced_duration_us: total_voiced_us,
            raw_system_gap_us: total_gap_us,
        }
    }
}

/// Binary-searches a song-time-sorted frame stream. Ties are resolved by the
/// lower sequence number as required by §9.2.
fn closest_timed_frame(
    frames: &[(i64, PitchAnalysisFrame)],
    target_us: i64,
) -> Option<(i64, &PitchAnalysisFrame)> {
    if frames.is_empty() {
        return None;
    }
    let index = frames.partition_point(|(time, _)| *time < target_us);
    let mut best: Option<(i64, &PitchAnalysisFrame)> = None;
    for candidate_index in [
        index.checked_sub(1),
        (index < frames.len()).then_some(index),
    ]
    .into_iter()
    .flatten()
    {
        let (time, frame) = &frames[candidate_index];
        let candidate_key = ((time - target_us).abs(), frame.sequence);
        if best
            .as_ref()
            .map(|(best_time, best_frame)| {
                candidate_key < ((best_time - target_us).abs(), best_frame.sequence)
            })
            .unwrap_or(true)
        {
            best = Some((*time, frame));
        }
    }
    best
}
