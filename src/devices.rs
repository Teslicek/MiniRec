use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{E_FAIL, ERROR_NOT_FOUND};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::core::{Error, HSTRING, Result};

#[derive(Clone, Copy, PartialEq)]
pub enum Flow {
    Output,
    Input,
}

impl Flow {
    fn data_flow(self) -> EDataFlow {
        match self {
            Flow::Output => eRender,
            Flow::Input => eCapture,
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            Flow::Output => "system audio device",
            Flow::Input => "microphone",
        }
    }
}

#[derive(Clone)]
pub struct AudioDevice {
    pub id:   String,
    pub name: String,
}

pub fn list(flow: Flow) -> Result<Vec<AudioDevice>> {
    unsafe {
        let collection = enumerator()?.EnumAudioEndpoints(flow.data_flow(), DEVICE_STATE_ACTIVE)?;

        (0..collection.GetCount()?).map(|index| describe(&collection.Item(index)?)).collect()
    }
}

pub fn default_device(flow: Flow) -> Result<Option<AudioDevice>> {
    unsafe {
        match enumerator()?.GetDefaultAudioEndpoint(flow.data_flow(), eConsole) {
            Ok(device) => Ok(Some(describe(&device)?)),
            Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => Ok(None),
            Err(e) => Err(e),
        }
    }
}

pub fn open(flow: Flow, id: Option<&str>) -> Result<Option<IMMDevice>> {
    unsafe {
        let enumerator = enumerator()?;
        let Some(id) = id else {
            return match enumerator.GetDefaultAudioEndpoint(flow.data_flow(), eConsole) {
                Ok(device) => Ok(Some(device)),
                Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => Ok(None),
                Err(e) => Err(e),
            };
        };
        let device = enumerator.GetDevice(&HSTRING::from(id)).map_err(|e| Error::new(e.code(), format!("The selected {} is no longer available. Pick another one in MiniRec.", flow.noun())))?;

        if device.GetState()? != DEVICE_STATE_ACTIVE {
            return Err(Error::new(E_FAIL, format!("The selected {} ({}) is not connected. Plug it in or pick another one.", flow.noun(), describe(&device)?.name)));
        }

        Ok(Some(device))
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

fn describe(device: &IMMDevice) -> Result<AudioDevice> {
    unsafe {
        let raw_id = device.GetId()?;
        let id = raw_id.to_string();

        CoTaskMemFree(Some(raw_id.0 as *const _));

        let mut value = device.OpenPropertyStore(STGM_READ)?.GetValue(&PKEY_Device_FriendlyName)?;
        let raw_name = PropVariantToStringAlloc(&value);

        PropVariantClear(&mut value)?;

        let raw_name = raw_name?;
        let name = raw_name.to_string();

        CoTaskMemFree(Some(raw_name.0 as *const _));
        Ok(AudioDevice {
            id: id.map_err(|e| Error::new(E_FAIL, format!("Invalid audio device id: {e}")))?,
            name: name.map_err(|e| Error::new(E_FAIL, format!("Invalid audio device name: {e}")))?,
        })
    }
}
