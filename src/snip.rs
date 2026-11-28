use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{E_FAIL, GENERIC_WRITE, GlobalFree, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_DIB;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Error, HSTRING, Interface, PCWSTR, Result, w};

use crate::recorder::recording_path;
use crate::settings::create_folder;
use crate::shell::error_box;
use crate::video::{MonitorInfo, create_device, monitors};

pub const WM_SNIP: u32 = WM_APP + 6;

const CLASS: PCWSTR = w!("MiniRecSnip");
const MASK_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);
const FRAME_WAIT: Duration = Duration::from_millis(250);
const MIN_SELECTION: i32 = 4;
const RECENTER_WINDOW: Duration = Duration::from_millis(1500);
const RECENTER_JUMP: i32 = 100;

static TARGET: AtomicIsize = AtomicIsize::new(0);
static SWALLOWED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static SNIP: RefCell<Option<Snip>> = const { RefCell::new(None) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
}

struct Shot {
    left:   i32,
    top:    i32,
    width:  i32,
    height: i32,
    pixels: Vec<u8>,
}

struct Layer {
    dc:       HDC,
    bitmap:   HBITMAP,
    previous: HGDIOBJ,
    bits:     *mut u8,
    len:      usize,
}

impl Layer {
    fn blank(width: i32, height: i32) -> Result<Self> {
        unsafe {
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(None, &bitmap_info(width, -height), DIB_RGB_COLORS, &mut bits, None, 0)?;
            let dc = CreateCompatibleDC(None);

            if dc.is_invalid() {
                _ = DeleteObject(bitmap.into());
                return Err(Error::new(E_FAIL, "Cannot create a drawing surface for the screenshot"));
            }

            let previous = SelectObject(dc, bitmap.into());

            Ok(Self { dc, bitmap, previous, bits: bits.cast(), len: (width * height) as usize * 4 })
        }
    }

    fn filled(width: i32, height: i32, pixels: &[u8]) -> Result<Self> {
        let layer = Self::blank(width, height)?;

        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), layer.bits, layer.len) };
        Ok(layer)
    }

    fn pixels(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bits, self.len) }
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            _ = DeleteObject(self.bitmap.into());
            _ = DeleteDC(self.dc);
        }
    }
}

struct Snip {
    window:   HWND,
    shot:     Shot,
    bright:   Layer,
    dim:      Layer,
    anchor:   Option<POINT>,
    cursor:   POINT,
    previous: HWND,
    folder:   PathBuf,
    opened:   Instant,
    recenter: Option<POINT>,
    last:     Option<POINT>,
}

pub fn hook(window: HWND) -> Result<()> {
    TARGET.store(window.0 as isize, Ordering::Relaxed);

    let instance: HINSTANCE = unsafe { GetModuleHandleW(None)? }.into();
    let module = instance.0 as isize;
    let (sender, receiver) = mpsc::channel();

    std::thread::spawn(move || {
        let installed = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(HINSTANCE(module as *mut _)), 0) }.map(|_| ());
        let running = installed.is_ok();

        if sender.send(installed).is_err() || !running {
            return;
        }

        let mut message = MSG::default();

        while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {}
    });

    receiver.recv().map_err(|_| Error::new(E_FAIL, "The screenshot shortcut thread stopped before it started"))?
}

