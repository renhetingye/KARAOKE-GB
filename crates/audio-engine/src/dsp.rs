use serde::{Deserialize, Serialize};

const MAX_SAMPLE_RATE: usize = 192_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorConfig {
    pub enabled: bool,
    pub monitor_gain_db: f32,
    pub reverb_mix: f32,
    pub echo_mix: f32,
    pub echo_delay_ms: f32,
    pub eq_low_db: f32,
    pub eq_mid_db: f32,
    pub eq_high_db: f32,
    pub compressor_threshold_db: f32,
    pub compressor_ratio: f32,
    pub noise_gate_threshold_db: f32,
    pub limiter_ceiling_db: f32,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            monitor_gain_db: -9.0,
            reverb_mix: 0.0,
            echo_mix: 0.0,
            echo_delay_ms: 180.0,
            eq_low_db: 0.0,
            eq_mid_db: 0.0,
            eq_high_db: 0.0,
            compressor_threshold_db: -18.0,
            compressor_ratio: 1.0,
            noise_gate_threshold_db: -80.0,
            limiter_ceiling_db: -1.0,
        }
    }
}

impl MonitorConfig {
    pub fn sanitized(mut self) -> Self {
        self.monitor_gain_db = self.monitor_gain_db.clamp(-60.0, 0.0);
        self.reverb_mix = self.reverb_mix.clamp(0.0, 0.6);
        self.echo_mix = self.echo_mix.clamp(0.0, 0.5);
        self.echo_delay_ms = self.echo_delay_ms.clamp(40.0, 500.0);
        self.eq_low_db = self.eq_low_db.clamp(-12.0, 12.0);
        self.eq_mid_db = self.eq_mid_db.clamp(-12.0, 12.0);
        self.eq_high_db = self.eq_high_db.clamp(-12.0, 12.0);
        self.compressor_threshold_db = self.compressor_threshold_db.clamp(-40.0, 0.0);
        self.compressor_ratio = self.compressor_ratio.clamp(1.0, 20.0);
        self.noise_gate_threshold_db = self.noise_gate_threshold_db.clamp(-80.0, -20.0);
        self.limiter_ceiling_db = self.limiter_ceiling_db.clamp(-12.0, 0.0);
        self
    }
}

struct DelayLine {
    buffer: Vec<f32>,
    index: usize,
    delay_samples: usize,
    feedback: f32,
}

impl DelayLine {
    fn new(max_samples: usize, feedback: f32) -> Self {
        Self {
            buffer: vec![0.0; max_samples.max(2)],
            index: 0,
            delay_samples: 1,
            feedback,
        }
    }

    fn set_delay(&mut self, samples: usize) {
        self.delay_samples = samples.clamp(1, self.buffer.len());
        self.index %= self.delay_samples;
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.buffer[self.index];
        self.buffer[self.index] = input + delayed * self.feedback;
        self.index += 1;
        if self.index >= self.delay_samples {
            self.index = 0;
        }
        delayed
    }
}

pub struct RealtimeVocalDsp {
    config: MonitorConfig,
    sample_rate: u32,
    low_state: f32,
    high_cut_state: f32,
    gate_envelope: f32,
    gate_gain: f32,
    compressor_envelope: f32,
    compressor_gain: f32,
    echo: DelayLine,
    reverbs: [DelayLine; 4],
}

impl RealtimeVocalDsp {
    pub fn new(config: MonitorConfig) -> Self {
        let max_delay = MAX_SAMPLE_RATE / 2 + 1;
        let mut dsp = Self {
            config: config.sanitized(),
            sample_rate: 48_000,
            low_state: 0.0,
            high_cut_state: 0.0,
            gate_envelope: 0.0,
            gate_gain: 0.0,
            compressor_envelope: 0.0,
            compressor_gain: 1.0,
            echo: DelayLine::new(max_delay, 0.35),
            reverbs: [
                DelayLine::new(max_delay, 0.72),
                DelayLine::new(max_delay, 0.69),
                DelayLine::new(max_delay, 0.66),
                DelayLine::new(max_delay, 0.63),
            ],
        };
        dsp.set_sample_rate(48_000);
        dsp
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        let sample_rate = sample_rate.clamp(8_000, MAX_SAMPLE_RATE as u32);
        if self.sample_rate == sample_rate && self.echo.delay_samples > 1 {
            return;
        }
        self.sample_rate = sample_rate;
        self.echo
            .set_delay((self.config.echo_delay_ms * sample_rate as f32 / 1000.0).round() as usize);
        for (line, delay_ms) in self.reverbs.iter_mut().zip([29.7_f32, 37.1, 41.1, 43.7]) {
            line.set_delay((delay_ms * sample_rate as f32 / 1000.0).round() as usize);
        }
    }

