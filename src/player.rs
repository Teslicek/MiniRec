use std::path::Path;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{BSTR, Error, Interface, Result};
use windows_core::implement;

pub const WM_PLAYER_EVENT: u32 = WM_APP + 30;

#[implement(IMFMediaEngineNotify)]
struct Notify {
    window: isize,
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, _param1: usize, param2: u32) -> windows_core::Result<()> {
        unsafe { PostMessageW(Some(HWND(self.window as *mut _)), WM_PLAYER_EVENT, WPARAM(event as usize), LPARAM(param2 as i32 as isize)) }
    }
}

pub enum PlayerEvent {
    Loaded,
    Playing,
    Stopped,
    Failed(Error),
    Other,
}

pub struct Player {
    engine: IMFMediaEngine,
    extra:  IMFMediaEngineEx,
}

impl Player {
    pub fn new(video: HWND, events: HWND) -> Result<Self> {
        unsafe {
            let factory: IMFMediaEngineClassFactory = CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
            let mut attributes = None;

            MFCreateAttributes(&mut attributes, 2)?;

            let attributes = attributes.ok_or_else(|| Error::new(windows::Win32::Foundation::E_FAIL, "MFCreateAttributes returned no attributes"))?;
            let notify: IMFMediaEngineNotify = Notify { window: events.0 as isize }.into();

            attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
            attributes.SetUINT64(&MF_MEDIA_ENGINE_PLAYBACK_HWND, video.0 as u64)?;

            let engine = factory.CreateInstance(0, &attributes)?;
            let extra = engine.cast()?;

            Ok(Self { engine, extra })
        }
    }

    pub fn open(&self, path: &Path) -> Result<()> {
        unsafe { self.engine.SetSource(&BSTR::from(path.to_string_lossy().as_ref())) }
    }

    pub fn play(&self) -> Result<()> {
        unsafe { self.engine.Play() }
    }

    pub fn pause(&self) -> Result<()> {
        unsafe { self.engine.Pause() }
    }

    pub fn time(&self) -> f64 {
        unsafe { self.engine.GetCurrentTime() }
    }

    pub fn duration(&self) -> f64 {
        unsafe { self.engine.GetDuration() }
    }

    pub fn seek(&self, seconds: f64, exact: bool) -> Result<()> {
        let mode = if exact { MF_MEDIA_ENGINE_SEEK_MODE_NORMAL } else { MF_MEDIA_ENGINE_SEEK_MODE_APPROXIMATE };

        unsafe { self.extra.SetCurrentTimeEx(seconds, mode) }
    }

    pub fn step(&self, forward: bool) -> Result<()> {
        unsafe { self.extra.FrameStep(forward) }
    }

    pub fn shutdown(self) -> Result<()> {
        unsafe { self.engine.Shutdown() }
    }
}

pub fn decode_event(wparam: WPARAM, lparam: LPARAM) -> PlayerEvent {
    let event = MF_MEDIA_ENGINE_EVENT(wparam.0 as i32);

    match event {
        MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA => PlayerEvent::Loaded,
        MF_MEDIA_ENGINE_EVENT_PLAYING => PlayerEvent::Playing,
        MF_MEDIA_ENGINE_EVENT_PAUSE | MF_MEDIA_ENGINE_EVENT_ENDED => PlayerEvent::Stopped,
        MF_MEDIA_ENGINE_EVENT_ERROR => PlayerEvent::Failed(Error::new(windows::core::HRESULT(lparam.0 as i32), "This video cannot be played. Only MP4/MOV files with H.264 or HEVC video are supported.")),
        _ => PlayerEvent::Other,
    }
}