pub fn open(folder: &Path) -> Result<()> {
    if SNIP.with_borrow(Option::is_some) {
        return Ok(());
    }

    let previous = unsafe { GetForegroundWindow() };
    let shot = capture()?;
    let bright = Layer::filled(shot.width, shot.height, &shot.pixels)?;
    let dim = Layer::filled(shot.width, shot.height, &dimmed(&shot.pixels))?;

    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();

        if !REGISTERED.get() {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_CROSS)?,
                lpszClassName: CLASS,
                ..Default::default()
            };

            if RegisterClassExW(&class) == 0 {
                return Err(Error::from_win32());
            }

            REGISTERED.set(true);
        }

        let window = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW, CLASS, w!("MiniRec Screenshot"), WS_POPUP, shot.left, shot.top, shot.width, shot.height, None, None, Some(instance), None)?;

        let recenter = client_center(previous)?;

        SNIP.set(Some(Snip { window, shot, bright, dim, anchor: None, cursor: POINT::default(), previous, folder: folder.to_path_buf(), opened: Instant::now(), recenter, last: None }));
        _ = ShowWindow(window, SW_SHOW);

        if !bring_to_front(window, previous) {
            close()?;
            return Err(Error::new(E_FAIL, "Windows did not let the screenshot window come to the front."));
        }
    }

    Ok(())
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && swallow(wparam.0 as u32, unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) }) {
        return LRESULT(1);
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn swallow(message: u32, key: &KBDLLHOOKSTRUCT) -> bool {
    if key.vkCode != u32::from(b'S') || key.flags.0 & LLKHF_INJECTED.0 != 0 {
        return false;
    }

    if !matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) {
        return SWALLOWED.swap(false, Ordering::Relaxed);
    }

    if SWALLOWED.load(Ordering::Relaxed) {
        return true;
    }

    let down = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(vk.0 as i32) } < 0;

    if !(down(VK_LWIN) || down(VK_RWIN)) || !down(VK_SHIFT) || down(VK_CONTROL) || down(VK_MENU) {
        return false;
    }

    let target = HWND(TARGET.load(Ordering::Relaxed) as *mut _);

    if unsafe { PostMessageW(Some(target), WM_SNIP, WPARAM(0), LPARAM(0)) }.is_err() {
        return false;
    }

    SWALLOWED.store(true, Ordering::Relaxed);
    mask_start_menu();
    true
}

fn mask_start_menu() {
    let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type:    INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: MASK_KEY, dwFlags: flags, ..Default::default() } },
    };

    unsafe { SendInput(&[key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)], size_of::<INPUT>() as i32) };
}

fn capture() -> Result<Shot> {
    let screens = monitors()?;
    let shots = std::thread::scope(|scope| {
        let threads: Vec<_> = screens.iter().map(|monitor| scope.spawn(|| capture_monitor(monitor))).collect();

        threads.into_iter().map(|thread| thread.join().map_err(|_| Error::new(E_FAIL, "A screenshot thread stopped unexpectedly"))?).collect::<Result<Vec<Shot>>>()
    })?;

    combine(&shots)
}

fn capture_monitor(monitor: &MonitorInfo) -> Result<Shot> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let adapter = factory.EnumAdapters1(monitor.adapter)?;
        let output = adapter.EnumOutputs(monitor.output)?;

        grab(&adapter, &output.cast()?, monitor.bounds)
    }
}

fn combine(shots: &[Shot]) -> Result<Shot> {
    let area = shots
        .iter()
        .map(|shot| RECT { left: shot.left, top: shot.top, right: shot.left + shot.width, bottom: shot.top + shot.height })
        .reduce(|a, b| RECT { left: a.left.min(b.left), top: a.top.min(b.top), right: a.right.max(b.right), bottom: a.bottom.max(b.bottom) })
        .ok_or_else(|| Error::new(E_FAIL, "No screen was captured"))?;
    let (left, top, width, height) = (area.left, area.top, area.right - area.left, area.bottom - area.top);
    let mut pixels: Vec<u8> = [0, 0, 0, 255].repeat((width * height) as usize);

    for shot in shots {
        let row = shot.width as usize * 4;

        for line in 0..shot.height {
            let target = (((shot.top - top + line) * width + shot.left - left) * 4) as usize;

            pixels[target..target + row].copy_from_slice(&shot.pixels[line as usize * row..][..row]);
        }
    }

    Ok(Shot { left, top, width, height, pixels })
}

unsafe fn grab(adapter: &IDXGIAdapter1, output: &IDXGIOutput1, bounds: RECT) -> Result<Shot> {
    unsafe {
        let (device, context) = create_device(Some(adapter))?;
        let duplication = output.DuplicateOutput(&device)?;
        let deadline = Instant::now() + FRAME_WAIT;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource = None;

            match duplication.AcquireNextFrame(remaining, &mut info, &mut resource) {
                Ok(()) if info.LastPresentTime != 0 => {
                    let texture: ID3D11Texture2D = resource.ok_or_else(|| Error::new(E_FAIL, "The screen capture returned no image"))?.cast()?;
                    let shot = read_pixels(&device, &context, &texture, bounds);

                    duplication.ReleaseFrame()?;
                    return shot;
                }
                Ok(()) => duplication.ReleaseFrame()?,
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return copy_desktop(bounds),
                Err(e) => return Err(e),
            }
        }
    }
}

