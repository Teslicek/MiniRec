use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;

use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, E_FAIL, HANDLE, HMODULE, HWND, POINT, RECT, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{ClientToScreen, DEVMODEW, DISPLAY_DEVICEW, ENUM_CURRENT_SETTINGS, EnumDisplayDevicesW, EnumDisplaySettingsW};
use windows::Win32::System::Threading::*;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{EDD_GET_DEVICE_INTERFACE_NAME, GetClientRect, GetWindowRect, IsIconic, IsWindow};
use windows::core::{Error, HSTRING, Interface, Result};

use crate::cursor::CursorOverlay;
use crate::encoder::{Frame, FramePool};
use crate::programs::Program;
use crate::recorder::{Clock, in_mta};

pub const PREVIEW_FPS: u32 = 30;
const FRAME_BUFFERS: i32 = 2;
const STARVED_SECONDS: i64 = 2;

#[derive(Clone)]
pub struct MonitorInfo {
    pub adapter: u32,
    pub output:  u32,
    pub id:      String,
    pub name:    String,
    pub width:   u32,
    pub height:  u32,
    pub refresh: u32,
    pub primary: bool,
    pub bounds:  RECT,
}

impl MonitorInfo {
    pub fn label(&self) -> String {
        format!("{} · {}×{} · {} Hz", self.name, self.width, self.height, self.refresh)
    }
}

pub fn monitors() -> Result<Vec<MonitorInfo>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut list = Vec::new();

    for adapter_index in 0.. {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e),
        };

        for output_index in 0.. {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e),
            };
            let desc = unsafe { output.GetDesc()? };

            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }

            let rect = desc.DesktopCoordinates;
            let device = String::from_utf16_lossy(&desc.DeviceName).trim_end_matches('\0').to_string();
            let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };

            if !unsafe { EnumDisplaySettingsW(&HSTRING::from(&device), ENUM_CURRENT_SETTINGS, &mut mode) }.as_bool() {
                return Err(Error::new(E_FAIL, format!("Cannot read the refresh rate of {device}")));
            }

            let mut display = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };

            if !unsafe { EnumDisplayDevicesW(&HSTRING::from(&device), 0, &mut display, EDD_GET_DEVICE_INTERFACE_NAME) }.as_bool() {
                return Err(Error::new(E_FAIL, format!("Cannot identify the monitor on {device}")));
            }

            let id = String::from_utf16_lossy(&display.DeviceID).trim_end_matches('\0').to_string();

            if id.is_empty() {
                return Err(Error::new(E_FAIL, format!("The monitor on {device} has no device ID")));
            }

            list.push(MonitorInfo {
                adapter: adapter_index,
                output: output_index,
                id,
                name: device.trim_start_matches(r"\\.\").to_string(),
                width: (rect.right - rect.left) as u32,
                height: (rect.bottom - rect.top) as u32,
                refresh: mode.dmDisplayFrequency,
                primary: rect.left == 0 && rect.top == 0,
                bounds: rect,
            });
        }
    }

    if list.is_empty() {
        return Err(Error::new(E_FAIL, "No monitors attached to the desktop were found"));
    }

    Ok(list)
}

pub enum Source {
    Monitor(MonitorInfo),
    Program(Program),
}

enum Backend {
    Monitor {
        output:      IDXGIOutput1,
        duplication: Option<IDXGIOutputDuplication>,
        cursor:      Option<CursorOverlay>,
        resize:      Option<Resize>,
        desktop:     (u32, u32),
    },
    Program {
        window:  HWND,
        capture: WindowCapture,
        content: SizeInt32,
        scaler:  Scaler,
    },
}

pub struct VideoCapture {
    device:      ID3D11Device,
    context:     ID3D11DeviceContext,
    multithread: ID3D10Multithread,
    backend:     Backend,
    swapchain:   IDXGISwapChain1,
    latest:      ID3D11Texture2D,
    width:       u32,
    height:      u32,
}

unsafe impl Send for VideoCapture {}

