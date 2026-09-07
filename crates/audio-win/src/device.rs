use thiserror::Error;
use windows::core::Result as WinResult;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::*;
use windows::Win32::System::Com::*;

const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

#[derive(Debug, Error)]
pub enum AudioDeviceError {
    #[error("Windows API error: {0}")]
    Windows(#[from] windows::core::Error),
    #[error("COM initialization failed: {0}")]
    ComInit(String),
    #[error("Device not found: {0}")]
    DeviceNotFound(String),
    #[error("Device format not supported: {0}")]
    UnsupportedFormat(String),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioDeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub data_flow: String, // "Render" or "Capture"
}

pub struct DeviceManager;

impl DeviceManager {
    /// Lists all active audio devices (Render and Capture) with friendly names (§4, §6)
    pub fn list_devices() -> Result<Vec<AudioDeviceInfo>, AudioDeviceError> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

            let mut result = Vec::new();

            // Default devices
            let default_render_id = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .and_then(|d| d.GetId())
                .map(|p| {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.as_ptr() as *const _));
                    s
                })
                .unwrap_or_default();

            let default_capture_id = enumerator
                .GetDefaultAudioEndpoint(eCapture, eConsole)
                .and_then(|d| d.GetId())
                .map(|p| {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.as_ptr() as *const _));
                    s
                })
                .unwrap_or_default();

            // Enumerate Render (Outputs)
            if let Ok(render_collection) =
                enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            {
                let count = render_collection.GetCount().unwrap_or(0);
                for i in 0..count {
                    if let Ok(device) = render_collection.Item(i) {
                        if let Ok(info) =
                            Self::get_device_info(&device, "Render", &default_render_id)
                        {
                            result.push(info);
                        }
                    }
                }
            }

            // Enumerate Capture (Inputs)
            if let Ok(capture_collection) =
                enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            {
                let count = capture_collection.GetCount().unwrap_or(0);
                for i in 0..count {
                    if let Ok(device) = capture_collection.Item(i) {
                        if let Ok(info) =
                            Self::get_device_info(&device, "Capture", &default_capture_id)
                        {
                            result.push(info);
                        }
                    }
                }
            }

            Ok(result)
        }
    }

    unsafe fn get_device_info(
        device: &IMMDevice,
        data_flow: &str,
        default_id: &str,
    ) -> WinResult<AudioDeviceInfo> {
        let id_pwstr = device.GetId()?;
        let id = id_pwstr.to_string()?;
        CoTaskMemFree(Some(id_pwstr.as_ptr() as *const _));

        let is_default = !default_id.is_empty() && id == default_id;

        // Extract Friendly Name from Property Store
        let mut name = String::new();
        if let Ok(store) = device.OpenPropertyStore(STGM_READ) {
            if let Ok(mut prop_var) = store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME) {
                let pwsz = prop_var.Anonymous.Anonymous.Anonymous.pwszVal;
                if !pwsz.is_null() {
                    name = pwsz.to_string().unwrap_or_default();
                }
                let _ = PropVariantClear(&mut prop_var);
            }
        }
        if name.is_empty() {
            name = id.clone();
        }

        Ok(AudioDeviceInfo {
            id,
            name,
            is_default,
            data_flow: data_flow.to_string(),
        })
    }
}