fn copy_desktop(bounds: RECT) -> Result<Shot> {
    let (width, height) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
    let layer = Layer::blank(width, height)?;

    unsafe {
        let screen = GetDC(None);

        if screen.is_invalid() {
            return Err(Error::new(E_FAIL, "Cannot read the screen"));
        }

        let copied = BitBlt(layer.dc, 0, 0, width, height, Some(screen), bounds.left, bounds.top, SRCCOPY | CAPTUREBLT);

        ReleaseDC(None, screen);
        copied?;
    }

    let mut pixels = layer.pixels().to_vec();

    pixels.chunks_exact_mut(4).for_each(|pixel| pixel[3] = 255);
    Ok(Shot { left: bounds.left, top: bounds.top, width, height, pixels })
}

unsafe fn read_pixels(device: &ID3D11Device, context: &ID3D11DeviceContext, texture: &ID3D11Texture2D, bounds: RECT) -> Result<Shot> {
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();

        texture.GetDesc(&mut desc);

        let (width, height) = (bounds.right - bounds.left, bounds.bottom - bounds.top);

        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(Error::new(E_FAIL, format!("Screenshots of this screen's color format ({}) are not supported", desc.Format.0)));
        }

        if (desc.Width as i32, desc.Height as i32) != (width, height) {
            return Err(Error::new(E_FAIL, "Screenshots of rotated screens are not supported"));
        }

        let staging_desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
            MipLevels: 1,
            ArraySize: 1,
            ..desc
        };
        let mut staging = None;

        device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;

        let staging = staging.ok_or_else(|| Error::new(E_FAIL, "CreateTexture2D returned no texture"))?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();

        context.CopyResource(&staging, texture);
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;

        let row = width as usize * 4;
        let source = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), mapped.RowPitch as usize * (height as usize - 1) + row);
        let mut pixels = Vec::with_capacity(row * height as usize);

        for line in 0..height as usize {
            pixels.extend_from_slice(&source[line * mapped.RowPitch as usize..][..row]);
        }

        context.Unmap(&staging, 0);
        pixels.chunks_exact_mut(4).for_each(|pixel| pixel[3] = 255);
        Ok(Shot { left: bounds.left, top: bounds.top, width, height, pixels })
    }
}

fn dimmed(pixels: &[u8]) -> Vec<u8> {
    let dim = |value: u8| (u16::from(value) * 2 / 5) as u8;

    pixels.chunks_exact(4).flat_map(|pixel| [dim(pixel[0]), dim(pixel[1]), dim(pixel[2]), 255]).collect()
}

fn bring_to_front(window: HWND, previous: HWND) -> bool {
    unsafe {
        let current = GetCurrentThreadId();
        let foreground = GetWindowThreadProcessId(previous, None);
        let attached = foreground != 0 && foreground != current && AttachThreadInput(current, foreground, true).as_bool();
        let raised = SetForegroundWindow(window).as_bool();

        if attached {
            _ = AttachThreadInput(current, foreground, false);
        }

        raised
    }
}

extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ready = SNIP.with(|cell| matches!(cell.try_borrow().as_deref(), Ok(Some(_))));

    if !ready {
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    }

    match handle(window, message, wparam, lparam) {
        Ok(Some(result)) => result,
        Ok(None) => unsafe { DefWindowProcW(window, message, wparam, lparam) },
        Err(e) => {
            error_box(None, &e);
            LRESULT(0)
        }
    }
}

