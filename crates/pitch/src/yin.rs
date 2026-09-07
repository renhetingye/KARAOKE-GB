use karaoke_protocol::{PitchAnalysisFrame, QualityFlags, Voicing};

pub struct YinDetector {
    sample_rate: u32,
    window_size: usize,
    threshold: f32,
    min_lag: usize, // e.g. 48000 / 1000 = 48
    max_lag: usize, // e.g. 48000 / 80 = 600
    d_buffer: Vec<f32>,
    cmnd_buffer: Vec<f32>,
}

impl YinDetector {
    pub fn new(sample_rate: u32, window_size: usize, threshold: f32) -> Self {
        let min_freq = 60.0;
        let max_freq = 1500.0;
        let min_lag = (sample_rate as f32 / max_freq).floor() as usize;
        let max_lag = ((sample_rate as f32 / min_freq).ceil() as usize).min(window_size / 2);

        Self {
            sample_rate,
            window_size,
            threshold,
            min_lag,
            max_lag,
            d_buffer: vec![0.0; max_lag + 1],
            cmnd_buffer: vec![0.0; max_lag + 1],
        }
    }

    /// Process a window of audio samples and return pitch frame information
    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &mut self,
        samples: &[f32],
        session_id: &str,
        epoch_id: u64,
        capture_stream_id: u64,
        sequence: u64,
        window_start_frame: u64,
        window_end_frame_exclusive: u64,
        reference_qpc: i64,
        generated_qpc: i64,
    ) -> PitchAnalysisFrame {
        let mut quality_flags = QualityFlags::default();

        if samples.len() < self.window_size {
            quality_flags.warmup = true;
            return PitchAnalysisFrame {
                session_id: session_id.to_string(),
                epoch_id,
                capture_stream_id,
                sequence,
                window_start_frame,
                window_end_frame_exclusive,
                analysis_rate: self.sample_rate,
                reference_qpc,
                generated_qpc,
                f0_hz: None,
                periodicity: 0.0,
                voicing: Voicing::Unknown,
                quality_flags,
                algorithm_version: "yin-1.0.0".to_string(),
            };
        }

        // 1. RMS & Clipping check (§8.2)
        let mut sum_sq = 0.0f32;
        let mut clip_count = 0;
        for &s in &samples[..self.window_size] {
            sum_sq += s * s;
            if s.abs() >= 0.999 {
                clip_count += 1;
            }
        }
        let rms = (sum_sq / self.window_size as f32).sqrt();
        if clip_count > 0 {
            quality_flags.clipping = true;
        }

        // Low RMS -> Silence
        if rms < 0.005 {
            return PitchAnalysisFrame {
                session_id: session_id.to_string(),
                epoch_id,
                capture_stream_id,
                sequence,
                window_start_frame,
                window_end_frame_exclusive,
                analysis_rate: self.sample_rate,
                reference_qpc,
                generated_qpc,
                f0_hz: None,
                periodicity: 0.0,
                voicing: Voicing::Silence,
                quality_flags,
                algorithm_version: "yin-1.0.0".to_string(),
            };
        }

        // 2. Difference function d(tau)
        self.d_buffer.fill(0.0);
        let half_win = self.window_size / 2;
        for tau in self.min_lag..=self.max_lag {
            let mut diff_sum = 0.0f32;
            for j in 0..half_win {
                let diff = samples[j] - samples[j + tau];
                diff_sum += diff * diff;
            }
            self.d_buffer[tau] = diff_sum;
        }

        // 3. Cumulative Mean Normalized Difference (CMND)
        self.cmnd_buffer[0] = 1.0;
        let mut running_sum = 0.0f32;
        for tau in 1..=self.max_lag {
            running_sum += self.d_buffer[tau];
            if running_sum > 0.0 {
                self.cmnd_buffer[tau] = self.d_buffer[tau] * (tau as f32) / running_sum;
            } else {
                self.cmnd_buffer[tau] = 1.0;
            }
        }

        // 4. Search for first minimum below threshold
        let mut tau_found = 0usize;
        let mut min_val = f32::MAX;

        for tau in self.min_lag..=self.max_lag {
            let val = self.cmnd_buffer[tau];
            if val < self.threshold {
                // Find local minimum
                let mut search_tau = tau;
                while search_tau < self.max_lag
                    && self.cmnd_buffer[search_tau + 1] < self.cmnd_buffer[search_tau]
                {
                    search_tau += 1;
                }
                tau_found = search_tau;
                min_val = self.cmnd_buffer[search_tau];
                break;
            }
        }

        // If no minimum under threshold, take global minimum
        if tau_found == 0 {
            for tau in self.min_lag..=self.max_lag {
                if self.cmnd_buffer[tau] < min_val {
                    min_val = self.cmnd_buffer[tau];
                    tau_found = tau;
                }
            }
            if min_val > 0.35 {
                // Low confidence / Unvoiced
                quality_flags.low_confidence = true;
                return PitchAnalysisFrame {
                    session_id: session_id.to_string(),
                    epoch_id,
                    capture_stream_id,
                    sequence,
                    window_start_frame,
                    window_end_frame_exclusive,
                    analysis_rate: self.sample_rate,
                    reference_qpc,
                    generated_qpc,
                    f0_hz: None,
                    periodicity: (1.0 - min_val).max(0.0),
                    voicing: Voicing::Unvoiced,
                    quality_flags,
                    algorithm_version: "yin-1.0.0".to_string(),
                };
            }
        }

        // 5. Parabolic Interpolation around tau_found
        let final_tau = if tau_found > self.min_lag && tau_found < self.max_lag {
            let s0 = self.cmnd_buffer[tau_found - 1];
            let s1 = self.cmnd_buffer[tau_found];
            let s2 = self.cmnd_buffer[tau_found + 1];
            let denom = 2.0 * (2.0 * s1 - s0 - s2);
            if denom.abs() > 1e-6 {
                tau_found as f32 + (s2 - s0) / denom
            } else {
                tau_found as f32
            }
        } else {
            tau_found as f32
        };

        let f0 = self.sample_rate as f32 / final_tau;
        let periodicity = (1.0 - min_val).clamp(0.0, 1.0);

        // Quality flags check
        if periodicity < 0.60 {
            quality_flags.low_confidence = true;
        }
        if f0 < 80.0 || f0 > 1000.0 {
            quality_flags.out_of_range = true;
        }

        let voicing = if periodicity >= 0.50 {
            Voicing::Voiced
        } else {
            Voicing::Unvoiced
        };

        PitchAnalysisFrame {
            session_id: session_id.to_string(),
            epoch_id,
            capture_stream_id,
            sequence,
            window_start_frame,
            window_end_frame_exclusive,
            analysis_rate: self.sample_rate,
            reference_qpc,
            generated_qpc,
            f0_hz: if voicing == Voicing::Voiced {
                Some(f0)
            } else {
                None
            },
            periodicity,
            voicing,
            quality_flags,
            algorithm_version: "yin-1.0.0".to_string(),
        }
    }
}