impl VideoCapture {
    pub fn open(source: &Source, output: Option<u32>, preview: HWND, cursor: bool) -> Result<Self> {
        unsafe {
            let factory: IDXGIFactory2 = CreateDXGIFactory1()?;
            let (device, context) = match source {
                Source::Monitor(monitor) => create_device(Some(&factory.EnumAdapters1(monitor.adapter)?))?,
                Source::Program(_) => create_device(None)?,
            };
            let multithread: ID3D10Multithread = device.cast()?;

            _ = multithread.SetMultithreadProtected(true);

            let (backend, latest, width, height) = match source {
                Source::Monitor(monitor) => open_monitor(&factory, &device, monitor, output, cursor)?,
                Source::Program(program) => open_program(&device, program, output, cursor)?,
            };
            let swapchain_desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let swapchain = factory.CreateSwapChainForHwnd(&device, preview, &swapchain_desc, None, None)?;

            factory.MakeWindowAssociation(preview, DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES)?;

            Ok(Self { device, context, multithread, backend, swapchain, latest, width, height })
        }
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn run(mut self, fps: u32, frames: Option<(SyncSender<Frame>, FramePool)>, clock: &Clock, stop: &AtomicBool, show_preview: &AtomicBool) -> Result<()> {
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST)?;

            let timer = CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0)?;
            let result = self.capture_loop(fps as i64, frames.as_ref(), clock, stop, show_preview, timer);

            self.multithread.Enter();
            self.context.ClearState();
            self.context.Flush();
            self.multithread.Leave();
            CloseHandle(timer)?;