fn handle(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Result<Option<LRESULT>> {
    match message {
        WM_PAINT => with(|snip| paint(snip))?,
        WM_ERASEBKGND => return Ok(Some(LRESULT(1))),
        WM_LBUTTONDOWN => with(|snip| {
            snip.anchor = Some(point(lparam));
            snip.cursor = point(lparam);
            unsafe { SetCapture(window) };
            invalidate(window)
        })?,
        WM_MOUSEMOVE => with(|snip| {
            let position = point(lparam);

            if let Some(last) = snip.last
                && was_recentered(snip, last, position)
            {
                snip.recenter = None;
                return unsafe { SetCursorPos(snip.shot.left + last.x, snip.shot.top + last.y) };
            }

            snip.last = Some(position);

            match snip.anchor {
                Some(_) => {
                    snip.cursor = position;
                    invalidate(window)
                }
                None => Ok(()),
            }
        })?,
        WM_LBUTTONUP => {
            let Some(area) = with(|snip| {
                snip.cursor = point(lparam);
                Ok(selection(snip))
            })?
            else {
                return Ok(Some(LRESULT(0)));
            };

            unsafe { ReleaseCapture()? };
            finish(area)?;
        }
        WM_RBUTTONUP => close()?,
        WM_KEYDOWN if wparam.0 as u16 == VK_ESCAPE.0 => close()?,
        WM_ACTIVATE if wparam.0 & 0xffff == WA_INACTIVE as usize => close()?,
        _ => return Ok(None),
    }

    Ok(Some(LRESULT(0)))
}

fn with<R>(f: impl FnOnce(&mut Snip) -> Result<R>) -> Result<R> {
    SNIP.with_borrow_mut(|snip| f(snip.as_mut().ok_or_else(|| Error::new(E_FAIL, "The screenshot window is not open"))?))
}

fn client_center(window: HWND) -> Result<Option<POINT>> {
    if !unsafe { IsWindow(Some(window)) }.as_bool() {
        return Ok(None);
    }

    let mut client = RECT::default();

    unsafe { GetClientRect(window, &mut client)? };

    let mut center = POINT { x: client.right / 2, y: client.bottom / 2 };

    unsafe { ClientToScreen(window, &mut center).ok()? };
    Ok(Some(center))
}

fn was_recentered(snip: &Snip, last: POINT, position: POINT) -> bool {
    let Some(center) = snip.recenter else {
        return false;
    };
    let (x, y) = (snip.shot.left + position.x, snip.shot.top + position.y);
    let jump = (position.x - last.x).abs().max((position.y - last.y).abs());

    snip.opened.elapsed() < RECENTER_WINDOW && (x - center.x).abs() <= 1 && (y - center.y).abs() <= 1 && jump > RECENTER_JUMP
}

fn point(lparam: LPARAM) -> POINT {
    POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 }
}

fn selection(snip: &Snip) -> Option<RECT> {
    let anchor = snip.anchor?;
    let clamp = |value: i32, limit: i32| value.clamp(0, limit);

    Some(RECT {
        left:   clamp(anchor.x.min(snip.cursor.x), snip.shot.width),
        top:    clamp(anchor.y.min(snip.cursor.y), snip.shot.height),
        right:  clamp(anchor.x.max(snip.cursor.x) + 1, snip.shot.width),
        bottom: clamp(anchor.y.max(snip.cursor.y) + 1, snip.shot.height),
    })
}

fn invalidate(window: HWND) -> Result<()> {
    unsafe { InvalidateRect(Some(window), None, false).ok() }
}

fn paint(snip: &Snip) -> Result<()> {
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let screen = BeginPaint(snip.window, &mut paint);
        let back = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, snip.shot.width, snip.shot.height);
        let previous = SelectObject(back, bitmap.into());
        let drawn = draw(snip, back).and_then(|()| BitBlt(screen, 0, 0, snip.shot.width, snip.shot.height, Some(back), 0, 0, SRCCOPY));

        SelectObject(back, previous);
        _ = DeleteObject(bitmap.into());
        _ = DeleteDC(back);
        EndPaint(snip.window, &paint).ok()?;
        drawn
    }
}

unsafe fn draw(snip: &Snip, target: HDC) -> Result<()> {
    unsafe {
        BitBlt(target, 0, 0, snip.shot.width, snip.shot.height, Some(snip.dim.dc), 0, 0, SRCCOPY)?;

        let Some(area) = selection(snip) else {
            return Ok(());
        };

        BitBlt(target, area.left, area.top, area.right - area.left, area.bottom - area.top, Some(snip.bright.dc), area.left, area.top, SRCCOPY)?;
        FrameRect(target, &area, HBRUSH(GetStockObject(WHITE_BRUSH).0));
        Ok(())
    }
}

