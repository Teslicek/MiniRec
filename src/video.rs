use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;

use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, E_FAIL, HANDLE, HMODULE, HWND, POINT, RECT, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::{ClientToScreen, DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromWindow};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, IsIconic, IsWindow};
use windows::core::{Error, HSTRING, Interface, Result};

use crate::cursor::CursorOverlay;
use crate::encoder::{Frame, FramePool};
use crate::programs::{self, Program};
use crate::recorder::Clock;

pub const PREVIEW_FPS: u32 = 30;
const STARVED_SECONDS: i64 = 2;

#[derive(Clone)]
pub struct MonitorInfo {
    pub adapter: u32,
    pub output:  u32,
    pub name:    String,
    pub width:   u32,
    pub height:  u32,
    pub refresh: u32,
    pub primary: bool,
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

            list.push(MonitorInfo {
                adapter: adapter_index,
                output: output_index,
                name: device.trim_start_matches(r"\\.\").to_string(),
                width: (rect.right - rect.left) as u32,
                height: (rect.bottom - rect.top) as u32,
                refresh: mode.dmDisplayFrequency,
                primary: rect.left == 0 && rect.top == 0,
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
    Program(Program, MonitorInfo),
}

pub struct VideoCapture {
    device:      ID3D11Device,
    context:     ID3D11DeviceContext,
    multithread: ID3D10Multithread,
    adapter:     IDXGIAdapter1,
    screen:      Screen,
    scaler:      Option<Scaler>,
    window:      Option<HWND>,
    cursor:      bool,
    swapchain:   IDXGISwapChain1,
    width:       u32,
    height:      u32,
}

unsafe impl Send for VideoCapture {}

impl VideoCapture {
    pub fn open(source: &Source, output_height: Option<u32>, preview: HWND, cursor: bool) -> Result<Self> {
        unsafe {
            let (monitor, program) = match source {
                Source::Monitor(monitor) => (monitor, None),
                Source::Program(program, monitor) => (monitor, Some(program)),
            };
            let factory: IDXGIFactory2 = CreateDXGIFactory1()?;
            let adapter = factory.EnumAdapters1(monitor.adapter)?;
            let (device, context) = create_device(&adapter)?;
            let multithread: ID3D10Multithread = device.cast()?;

            _ = multithread.SetMultithreadProtected(true);

            let desktop = (monitor.width, monitor.height);
            let source_size = match program {
                Some(program) => program_size(program)?,
                None => desktop,
            };
            let (width, height) = output_size(source_size, output_height);
            let scaled = program.is_some() || (width, height) != desktop;
            let screen = Screen::open(&device, adapter.EnumOutputs(monitor.output)?.cast()?, cursor, scaled)?;
            let scaler = match scaled {
                true => Some(Scaler::new(&device)?),
                false => None,
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

            Ok(Self { device, context, multithread, adapter, screen, scaler, window: program.map(Program::hwnd), cursor, swapchain, width, height })
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
        unsafe {
            if let Some(window) = self.window {
                if !IsWindow(Some(window)).as_bool() {
                    return Err(Error::new(E_FAIL, "The program being captured was closed"));
                }

                self.follow(window)?;
            }

            let screen = &mut self.screen;

            if screen.duplication.is_none() {
                match screen.output.DuplicateOutput(&self.device) {
                    Ok(fresh) => {
                        let mode = fresh.GetDesc().ModeDesc;

                        if (mode.Width, mode.Height) != screen.desktop {
                            return Err(Error::new(E_FAIL, "Monitor resolution changed during capture"));
                        }

                        screen.duplication = Some(fresh);
                    }
                    Err(e) if e.code() == E_ACCESSDENIED => return Ok(false),
                    Err(e) => return Err(e),
                }
            }

            let Some(active) = screen.duplication.as_ref() else {
                return Ok(false);
            };
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource = None;

            match active.AcquireNextFrame(0, &mut info, &mut resource) {
                Ok(()) => {
                    let texture: ID3D11Texture2D = resource.ok_or_else(|| Error::new(E_FAIL, "AcquireNextFrame returned no resource"))?.cast()?;

                    self.multithread.Enter();
                    self.context.CopyResource(&screen.latest, &texture);
                    self.multithread.Leave();

                    if let Some(cursor) = &mut screen.cursor {
                        cursor.update(&self.device, active, &info)?;
                    }

                    active.ReleaseFrame()?;
                    Ok(true)
                }
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => Ok(false),
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    screen.duplication = None;
                    Ok(false)
                }
                Err(e) => Err(e),
            }
        }
    }

    fn follow(&mut self, window: HWND) -> Result<()> {
        let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };

        if monitor == self.screen.monitor {
            return Ok(());
        }

        let output = find_output(&self.adapter, monitor)?;

        self.screen = Screen::open(&self.device, output, self.cursor, self.scaler.is_some())?;
        Ok(())
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
            let screen = &self.screen;
            let canvas = screen.canvas.as_ref().unwrap_or(texture);

            self.context.CopyResource(canvas, &screen.latest);

            if let Some(cursor) = &screen.cursor {
                cursor.draw(&self.device, &self.context, canvas)?;
            }

            if let Some(scaler) = &self.scaler {
                scaler.draw(canvas, self.area()?, texture, self.width, self.height)?;
            }

            Ok(())
        }
    }

    fn area(&self) -> Result<Option<D2D_RECT_F>> {
        let (width, height) = (self.screen.desktop.0 as i32, self.screen.desktop.1 as i32);
        let Some(window) = self.window else {
            return Ok(Some(D2D_RECT_F { left: 0.0, top: 0.0, right: width as f32, bottom: height as f32 }));
        };

        unsafe {
            if IsIconic(window).as_bool() {
                return Ok(None);
            }

            let (mut client, mut corner) = (RECT::default(), POINT::default());

            GetClientRect(window, &mut client)?;
            ClientToScreen(window, &mut corner).ok()?;

            let (x, y) = (corner.x - self.screen.origin.x, corner.y - self.screen.origin.y);
            let (left, top) = (x.clamp(0, width), y.clamp(0, height));
            let (right, bottom) = ((x + client.right).clamp(0, width), (y + client.bottom).clamp(0, height));

            if right - left < 2 || bottom - top < 2 {
                return Ok(None);
            }

            Ok(Some(D2D_RECT_F { left: left as f32, top: top as f32, right: right as f32, bottom: bottom as f32 }))
        }
    }
}

struct Screen {
    output:      IDXGIOutput1,
    duplication: Option<IDXGIOutputDuplication>,
    monitor:     HMONITOR,
    origin:      POINT,
    desktop:     (u32, u32),
    latest:      ID3D11Texture2D,
    canvas:      Option<ID3D11Texture2D>,
    cursor:      Option<CursorOverlay>,
}

impl Screen {
    fn open(device: &ID3D11Device, output: IDXGIOutput1, cursor: bool, canvas: bool) -> Result<Self> {
        unsafe {
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
            let canvas = match canvas {
                true => Some(create_texture(device, mode.Width, mode.Height)?),
                false => None,
            };

            Ok(Self {
                output,
                duplication: Some(duplication),
                monitor: desc.Monitor,
                origin: POINT { x: desc.DesktopCoordinates.left, y: desc.DesktopCoordinates.top },
                desktop: (mode.Width, mode.Height),
                latest,
                canvas,
                cursor,
            })
        }
    }
}

fn find_output(adapter: &IDXGIAdapter1, monitor: HMONITOR) -> Result<IDXGIOutput1> {
    for index in 0.. {
        let output = match unsafe { adapter.EnumOutputs(index) } {
            Ok(output) => output,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e),
        };

        if unsafe { output.GetDesc()? }.Monitor == monitor {
            return output.cast();
        }
    }

    Err(Error::new(E_FAIL, "The program moved to a monitor on another graphics card, which MiniRec cannot follow"))
}

unsafe fn create_device(adapter: &IDXGIAdapter1) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    unsafe {
        let mut device = None;
        let mut context = None;

        D3D11CreateDevice(
            &adapter.cast::<IDXGIAdapter>()?,
            D3D_DRIVER_TYPE_UNKNOWN,
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

fn program_size(program: &Program) -> Result<(u32, u32)> {
    let mut client = RECT::default();

    unsafe { GetClientRect(program.hwnd(), &mut client)? };

    let size = (client.right as u32 & !1, client.bottom as u32 & !1);

    if programs::minimized(program) || size.0 < 2 || size.1 < 2 {
        return Err(Error::new(E_FAIL, format!("{} is minimized or too small to capture. Restore it and try again.", program.label())));
    }

    Ok(size)
}

pub fn monitor_of(window: HWND, monitors: &[MonitorInfo]) -> Result<MonitorInfo> {
    unsafe {
        let mut info = MONITORINFOEXW { monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() }, ..Default::default() };

        GetMonitorInfoW(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST), &mut info.monitorInfo).ok()?;

        let name = String::from_utf16_lossy(&info.szDevice).trim_end_matches('\0').trim_start_matches(r"\\.\").to_string();

        monitors.iter().find(|monitor| monitor.name == name).cloned().ok_or_else(|| Error::new(E_FAIL, format!("The window is on {name}, which is not an active monitor")))
    }
}

pub fn output_size(source: (u32, u32), output_height: Option<u32>) -> (u32, u32) {
    output_height.map_or(source, fixed_size)
}

pub fn fixed_size(height: u32) -> (u32, u32) {
    ((height * 16 / 9 + 1) & !1, height)
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

    unsafe fn draw(&self, texture: &ID3D11Texture2D, crop: Option<D2D_RECT_F>, target: &ID3D11Texture2D, width: u32, height: u32) -> Result<()> {
        unsafe {
            let output = self.context.CreateBitmapFromDxgiSurface(&target.cast::<IDXGISurface>()?, Some(&bitmap_properties(D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW)))?;

            self.context.SetTarget(&output);
            self.context.BeginDraw();
            self.context.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }));

            if let Some(crop) = crop {
                let source = self.context.CreateBitmapFromDxgiSurface(&texture.cast::<IDXGISurface>()?, Some(&bitmap_properties(D2D1_BITMAP_OPTIONS_NONE)))?;
                let (crop, destination) = fit(crop, width as f32, height as f32);

                self.context.DrawBitmap(&source, Some(&destination), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, Some(&crop), None);
            }

            let drawn = self.context.EndDraw(None, None);

            self.context.SetTarget(None);
            drawn
        }
    }
}

fn fit(crop: D2D_RECT_F, width: f32, height: f32) -> (D2D_RECT_F, D2D_RECT_F) {
    let (crop_width, crop_height) = (crop.right - crop.left, crop.bottom - crop.top);

    if (width..=width + 1.0).contains(&crop_width) && (height..=height + 1.0).contains(&crop_height) {
        return (D2D_RECT_F { right: crop.left + width, bottom: crop.top + height, ..crop }, D2D_RECT_F { left: 0.0, top: 0.0, right: width, bottom: height });
    }

    let scale = (width / crop_width).min(height / crop_height);
    let (scaled_width, scaled_height) = (crop_width * scale, crop_height * scale);
    let (left, top) = (((width - scaled_width) / 2.0).round(), ((height - scaled_height) / 2.0).round());

    (crop, D2D_RECT_F { left, top, right: left + scaled_width, bottom: top + scaled_height })
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