            result
        }
    }

    fn capture_loop(&mut self, fps: i64, frames: Option<&(SyncSender<Frame>, FramePool)>, clock: &Clock, stop: &AtomicBool, show_preview: &AtomicBool, timer: HANDLE) -> Result<()> {
        let preview_every = (fps / PREVIEW_FPS as i64).max(1);
        let mut frame: i64 = 0;
        let mut stale = true;
        let mut starved: i64 = 0;

        while !stop.load(Ordering::Relaxed) {
            let due  = frame_time(frame, fps);
            let wait = due - clock.elapsed()?;

            if wait > 0 {
                unsafe {
                    SetWaitableTimer(timer, &-wait, 0, None, None, false)?;

                    if WaitForSingleObject(timer, INFINITE) != WAIT_OBJECT_0 {
                        return Err(Error::from_win32());
                    }
                }
            }

            stale |= self.acquire()?;

            let present = stale && show_preview.load(Ordering::Relaxed) && frame % preview_every == 0;

            match frames {
                Some((sender, pool)) => match pool.take()? {
                    Some(pooled) => {
                        self.render(Some(&pooled.texture), present)?;
                        sender.send(pooled.finish(due, frame_time(frame + 1, fps) - due)?).map_err(|_| Error::new(E_FAIL, "The video encoder stopped"))?;
                        stale &= !present;
                        starved = 0;
                    }
                    None if starved > fps * STARVED_SECONDS => return Err(Error::new(E_FAIL, format!("The video encoder has not accepted a frame for {STARVED_SECONDS} seconds"))),
                    None => starved += 1,
                },
                None if present => {
                    self.render(None, true)?;
                    stale = false;
                }
                None => {}
            }

            frame += 1;

            let current = clock.elapsed()? * fps / 10_000_000;

            if current > frame {
                frame = current;
            }
        }

        Ok(())
    }

    fn acquire(&mut self) -> Result<bool> {
        match &mut self.backend {
            Backend::Monitor { output, duplication, cursor, desktop, .. } => unsafe {
                if duplication.is_none() {
                    match output.DuplicateOutput(&self.device) {
                        Ok(fresh) => {
                            let mode = fresh.GetDesc().ModeDesc;

                            if (mode.Width, mode.Height) != *desktop {
                                return Err(Error::new(E_FAIL, "Monitor resolution changed during capture"));
                            }

                            *duplication = Some(fresh);
                        }
                        Err(e) if e.code() == E_ACCESSDENIED => return Ok(false),
                        Err(e) => return Err(e),
                    }
                }

                let Some(active) = duplication.as_ref() else {
                    return Ok(false);
                };
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource = None;

                match active.AcquireNextFrame(0, &mut info, &mut resource) {
                    Ok(()) => {
                        let texture: ID3D11Texture2D = resource.ok_or_else(|| Error::new(E_FAIL, "AcquireNextFrame returned no resource"))?.cast()?;

                        self.multithread.Enter();
                        self.context.CopyResource(&self.latest, &texture);
                        self.multithread.Leave();

                        if let Some(cursor) = cursor {
                            cursor.update(&self.device, active, &info)?;
                        }

                        active.ReleaseFrame()?;
                        Ok(true)
                    }
                    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => Ok(false),
                    Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                        *duplication = None;
                        Ok(false)
                    }
                    Err(e) => Err(e),
                }
            },
            Backend::Program { window, capture, content, scaler } => unsafe {
                if !IsWindow(Some(*window)).as_bool() {
                    return Err(Error::new(E_FAIL, "The program being captured was closed"));
                }

                let frame = match capture.pool.TryGetNextFrame() {
                    Ok(frame) => frame,
                    Err(e) if e.code().is_ok() => return Ok(false),
                    Err(e) => return Err(e),
                };
                let size = frame.ContentSize()?;
                let texture: ID3D11Texture2D = frame.Surface()?.cast::<IDirect3DDxgiInterfaceAccess>()?.GetInterface()?;
                let mut surface = D3D11_TEXTURE2D_DESC::default();

                texture.GetDesc(&mut surface);

                let visible = SizeInt32 { Width: size.Width.min(surface.Width as i32), Height: size.Height.min(surface.Height as i32) };

                let crop = client_crop(*window, visible)?;

                if let Some(crop) = crop {
                    self.multithread.Enter();

                    let drawn = scaler.draw(&texture, crop, &self.latest, self.width, self.height);

                    self.multithread.Leave();
                    drawn?;
                }

                drop(texture);
                frame.Close()?;

                if size != *content && size.Width > 0 && size.Height > 0 {
                    capture.pool.Recreate(&*capture.device, DirectXPixelFormat::B8G8R8A8UIntNormalized, FRAME_BUFFERS, size)?;
                    *content = size;
                }

                Ok(crop.is_some())
            },
        }
    }

    fn render(&self, target: Option<&ID3D11Texture2D>, present: bool) -> Result<()> {
        unsafe {
            self.multithread.Enter();

            let result = self.render_locked(target, present);

            self.multithread.Leave();
            result
        }
    }

    unsafe fn render_locked(&self, target: Option<&ID3D11Texture2D>, present: bool) -> Result<()> {
        unsafe {
            let back: Option<ID3D11Texture2D> = match present {
                true => Some(self.swapchain.GetBuffer(0)?),
                false => None,
            };

            match (target, &back) {
                (Some(target), back) => {
                    self.compose(target)?;

                    if let Some(back) = back {
                        self.context.CopyResource(back, target);
                    }
                }
                (None, Some(back)) => self.compose(back)?,
                (None, None) => return Ok(()),
            }

            if !present {
                return Ok(());
            }

            match self.swapchain.Present(0, DXGI_PRESENT_DO_NOT_WAIT).ok() {
                Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => Ok(()),
                result => result,
            }
        }
    }

    unsafe fn compose(&self, texture: &ID3D11Texture2D) -> Result<()> {
        unsafe {
            match &self.backend {
                Backend::Monitor { cursor, resize, .. } => {
                    let canvas = resize.as_ref().map_or(texture, |resize| &resize.canvas);

                    self.context.CopyResource(canvas, &self.latest);

                    if let Some(cursor) = cursor {
                        cursor.draw(&self.device, &self.context, canvas)?;
                    }

                    if let Some(resize) = resize {
                        resize.scaler.draw(&resize.canvas, resize.area, texture, self.width, self.height)?;
                    }
                }
                Backend::Program { .. } => self.context.CopyResource(texture, &self.latest),
            }

            Ok(())
        }
    }
}

pub unsafe fn create_device(adapter: Option<&IDXGIAdapter1>) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    unsafe {
        let mut device = None;
        let mut context = None;
        let driver = if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE };

        let adapter: Option<IDXGIAdapter> = adapter.map(Interface::cast).transpose()?;

        D3D11CreateDevice(
            adapter.as_ref(),
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;

        Ok((
            device.ok_or_else(|| Error::new(E_FAIL, "D3D11CreateDevice returned no device"))?,
            context.ok_or_else(|| Error::new(E_FAIL, "D3D11CreateDevice returned no context"))?,
        ))
    }
}