    pub fn process_sample(&mut self, input: f32) -> f32 {
        if !self.config.enabled {
            return 0.0;
        }

        let absolute = input.abs();
        self.gate_envelope =
            envelope_follow(self.gate_envelope, absolute, self.sample_rate, 2.0, 80.0);
        let gate_threshold = db_to_gain(self.config.noise_gate_threshold_db);
        let gate_target = if self.gate_envelope >= gate_threshold {
            1.0
        } else {
            0.0
        };
        self.gate_gain = smooth_toward(
            self.gate_gain,
            gate_target,
            self.sample_rate,
            if gate_target > self.gate_gain {
                5.0
            } else {
                120.0
            },
        );
        let gated = input * self.gate_gain;

        let low_alpha = one_pole_alpha(220.0, self.sample_rate);
        let high_cut_alpha = one_pole_alpha(4_000.0, self.sample_rate);
        self.low_state += low_alpha * (gated - self.low_state);
        self.high_cut_state += high_cut_alpha * (gated - self.high_cut_state);
        let low = self.low_state;
        let mid = self.high_cut_state - self.low_state;
        let high = gated - self.high_cut_state;
        let equalized = low * db_to_gain(self.config.eq_low_db)
            + mid * db_to_gain(self.config.eq_mid_db)
            + high * db_to_gain(self.config.eq_high_db);

        self.compressor_envelope = envelope_follow(
            self.compressor_envelope,
            equalized.abs(),
            self.sample_rate,
            8.0,
            100.0,
        );
        let threshold = db_to_gain(self.config.compressor_threshold_db);
        let desired_gain = if self.compressor_envelope > threshold {
            let over_db = gain_to_db(self.compressor_envelope / threshold);
            db_to_gain(-over_db * (1.0 - 1.0 / self.config.compressor_ratio))
        } else {
            1.0
        };
        self.compressor_gain = smooth_toward(
            self.compressor_gain,
            desired_gain,
            self.sample_rate,
            if desired_gain < self.compressor_gain {
                8.0
            } else {
                120.0
            },
        );
        let compressed = equalized * self.compressor_gain;

        let echo_wet = self.echo.process(compressed);
        let mut reverb_wet = 0.0;
        for line in &mut self.reverbs {
            reverb_wet += line.process(compressed);
        }
        reverb_wet *= 0.25;
        let effected =
            compressed + echo_wet * self.config.echo_mix + reverb_wet * self.config.reverb_mix;
        let monitored = effected * db_to_gain(self.config.monitor_gain_db);
        limit_sample(monitored, self.config.limiter_ceiling_db)
    }

    pub fn limit_master(&self, sample: f32) -> f32 {
        limit_sample(sample, self.config.limiter_ceiling_db)
    }
}

fn one_pole_alpha(cutoff_hz: f32, sample_rate: u32) -> f32 {
    1.0 - (-2.0 * std::f32::consts::PI * cutoff_hz / sample_rate as f32).exp()
}

fn envelope_follow(
    current: f32,
    target: f32,
    sample_rate: u32,
    attack_ms: f32,
    release_ms: f32,
) -> f32 {
    smooth_toward(
        current,
        target,
        sample_rate,
        if target > current {
            attack_ms
        } else {
            release_ms
        },
    )
}

fn smooth_toward(current: f32, target: f32, sample_rate: u32, time_ms: f32) -> f32 {
    let coefficient = (-1.0 / (time_ms.max(0.1) * 0.001 * sample_rate as f32)).exp();
    target + coefficient * (current - target)
}

fn db_to_gain(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.max(1e-12).log10()
}

fn limit_sample(sample: f32, ceiling_db: f32) -> f32 {
    let ceiling = db_to_gain(ceiling_db);
    sample.clamp(-ceiling, ceiling)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_monitor_is_silent() {
        let mut dsp = RealtimeVocalDsp::new(MonitorConfig::default());
        assert_eq!(dsp.process_sample(0.8), 0.0);
    }

    #[test]
    fn limiter_never_exceeds_configured_ceiling() {
        let config = MonitorConfig {
            enabled: true,
            monitor_gain_db: 0.0,
            eq_low_db: 12.0,
            eq_mid_db: 12.0,
            eq_high_db: 12.0,
            limiter_ceiling_db: -3.0,
            ..MonitorConfig::default()
        };
        let mut dsp = RealtimeVocalDsp::new(config);
        for _ in 0..10_000 {
            assert!(dsp.process_sample(2.0).abs() <= db_to_gain(-3.0) + 1e-6);
        }
    }

    #[test]
    fn sanitizer_clamps_unsafe_values() {
        let config = MonitorConfig {
            enabled: true,
            monitor_gain_db: 12.0,
            reverb_mix: 5.0,
            echo_mix: 5.0,
            echo_delay_ms: 5_000.0,
            eq_low_db: 50.0,
            eq_mid_db: -50.0,
            eq_high_db: 50.0,
            compressor_threshold_db: -100.0,
            compressor_ratio: 100.0,
            noise_gate_threshold_db: 0.0,
            limiter_ceiling_db: 10.0,
        }
        .sanitized();
        assert_eq!(config.monitor_gain_db, 0.0);
        assert_eq!(config.reverb_mix, 0.6);
        assert_eq!(config.limiter_ceiling_db, 0.0);
    }
}
