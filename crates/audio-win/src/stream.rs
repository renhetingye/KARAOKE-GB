use crate::{device::AudioDeviceError, is_float32_format};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use thiserror::Error;
use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::*;

#[derive(Debug, Error)]
pub enum StreamError {
    #[error("Windows error: {0}")]
    Windows(#[from] windows::core::Error),
    #[error("Device error: {0}")]
    Device(#[from] AudioDeviceError),
    #[error("Failed to create event: {0}")]
    EventCreation(String),
    #[error("Unsupported audio format")]
    UnsupportedFormat,
}

pub struct WasapiCaptureStream {
    running: Arc<AtomicBool>,
    thread_handle: Option<JoinHandle<()>>,
}

impl WasapiCaptureStream {
    /// Starts an event-driven WASAPI capture stream on the default or specified capture device (§4, §6, §7)
    pub fn start<F>(device_id: Option<&str>, mut on_data: F) -> Result<Self, StreamError>
    where
        F: FnMut(&[f32], u64, i64, bool, u32) + Send + 'static,
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
                    match enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) {
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
                // Keep shared-mode capture responsive enough for vocal monitoring.
                // Windows may round this up to the endpoint's supported engine period.
                let buffer_duration_hns = 100_000; // 10 ms in 100 ns units

                if let Err(e) = audio_client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    stream_flags,
                    buffer_duration_hns,
                    0,
                    mix_format,
                    None,
                ) {
                    eprintln!("IAudioClient::Initialize failed: {:?}", e);
                    let _ = CloseHandle(event_handle);
                    let _ = init_tx.send(Err(StreamError::Windows(e)));
                    return;
                }

                if let Err(e) = audio_client.SetEventHandle(event_handle) {
                    eprintln!("SetEventHandle failed: {:?}", e);
                    let _ = CloseHandle(event_handle);
                    let _ = init_tx.send(Err(StreamError::Windows(e)));
                    return;
                }

                let capture_client: IAudioCaptureClient = match audio_client.GetService() {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("GetService<IAudioCaptureClient> failed: {:?}", e);
                        let _ = CloseHandle(event_handle);
                        let _ = init_tx.send(Err(StreamError::Windows(e)));
                        return;
                    }
                };

                if let Err(e) = audio_client.Start() {
                    eprintln!("IAudioClient::Start failed: {:?}", e);
                    let _ = CloseHandle(event_handle);
                    let _ = init_tx.send(Err(StreamError::Windows(e)));
                    return;
                }

                // Notify main thread that stream started successfully!
                let _ = init_tx.send(Ok(()));

                let mut sample_buffer = Vec::with_capacity(4096);

                while running_clone.load(Ordering::Relaxed) {
                    let wait_res = WaitForSingleObject(event_handle, 50);
                    if wait_res == WAIT_OBJECT_0 {
                        while let Ok(next_packet_size) = capture_client.GetNextPacketSize() {
                            if next_packet_size == 0 {
                                break;
                            }

                            let mut buffer_ptr: *mut u8 = std::ptr::null_mut();
                            let mut num_frames_read = 0u32;
                            let mut flags = 0u32;
                            let mut device_position = 0u64;
                            let mut qpc_position = 0u64;

                            let hr = capture_client.GetBuffer(
                                &mut buffer_ptr,
                                &mut num_frames_read,
                                &mut flags,
                                Some(&mut device_position),
                                Some(&mut qpc_position),
                            );

                            if hr.is_ok() && num_frames_read > 0 && !buffer_ptr.is_null() {
                                let has_discontinuity =
                                    (flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32) != 0;
                                let is_silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;

                                sample_buffer.clear();
                                if is_silent {
                                    sample_buffer.resize(num_frames_read as usize, 0.0);
                                } else {
                                    // Extract mono float32 channel
                                    let float_ptr = buffer_ptr as *const f32;
                                    for f in 0..num_frames_read as usize {
                                        let sample = *float_ptr.add(f * channels);
                                        sample_buffer.push(sample);
                                    }
                                }

                                on_data(
                                    &sample_buffer,
                                    device_position,
                                    qpc_position as i64,
                                    has_discontinuity,
                                    sample_rate,
                                );

                                let _ = capture_client.ReleaseBuffer(num_frames_read);
                            } else {
                                break;
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

        // Wait for thread initialization result synchronously
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
                    "Thread terminated without sending init result".to_string(),
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

impl Drop for WasapiCaptureStream {
    fn drop(&mut self) {
        self.stop();
    }
}