fn close() -> Result<()> {
    let Some(snip) = SNIP.take() else {
        return Ok(());
    };

    unsafe {
        if IsWindow(Some(snip.previous)).as_bool() {
            _ = SetForegroundWindow(snip.previous);
        }

        DestroyWindow(snip.window)
    }
}

fn finish(area: RECT) -> Result<()> {
    let (shot, folder, width, height) = with(|snip| Ok((std::mem::take(&mut snip.shot.pixels), snip.folder.clone(), snip.shot.width, snip.shot.height)))?;

    close()?;

    let area = match area.right - area.left >= MIN_SELECTION && area.bottom - area.top >= MIN_SELECTION {
        true => area,
        false => RECT { left: 0, top: 0, right: width, bottom: height },
    };
    let (cropped_width, cropped_height) = (area.right - area.left, area.bottom - area.top);
    let row = cropped_width as usize * 4;
    let pixels: Vec<u8> = (area.top..area.bottom).flat_map(|line| shot[(line * width + area.left) as usize * 4..][..row].iter().copied()).collect();

    create_folder(&folder)?;

    let path = recording_path(&folder, "Screenshot ", "png");

    save_png(&path, cropped_width as u32, cropped_height as u32, &pixels)?;
    copy_to_clipboard(cropped_width, cropped_height, &pixels, &fs::read(&path).map_err(|e| Error::new(E_FAIL, format!("Cannot read {}: {e}", path.display())))?)
}

fn save_png(path: &Path, width: u32, height: u32, pixels: &[u8]) -> Result<()> {
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = factory.CreateStream()?;

        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0)?;

        let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        let mut frame = None;

        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;

        let frame = frame.ok_or_else(|| Error::new(E_FAIL, "The PNG encoder returned no frame"))?;
        let mut format = GUID_WICPixelFormat32bppBGRA;

        frame.Initialize(None)?;
        frame.SetSize(width, height)?;
        frame.SetPixelFormat(&mut format)?;

        if format != GUID_WICPixelFormat32bppBGRA {
            return Err(Error::new(E_FAIL, "The PNG encoder does not accept 32-bit pixels"));
        }

        frame.WritePixels(height, width * 4, pixels)?;
        frame.Commit()?;
        encoder.Commit()
    }
}

fn copy_to_clipboard(width: i32, height: i32, pixels: &[u8], png: &[u8]) -> Result<()> {
    let row = width as usize * 4;
    let header = bitmap_info(width, height).bmiHeader;
    let header_bytes = unsafe { std::slice::from_raw_parts((&header as *const BITMAPINFOHEADER).cast::<u8>(), size_of::<BITMAPINFOHEADER>()) };
    let dib: Vec<u8> = header_bytes.iter().copied().chain(pixels.chunks_exact(row).rev().flatten().copied()).collect();
    let png_format = unsafe { RegisterClipboardFormatW(w!("PNG")) };

    if png_format == 0 {
        return Err(Error::from_win32());
    }

    unsafe { OpenClipboard(None)? };

    let filled = unsafe { EmptyClipboard() }.and_then(|()| place(u32::from(CF_DIB.0), &dib)).and_then(|()| place(png_format, png));
    let closed = unsafe { CloseClipboard() };

    filled.and(closed)
}

fn place(format: u32, bytes: &[u8]) -> Result<()> {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
        let target = GlobalLock(memory);

        if target.is_null() {
            let error = Error::from_win32();

            _ = GlobalFree(Some(memory));
            return Err(error);
        }

        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast::<u8>(), bytes.len());

        if let Err(e) = GlobalUnlock(memory)
            && e.code().is_err()
        {
            _ = GlobalFree(Some(memory));
            return Err(e);
        }

        if let Err(e) = SetClipboardData(format, Some(HANDLE(memory.0))) {
            _ = GlobalFree(Some(memory));
            return Err(e);
        }

        Ok(())
    }
}

fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (width * height.abs() * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    }
}
