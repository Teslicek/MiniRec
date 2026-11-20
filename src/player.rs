use std::path::Path;

use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{BSTR, Error, Interface, Result};
use windows_core::implement;

use crate::encoder::device_manager;
use crate::export::hardware_device;

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
    engine:      IMFMediaEngine,
    extra:       IMFMediaEngineEx,
    composition: IDCompositionDevice,
    visual:      IDCompositionVisual,
    _target:     IDCompositionTarget,
}

impl Player {
    pub fn new(video: HWND, events: HWND) -> Result<Self> {
        unsafe {
            let factory: IMFMediaEngineClassFactory = CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
            let mut attributes = None;

            MFCreateAttributes(&mut attributes, 3)?;

            let attributes = attributes.ok_or_else(|| Error::new(E_FAIL, "MFCreateAttributes returned no attributes"))?;
            let notify: IMFMediaEngineNotify = Notify { window: events.0 as isize }.into();
            let device = hardware_device()?;

            attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
            attributes.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &device_manager(&device)?)?;
            attributes.SetUINT32(&MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32)?;

            let engine = factory.CreateInstance(0, &attributes)?;
            let extra: IMFMediaEngineEx = engine.cast()?;

            extra.EnableWindowlessSwapchainMode(true)?;

            let composition: IDCompositionDevice = DCompositionCreateDevice(&device.cast::<IDXGIDevice>()?)?;
            let target = composition.CreateTargetForHwnd(video, true)?;
            let visual = composition.CreateVisual()?;

            target.SetRoot(&visual)?;
            composition.Commit()?;
            Ok(Self { engine, extra, composition, visual, _target: target })
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

    pub fn attach(&self) -> Result<()> {
        unsafe {
            let surface = self.composition.CreateSurfaceFromHandle(self.extra.GetVideoSwapchainHandle()?)?;

            self.visual.SetContent(&surface)?;
            self.composition.Commit()
        }
    }

    pub fn set_volume(&self, volume: f64) -> Result<()> {
        unsafe { self.engine.SetVolume(volume) }
    }

    pub fn set_muted(&self, muted: bool) -> Result<()> {
        unsafe { self.engine.SetMuted(muted) }
    }

    pub fn set_rate(&self, rate: f64) -> Result<()> {
        unsafe {
            self.engine.SetDefaultPlaybackRate(rate)?;
            self.engine.SetPlaybackRate(rate)
        }
    }

    pub fn resize(&self, area: RECT) -> Result<()> {
        let target = RECT { left: 0, top: 0, right: area.right - area.left, bottom: area.bottom - area.top };

        unsafe { self.extra.UpdateVideoStream(None, Some(&target), None) }
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