unsafe fn open_monitor(factory: &IDXGIFactory2, device: &ID3D11Device, monitor: &MonitorInfo, output_height: Option<u32>, cursor: bool) -> Result<(Backend, ID3D11Texture2D, u32, u32)> {
    unsafe {
        let output: IDXGIOutput1 = factory.EnumAdapters1(monitor.adapter)?.EnumOutputs(monitor.output)?.cast()?;
        let desc = output.GetDesc()?;

        if desc.Rotation != DXGI_MODE_ROTATION_IDENTITY && desc.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED {
            return Err(Error::new(E_FAIL, "Rotated monitors are not supported"));
        }

        let duplication = output.DuplicateOutput(device)?;
        let mode = duplication.GetDesc().ModeDesc;

        if mode.Width % 2 != 0 || mode.Height % 2 != 0 {
            return Err(Error::new(E_FAIL, format!("Monitor resolution {}x{} must have even dimensions for H.264", mode.Width, mode.Height)));
        }

        let latest = create_texture(device, mode.Width, mode.Height)?;
        let cursor = match cursor {
            true => Some(CursorOverlay::new(device, &latest, mode.Width, mode.Height)?),
            false => None,
        };

        let (width, height) = output_size((mode.Width, mode.Height), output_height);
        let resize = match (width, height) == (mode.Width, mode.Height) {
            true => None,
            false => Some(Resize {
                canvas: create_texture(device, mode.Width, mode.Height)?,
                scaler: Scaler::new(device)?,
                area: D2D_RECT_F { left: 0.0, top: 0.0, right: mode.Width as f32, bottom: mode.Height as f32 },
            }),
        };

        Ok((Backend::Monitor { output, duplication: Some(duplication), cursor, resize, desktop: (mode.Width, mode.Height) }, latest, width, height))
    }
}

unsafe fn open_program(device: &ID3D11Device, program: &Program, output_height: Option<u32>, cursor: bool) -> Result<(Backend, ID3D11Texture2D, u32, u32)> {
    if !GraphicsCaptureSession::IsSupported()? {
        return Err(Error::new(E_FAIL, "Window capture is not supported on this version of Windows"));
    }

    let window = program.hwnd();
    let mut client = RECT::default();

    unsafe { GetClientRect(window, &mut client)? };

    let client = (client.right as u32 & !1, client.bottom as u32 & !1);

    if unsafe { IsIconic(window) }.as_bool() || client.0 < 2 || client.1 < 2 {
        return Err(Error::new(E_FAIL, format!("{} is minimized or too small to capture. Restore it and try again.", program.label())));
    }

    let shared = Movable(device.clone());
    let target = program.clone();
    let Movable((capture, size)) = in_mta(move || unsafe {
        let Movable(device) = shared;
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = interop.CreateForWindow(target.hwnd()).map_err(|e| Error::new(e.code(), format!("Cannot capture {}: {}", target.label(), e.message())))?;
        let size = item.Size()?;
        let winrt_device: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&device.cast::<IDXGIDevice>()?)?.cast()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt_device, DirectXPixelFormat::B8G8R8A8UIntNormalized, FRAME_BUFFERS, size)?;
        let session = pool.CreateCaptureSession(&item)?;
        let capture = WindowCapture { pool: ManuallyDrop::new(pool), session: ManuallyDrop::new(session), device: ManuallyDrop::new(winrt_device) };

        capture.session.SetIsCursorCaptureEnabled(cursor)?;
        capture.session.StartCapture()?;
        Ok(Movable((capture, size)))
    })?;
    let (width, height) = output_size(client, output_height);
    let latest = create_texture(device, width, height)?;
    let scaler = Scaler::new(device)?;

    Ok((Backend::Program { window, capture, content: size, scaler }, latest, width, height))
}

