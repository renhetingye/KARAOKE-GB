use crate::dsp::{MonitorConfig, RealtimeVocalDsp};
use karaoke_audio_win::{WasapiCaptureStream, WasapiRenderStream};
use rtrb::RingBuffer;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use crate::session::SessionError;

/// A lightweight full-duplex session used to audition the microphone without
/// loading or playing a song. DSP state and buffers are prepared before the
/// real-time callbacks start.
pub struct MicMonitorSession {
    running: Arc<AtomicBool>,
    capture_stream: Option<WasapiCaptureStream>,
    render_stream: Option<WasapiRenderStream>,
}

impl MicMonitorSession {
    pub fn start(
        mic_device_id: Option<&str>,
        render_device_id: Option<&str>,
        mut config: MonitorConfig,
    ) -> Result<Self, SessionError> {
        config.enabled = true;
        let config = config.sanitized();
        let running = Arc::new(AtomicBool::new(true));
        let running_render = running.clone();
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(96_000);
        let capture_rate = Arc::new(AtomicU32::new(48_000));
        let render_capture_rate = capture_rate.clone();

        let mut dsp = RealtimeVocalDsp::new(config);
        let mut started = false;
        let mut current = 0.0f32;
        let mut next = 0.0f32;
        let mut phase = 0.0f64;

        let render_stream = WasapiRenderStream::start(
            render_device_id,
            move |buffer, frames, _clock, _qpc, output_rate, channels, _queued| {
                if !running_render.load(Ordering::Relaxed) {
                    buffer.fill(0.0);
                    return;
                }

                dsp.set_sample_rate(output_rate);
                let input_rate = render_capture_rate.load(Ordering::Relaxed).max(1);
                let target = (input_rate as usize / 100).max(80);
                if !started && consumer.slots() >= target {
                    current = consumer.pop().unwrap_or(0.0);
                    next = consumer.pop().unwrap_or(current);
                    phase = 0.0;
                    started = true;
                }

                for frame in 0..frames {
                    let sample = if started {
                        let interpolated = current + (next - current) * phase as f32;
                        let occupancy_error = consumer.slots() as f64 / target as f64 - 1.0;
                        let correction = (occupancy_error * 0.001).clamp(-0.005, 0.005);
                        phase += input_rate as f64 / output_rate.max(1) as f64 * (1.0 + correction);
                        while phase >= 1.0 {
                            phase -= 1.0;
                            current = next;
                            match consumer.pop() {
                                Ok(value) => next = value,
                                Err(_) => {
                                    started = false;
                                    current = 0.0;
                                    next = 0.0;
                                    break;
                                }
                            }
                        }
                        dsp.process_sample(interpolated)
                    } else {
                        0.0
                    };

                    for channel in 0..channels {
                        buffer[frame * channels + channel] = sample;
                    }
                }
            },
        )
        .map_err(|error| SessionError::Device(format!("Monitor render init failed: {error:?}")))?;

        let capture_stream = WasapiCaptureStream::start(
            mic_device_id,
            move |samples, _position, _qpc, _discontinuity, sample_rate| {
                capture_rate.store(sample_rate, Ordering::Relaxed);
                for &sample in samples {
                    if producer.push(sample).is_err() {
                        break;
                    }
                }
            },
        )
        .map_err(|error| SessionError::Device(format!("Monitor capture init failed: {error:?}")))?;

        Ok(Self {
            running,
            capture_stream: Some(capture_stream),
            render_stream: Some(render_stream),
        })
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(mut stream) = self.capture_stream.take() {
            stream.stop();
        }
        if let Some(mut stream) = self.render_stream.take() {
            stream.stop();
        }
    }
}

impl Drop for MicMonitorSession {
    fn drop(&mut self) {
        self.stop();
    }
}
