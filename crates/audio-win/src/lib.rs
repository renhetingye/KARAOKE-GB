pub mod device;
pub mod render_stream;
pub mod stream;

pub use device::{AudioDeviceError, AudioDeviceInfo, DeviceManager};
pub use render_stream::WasapiRenderStream;
pub use stream::{StreamError, WasapiCaptureStream};

use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE};
use windows::Win32::System::Performance::*;

pub(crate) unsafe fn is_float32_format(format: *const WAVEFORMATEX) -> bool {
    if format.is_null() || (*format).wBitsPerSample != 32 {
        return false;
    }
    match (*format).wFormatTag {
        3 => true, // WAVE_FORMAT_IEEE_FLOAT
        0xfffe if (*format).cbSize as usize >= 22 => {
            let extensible = format as *const WAVEFORMATEXTENSIBLE;
            // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
            std::ptr::addr_of!((*extensible).SubFormat).read_unaligned()
                == windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71)
        }
        _ => false,
    }
}

/// Get system QPC frequency (§6.1)
pub fn get_qpc_frequency() -> i64 {
    unsafe {
        let mut freq = 0i64;
        let _ = QueryPerformanceFrequency(&mut freq);
        freq
    }
}

/// Get current QPC counter tick
pub fn get_qpc_counter() -> i64 {
    unsafe {
        let mut counter = 0i64;
        let _ = QueryPerformanceCounter(&mut counter);
        counter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qpc_frequency_is_positive() {
        let freq = get_qpc_frequency();
        assert!(freq > 0, "QPC frequency must be strictly positive");
        println!(
            "System QPC Frequency: {} Hz (tick resolution: {:.2} ns)",
            freq,
            1_000_000_000.0 / freq as f64
        );
    }

    #[test]
    fn test_list_audio_devices() {
        let devices = DeviceManager::list_devices();
        match devices {
            Ok(devs) => {
                println!("Found {} active audio endpoints:", devs.len());
                for d in &devs {
                    println!("  [{}] {}", d.data_flow, d.id);
                }
            }
            Err(e) => {
                println!("Audio device enumeration skipped or failed: {}", e);
            }
        }
    }
}
