use crate::{is_float32_format, stream::StreamError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::*;

pub struct WasapiRenderStream {
    running: Arc<AtomicBool>,
    thread_handle: Option<JoinHandle<()>>,
}

impl WasapiRenderStream {
    /// Starts an event-driven WASAPI render (playback) stream on the default or specified render device (§4, §6, §7)
    ///
    /// Callback signature: `on_fill(&mut [f32], frames_needed, clock_pos,
    /// qpc_100ns, sample_rate, channels, queued_frames)`.
    pub fn start<F>(device_id: Option<&str>, mut on_fill: F) -> Result<Self, StreamError>
    where
        F: FnMut(&mut [f32], usize, u64, i64, u32, usize, usize) + Send + 'static,
    {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        // Normalize empty string or whitespace to None (default device)
        let dev_id_owned = device_id
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let (init_tx, init_rx) = std::sync::mpsc::channel();

        let thread_handle = thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

                let enumerator: IMMDeviceEnumerator =
                    match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                        Ok(e) => e,
                        Err(e) => {
                            let _ = init_tx.send(Err(StreamError::Windows(e)));
                            return;
                        }
                    };

                let device: IMMDevice = if let Some(ref id) = dev_id_owned {
                    let hstr: windows::core::HSTRING = id.as_str().into();
                    match enumerator.GetDevice(&hstr) {
                        Ok(d) => d,
                        Err(e) => {
                            let _ = init_tx.send(Err(StreamError::Windows(e)));
                            return;
                        }
                    }
                } else {
                    match enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                        Ok(d) => d,
                        Err(e) => {
                            let _ = init_tx.send(Err(StreamError::Windows(e)));
                            return;
                        }
                    }
                };

                let audio_client: IAudioClient = match device.Activate(CLSCTX_ALL, None) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = init_tx.send(Err(StreamError::Windows(e)));
                        return;
                    }
                };

                let mix_format = match audio_client.GetMixFormat() {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("GetMixFormat failed: {:?}", e);
                        return;
                    }
                };

                let channels = (*mix_format).nChannels as usize;
                let sample_rate = (*mix_format).nSamplesPerSec;
                if !is_float32_format(mix_format) {
                    CoTaskMemFree(Some(mix_format.cast()));
                    let _ = init_tx.send(Err(StreamError::UnsupportedFormat));
                    return;
                }

                let event_handle = match CreateEventW(None, false, false, None) {
                    Ok(h) => h,
                    Err(e) => {
                        eprintln!("CreateEventW failed: {:?}", e);
                        return;
                    }
                };

                let stream_flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
                // Match capture's low-latency request. The endpoint may round this
                // up when its shared-mode engine period is longer than 10 ms.
                let buffer_duration_hns = 100_000; // 10 ms in 100 ns units

                if let Err(e) = audio_client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    stream_flags,
                    buffer_duration_hns,
                    0,
                    mix_format,
                    None,
                ) {
                    eprintln!("IAudioClient::Initialize (render) failed: {:?}", e);
                    let _ = CloseHandle(event_handle);
                    let _ = init_tx.send(Err(StreamError::Windows(e)));
                    return;
                }

                if let Err(e) = audio_client.SetEventHandle(event_handle) {
                    eprintln!("SetEventHandle (render) failed: {:?}", e);
                    let _ = CloseHandle(event_handle);
                    let _ = init_tx.send(Err(StreamError::Windows(e)));
                    return;
                }

                let buffer_frame_count = match audio_client.GetBufferSize() {
                    Ok(count) => count as usize,
                    Err(e) => {
                        eprintln!("GetBufferSize failed: {:?}", e);
                        let _ = CloseHandle(event_handle);
                        let _ = init_tx.send(Err(StreamError::Windows(e)));
                        return;
                    }
                };

                let render_client: IAudioRenderClient = match audio_client.GetService() {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("GetService<IAudioRenderClient> failed: {:?}", e);
                        let _ = CloseHandle(event_handle);
                        let _ = init_tx.send(Err(StreamError::Windows(e)));
                        return;
                    }
                };

                let audio_clock: IAudioClock = match audio_client.GetService() {
                    Ok(clock) => clock,
                    Err(error) => {
                        let _ = CloseHandle(event_handle);
                        CoTaskMemFree(Some(mix_format.cast()));
                        let _ = init_tx.send(Err(StreamError::Windows(error)));
                        return;
                    }
                };
                let audio_clock_frequency = match audio_clock.GetFrequency() {
                    Ok(frequency) if frequency > 0 => frequency,
                    Ok(_) => {
                        let _ = CloseHandle(event_handle);
                        CoTaskMemFree(Some(mix_format.cast()));
                        let _ = init_tx.send(Err(StreamError::UnsupportedFormat));
                        return;
                    }
                    Err(error) => {
                        let _ = CloseHandle(event_handle);
                        CoTaskMemFree(Some(mix_format.cast()));
                        let _ = init_tx.send(Err(StreamError::Windows(error)));
                        return;
                    }
                };

                // Pre-roll: Fill initial buffer with silence before start to avoid underflow
                if let Ok(p_data) = render_client.GetBuffer(buffer_frame_count as u32) {
                    std::ptr::write_bytes(
                        p_data,
                        0,
                        buffer_frame_count * channels * std::mem::size_of::<f32>(),
                    );
                    let _ = render_client.ReleaseBuffer(buffer_frame_count as u32, 0);
                }

                if let Err(e) = audio_client.Start() {
                    eprintln!("IAudioClient::Start (render) failed: {:?}", e);
                    let _ = CloseHandle(event_handle);
                    let _ = init_tx.send(Err(StreamError::Windows(e)));
                    return;
                }

                // Notify main thread of successful startup
                let _ = init_tx.send(Ok(()));

                let mut sample_buffer = Vec::with_capacity(buffer_frame_count * channels);

                while running_clone.load(Ordering::Relaxed) {
                    let wait_res = WaitForSingleObject(event_handle, 50);
                    if wait_res == WAIT_OBJECT_0 {
                        let padding = match audio_client.GetCurrentPadding() {
                            Ok(p) => p as usize,
                            Err(_) => continue,
                        };

                        if buffer_frame_count > padding {
                            let frames_needed = buffer_frame_count - padding;
                            if frames_needed == 0 {
                                continue;
                            }

                            let mut clock_pos = 0u64;
                            let mut qpc_pos = 0u64;
                            if let Err(error) =
                                audio_clock.GetPosition(&mut clock_pos, Some(&mut qpc_pos))
                            {
                                eprintln!("IAudioClock::GetPosition failed: {error:?}");
                                continue;
                            }
                            // IAudioClock position units are defined by
                            // GetFrequency(), not guaranteed to be PCM frames.
                            // Normalize before exposing it to timeline/drift code.
                            let clock_frame_pos = ((clock_pos as u128 * sample_rate as u128)
                                / audio_clock_frequency as u128)
                                as u64;

                            sample_buffer.clear();
                            sample_buffer.resize(frames_needed * channels, 0.0);

                            on_fill(
                                &mut sample_buffer,
                                frames_needed,
                                clock_frame_pos,
                                qpc_pos as i64,
                                sample_rate,
                                channels,
                                padding,
                            );

                            if let Ok(p_data) = render_client.GetBuffer(frames_needed as u32) {
                                let float_dest = p_data as *mut f32;
                                std::ptr::copy_nonoverlapping(
                                    sample_buffer.as_ptr(),
                                    float_dest,
                                    frames_needed * channels,
                                );
                                let _ = render_client.ReleaseBuffer(frames_needed as u32, 0);
                            }
                        }
                    }
                }

                let _ = audio_client.Stop();
                let _ = CloseHandle(event_handle);
                CoTaskMemFree(Some(mix_format.cast()));
                CoUninitialize();
            }
        });

        match init_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                running,
                thread_handle: Some(thread_handle),
            }),
            Ok(Err(e)) => {
                running.store(false, Ordering::Relaxed);
                let _ = thread_handle.join();
                Err(e)
            }
            Err(_) => {
                running.store(false, Ordering::Relaxed);
                let _ = thread_handle.join();
                Err(StreamError::EventCreation(
                    "Render thread terminated without sending init result".to_string(),
                ))
            }
        }
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.thread_handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for WasapiRenderStream {
    fn drop(&mut self) {
        self.stop();
    }
}