fn client_crop(window: HWND, frame: SizeInt32) -> Result<Option<D2D_RECT_F>> {
    unsafe {
        if IsIconic(window).as_bool() {
            return Ok(None);
        }

        let (mut outer, mut bounds, mut client, mut origin) = (RECT::default(), RECT::default(), RECT::default(), POINT::default());

        GetWindowRect(window, &mut outer)?;
        DwmGetWindowAttribute(window, DWMWA_EXTENDED_FRAME_BOUNDS, &mut bounds as *mut RECT as *mut _, size_of::<RECT>() as u32)?;
        GetClientRect(window, &mut client)?;
        ClientToScreen(window, &mut origin).ok()?;

        let frame_origin = match outer.right - outer.left == frame.Width && outer.bottom - outer.top == frame.Height {
            true => (outer.left, outer.top),
            false => (bounds.left, bounds.top),
        };
        let left = (origin.x - frame_origin.0).clamp(0, frame.Width);
        let top = (origin.y - frame_origin.1).clamp(0, frame.Height);
        let right = (left + client.right).min(frame.Width);
        let bottom = (top + client.bottom).min(frame.Height);

        if right - left < 2 || bottom - top < 2 {
            return Ok(None);
        }

        Ok(Some(D2D_RECT_F { left: left as f32, top: top as f32, right: right as f32, bottom: bottom as f32 }))
    }
}

pub fn output_size(source: (u32, u32), output_height: Option<u32>) -> (u32, u32) {
    output_height.map_or(source, fixed_size)
}

pub fn fixed_size(height: u32) -> (u32, u32) {
    ((height * 16 / 9 + 1) & !1, height)
}

struct Resize {
    canvas: ID3D11Texture2D,
    scaler: Scaler,
    area:   D2D_RECT_F,
}

struct Scaler {
    context: ID2D1DeviceContext,
}

impl Scaler {
    fn new(device: &ID3D11Device) -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)?;
            let context = factory.CreateDevice(&device.cast::<IDXGIDevice>()?)?.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;

            Ok(Self { context })
        }
    }

    unsafe fn draw(&self, texture: &ID3D11Texture2D, crop: D2D_RECT_F, target: &ID3D11Texture2D, width: u32, height: u32) -> Result<()> {
        unsafe {
            let source = self.context.CreateBitmapFromDxgiSurface(&texture.cast::<IDXGISurface>()?, Some(&bitmap_properties(D2D1_BITMAP_OPTIONS_NONE)))?;
            let output = self.context.CreateBitmapFromDxgiSurface(&target.cast::<IDXGISurface>()?, Some(&bitmap_properties(D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW)))?;
            let (width, height) = (width as f32, height as f32);
            let (crop_width, crop_height) = (crop.right - crop.left, crop.bottom - crop.top);
            let fits = (width..=width + 1.0).contains(&crop_width) && (height..=height + 1.0).contains(&crop_height);
            let (crop, destination) = match fits {
                true => (D2D_RECT_F { right: crop.left + width, bottom: crop.top + height, ..crop }, D2D_RECT_F { left: 0.0, top: 0.0, right: width, bottom: height }),
                false => {
                    let scale = (width / crop_width).min(height / crop_height);
                    let (scaled_width, scaled_height) = (crop_width * scale, crop_height * scale);
                    let (left, top) = (((width - scaled_width) / 2.0).round(), ((height - scaled_height) / 2.0).round());

                    (crop, D2D_RECT_F { left, top, right: left + scaled_width, bottom: top + scaled_height })
                }
            };

            self.context.SetTarget(&output);
            self.context.BeginDraw();
            self.context.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }));
            self.context.DrawBitmap(&source, Some(&destination), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, Some(&crop), None);

            let drawn = self.context.EndDraw(None, None);

            self.context.SetTarget(None);
            drawn
        }
    }
}

fn bitmap_properties(options: D2D1_BITMAP_OPTIONS) -> D2D1_BITMAP_PROPERTIES1 {
    D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: options,
        colorContext: ManuallyDrop::new(None),
    }
}

struct WindowCapture {
    pool:    ManuallyDrop<Direct3D11CaptureFramePool>,
    session: ManuallyDrop<GraphicsCaptureSession>,
    device:  ManuallyDrop<IDirect3DDevice>,
}

impl Drop for WindowCapture {
    fn drop(&mut self) {
        let objects = Movable(unsafe { (ManuallyDrop::take(&mut self.session), ManuallyDrop::take(&mut self.pool), ManuallyDrop::take(&mut self.device)) });

        _ = in_mta(move || {
            drop(objects);
            Ok(())
        });
    }
}

pub struct Movable<T>(pub T);

unsafe impl<T> Send for Movable<T> {}

fn frame_time(frame: i64, fps: i64) -> i64 {
    frame * 10_000_000 / fps
}

fn create_texture(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;

    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
    texture.ok_or_else(|| Error::new(E_FAIL, "CreateTexture2D returned no texture"))
}
