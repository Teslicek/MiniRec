use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Gdi::{ClientToScreen, GetMonitorInfoW, HDC, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromRect, MonitorFromWindow, ScreenToClient};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Error, HSTRING, PCWSTR, Result, w};

use crate::draw::*;
use crate::edit::{InlineEdit, WM_EDIT_DONE, edit_colors};
use crate::export::{Job, Keyframes, Mode, Plan, Target, WM_EXPORT_DONE, is_cancelled, output_path};
use crate::openwith::is_video;
use crate::player::{Player, PlayerEvent, WM_PLAYER_EVENT, decode_event};
use crate::settings::{CompressSettings, PlayerSettings, QUALITY_PRESETS, SIZE_PRESETS};
use crate::shell::{VIDEO_CLASS, WM_MOUSELEAVE, dark_title, error_box, fit_client, high_word, low_word, mouse_point, pick_video, place_child, popup_menu, reveal};
use crate::units::{format_bytes, format_megabytes, format_time, parse_quality, parse_size, parse_time};

const CLASS: PCWSTR = w!("MiniRecTrim");
const BAR_CLASS: PCWSTR = w!("MiniRecBar");
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0);
const DEFAULT_WIDTH: f32 = 1280.0;
const DEFAULT_HEIGHT: f32 = 800.0;
const MIN_WIDTH: f32 = 480.0;
const MIN_HEIGHT: f32 = 300.0;
const MIN_EDIT_VIDEO: f32 = 200.0;
const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 33;
const HIDE_TIMER_ID: usize = 2;
const HIDE_MS: u32 = 2500;
const CLICK_TIMER_ID: usize = 3;
const TICKS: f64 = 10_000_000.0;
const VISIBLE_ROWS: usize = 3;
const SEEK_STEP: f64 = 5.0;
const VOLUME_STEP: f64 = 0.05;
const SPEEDS: [f64; 7] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
const BAR_HEIGHT: f32 = 60.0;
const PLAY_GLYPH: &str = "\u{e768}";
const PAUSE_GLYPH: &str = "\u{e769}";
const PREVIOUS_GLYPH: &str = "\u{e892}";
const NEXT_GLYPH: &str = "\u{e893}";
const BACK_GLYPH: &str = "\u{eb9e}";
const FORWARD_GLYPH: &str = "\u{eb9d}";
const VOLUME_GLYPH: &str = "\u{e767}";
const MUTE_GLYPH: &str = "\u{e74f}";
const EDIT_GLYPH: &str = "\u{e70f}";
const FULLSCREEN_GLYPH: &str = "\u{e740}";
const WINDOWED_GLYPH: &str = "\u{e73f}";
const BAR_ALPHA: u8 = 235;

const PANEL_WIDTH: f32 = 960.0;
const PANEL_HEIGHT: f32 = 248.0;
const SEGMENT_COLOR: u32 = 0xe8a33d;
const LEAD_IN_COLOR: u32 = 0x6b4f22;
const STEP_BACK: D2D_RECT_F = rect(20.0, 12.0, 68.0, 44.0);
const STEP_FORWARD: D2D_RECT_F = rect(74.0, 12.0, 122.0, 44.0);
const SET_IN: D2D_RECT_F = rect(146.0, 12.0, 240.0, 44.0);
const SET_OUT: D2D_RECT_F = rect(246.0, 12.0, 346.0, 44.0);
const ADD: D2D_RECT_F = rect(352.0, 12.0, 484.0, 44.0);
const IN_OUT: D2D_RECT_F = rect(504.0, 12.0, 940.0, 44.0);
const LIST_TOP: f32 = 80.0;
const ROW_STEP: f32 = 36.0;
const ROW_HEIGHT: f32 = 32.0;
const MODES: D2D_RECT_F = rect(20.0, 200.0, 404.0, 236.0);
const LOSSLESS: D2D_RECT_F = rect(24.0, 204.0, 148.0, 232.0);
const EXACT: D2D_RECT_F = rect(148.0, 204.0, 272.0, 232.0);
const COMPRESSED: D2D_RECT_F = rect(276.0, 204.0, 400.0, 232.0);
const TARGET_VALUE: D2D_RECT_F = rect(416.0, 204.0, 476.0, 232.0);
const TARGET: D2D_RECT_F = rect(482.0, 200.0, 592.0, 236.0);
const STATUS_LEFT: f32 = 420.0;
const STATUS_LEFT_WITH_TARGET: f32 = 604.0;
const STATUS_RIGHT: f32 = 764.0;
const QUALITY_MENU: usize = QUALITY_PRESETS.len();
const EXPORT: D2D_RECT_F = rect(780.0, 200.0, 940.0, 236.0);

#[derive(Clone, Copy, PartialEq)]
enum Item {
    Video,
    Open,
    Fullscreen,
    Play,
    StepBack,
    StepForward,
    SetIn,
    SetOut,
    Add,
    Start(usize),
    End(usize),
    Remove(usize),
    Lossless,
    Exact,
    Compressed,
    TargetValue,
    Target,
    Export,
    Status,
}

#[derive(Clone, Copy, PartialEq)]
enum BarItem {
    Seek,
    Previous,
    Back,
    Play,
    Forward,
    Next,
    Volume,
    Mute,
    Speed,
    Edit,
    Fullscreen,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Player,
    Edit,
}

#[derive(Clone, Copy, PartialEq)]
enum Output {
    Lossless,
    Exact,
    Compressed,
}

#[derive(Clone, Copy)]
struct Segment {
    start:    f64,
    end:      f64,
    keyframe: i64,
}

impl Segment {
    fn keyframe_time(self) -> f64 {
        self.keyframe as f64 / TICKS
    }
}

enum Status {
    Hint,
    Info(String),
    Saved(PathBuf),
}

struct BarLayout {
    seek:         D2D_RECT_F,
    track:        D2D_RECT_F,
    previous:     D2D_RECT_F,
    back:         D2D_RECT_F,
    play:         D2D_RECT_F,
    forward:      D2D_RECT_F,
    next:         D2D_RECT_F,
    time:         D2D_RECT_F,
    volume:       D2D_RECT_F,
    volume_track: D2D_RECT_F,
    mute:         D2D_RECT_F,
    speed:        D2D_RECT_F,
    edit:         D2D_RECT_F,
    fullscreen:   D2D_RECT_F,
}

struct Trim {
    window:         HWND,
    video:          HWND,
    bar:            HWND,
    canvas:         Canvas,
    bar_canvas:     Canvas,
    player:         Player,
    settings:       PlayerSettings,
    view:           View,
    client:         (f32, f32),
    bar_width:      f32,
    folder:         PathBuf,
    file:           Option<PathBuf>,
    duration:       f64,
    playing:        bool,
    volume:         f64,
    muted:          bool,
    speed:          f64,
    keyframes:      Option<Keyframes>,
    in_point:       Option<f64>,
    out_point:      Option<f64>,
    segments:       Vec<Segment>,
    mode:           Output,
    target:         CompressSettings,
    job:            Option<Job>,
    status:         Status,
    hover:          Option<Item>,
    pressed:        Option<Item>,
    tracking_mouse: bool,
    bar_hover:      Option<BarItem>,
    bar_pressed:    Option<BarItem>,
    bar_drag:       Option<BarItem>,
    bar_tracking:   bool,
    controls:       bool,
    last_mouse:     POINT,
    menu_open:      bool,
    click_pending:  bool,
    timer_on:       bool,
    scroll:         usize,
    edit:           Option<InlineEdit<Item>>,
    hand:           HCURSOR,
    arrow:          HCURSOR,
    windowed:       Option<RECT>,
    placed:         RECT,
    moving:         bool,
}

thread_local! {
    static TRIM: RefCell<Option<Trim>> = const { RefCell::new(None) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
    static STANDALONE: Cell<bool> = const { Cell::new(false) };
}

pub fn open_standalone(video: &Path) -> Result<()> {
    let folder = video.parent().ok_or_else(|| Error::new(E_FAIL, format!("{} has no folder", video.display())))?;

    STANDALONE.set(true);
    create(folder, View::Player)?;

    let window = with_trim(|t| Ok(t.window))?;

    load(window, video.to_path_buf())
}

pub fn open(folder: &Path) -> Result<()> {
    create(folder, View::Edit)
}

fn create(folder: &Path, view: View) -> Result<()> {
    if let Some(window) = TRIM.with_borrow(|trim| trim.as_ref().map(|t| t.window)) {
        unsafe {
            _ = ShowWindow(window, if IsIconic(window).as_bool() { SW_RESTORE } else { SW_SHOW });
            _ = SetForegroundWindow(window);
        }

        return Ok(());
    }

    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();

        register_classes(instance)?;

        let window = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("MiniRec"), STYLE, CW_USEDEFAULT, CW_USEDEFAULT, 1280, 800, None, None, Some(instance), None)?;

        dark_title(window)?;

        let video = CreateWindowExW(WINDOW_EX_STYLE(0), VIDEO_CLASS, None, WS_CHILD, 0, 0, 0, 0, Some(window), None, Some(instance), None)?;
        let bar = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED, BAR_CLASS, None, WS_POPUP, 0, 0, 0, 0, Some(window), None, Some(instance), None)?;

        SetLayeredWindowAttributes(bar, COLORREF(0), BAR_ALPHA, LWA_ALPHA)?;
        DragAcceptFiles(window, true);

        let dpi = GetDpiForWindow(window);
        let settings = PlayerSettings::load()?;
        let player = Player::new(video, window)?;

        player.set_volume(settings.volume)?;
        player.set_muted(settings.muted)?;

        let arrow = LoadCursorW(None, IDC_ARROW)?;
        let trim = Trim {
            window,
            video,
            bar,
            canvas: Canvas::new(dpi)?,
            bar_canvas: Canvas::new(dpi)?,
            player,
            settings,
            view,
            client: (0.0, 0.0),
            bar_width: 0.0,
            folder: folder.to_path_buf(),
            file: None,
            duration: 0.0,
            playing: false,
            volume: settings.volume,
            muted: settings.muted,
            speed: 1.0,
            keyframes: None,
            in_point: None,
            out_point: None,
            segments: Vec::new(),
            mode: Output::Lossless,
            target: CompressSettings::load()?,
            job: None,
            status: Status::Hint,
            hover: None,
            pressed: None,
            tracking_mouse: false,
            bar_hover: None,
            bar_pressed: None,
            bar_drag: None,
            bar_tracking: false,
            controls: true,
            last_mouse: POINT::default(),
            menu_open: false,
            click_pending: false,
            timer_on: false,
            scroll: 0,
            edit: None,
            hand: LoadCursorW(None, IDC_HAND)?,
            arrow,
            windowed: None,
            placed: RECT::default(),
            moving: false,
        };

        TRIM.set(Some(trim));
        place_window(window, dpi, settings)?;
        _ = SetForegroundWindow(window);
        layout(window)
    }
}

fn register_classes(instance: HINSTANCE) -> Result<()> {
    if REGISTERED.get() {
        return Ok(());
    }

    unsafe {
        let arrow = LoadCursorW(None, IDC_ARROW)?;
        let main = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hIcon: LoadIconW(Some(instance), PCWSTR(1 as *const u16))?,
            hCursor: arrow,
            lpszClassName: CLASS,
            ..Default::default()
        };
        let bar = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(bar_proc),
            hInstance: instance,
            hCursor: arrow,
            lpszClassName: BAR_CLASS,
            ..Default::default()
        };

        if RegisterClassExW(&main) == 0 || RegisterClassExW(&bar) == 0 {
            return Err(Error::from_win32());
        }
    }

    REGISTERED.set(true);
    Ok(())
}

fn place_window(window: HWND, dpi: u32, settings: PlayerSettings) -> Result<()> {
    let saved = settings
        .window
        .map(|[left, top, right, bottom]| RECT { left, top, right, bottom })
        .filter(|frame| !unsafe { MonitorFromRect(frame, MONITOR_DEFAULTTONULL) }.is_invalid());

    let Some(frame) = saved else {
        fit_client(window, dpi, DEFAULT_WIDTH, DEFAULT_HEIGHT, STYLE)?;
        unsafe { _ = ShowWindow(window, SW_SHOW) };
        return Ok(());
    };
    let show = if settings.maximized { SW_SHOWMAXIMIZED } else { SW_SHOWNORMAL };
    let placement = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, showCmd: show.0 as u32, rcNormalPosition: frame, ..Default::default() };

    unsafe { SetWindowPlacement(window, &placement) }
}

pub fn close() -> Result<()> {
    let Some(trim) = TRIM.take() else {
        return Ok(());
    };
    let Trim { window, player, job, edit, mut settings, windowed, volume, muted, .. } = trim;
    let export = job.map(|job| {
        job.cancel();
        job.finish()
    });

    settings.volume = volume;
    settings.muted = muted;

    if windowed.is_none() {
        let mut placement = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };

        unsafe { GetWindowPlacement(window, &mut placement)? };

        let frame = placement.rcNormalPosition;

        settings.window = Some([frame.left, frame.top, frame.right, frame.bottom]);
        settings.maximized = placement.showCmd == SW_SHOWMAXIMIZED.0 as u32;
    }

    let saved = settings.save();

    if let Some(edit) = edit {
        edit.close()?;
    }

    player.shutdown()?;
    unsafe { DestroyWindow(window)? };

    if STANDALONE.get() {
        unsafe { PostQuitMessage(0) };
    }

    saved?;

    match export {
        Some(Err(e)) if !is_cancelled(&e) => Err(e),
        _ => Ok(()),
    }
}

extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    dispatch(window, message, wparam, lparam, handle)
}

extern "system" fn bar_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    dispatch(window, message, wparam, lparam, handle_bar)
}

fn dispatch(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, handler: fn(HWND, u32, WPARAM, LPARAM) -> Result<Option<LRESULT>>) -> LRESULT {
    let ready = TRIM.with(|cell| matches!(cell.try_borrow().as_deref(), Ok(Some(_))));

    if !ready {
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    }

    match handler(window, message, wparam, lparam) {
        Ok(Some(result)) => result,
        Ok(None) => unsafe { DefWindowProcW(window, message, wparam, lparam) },
        Err(e) => {
            error_box(Some(window), &e);
            LRESULT(0)
        }
    }
}

fn with_trim<R>(f: impl FnOnce(&mut Trim) -> Result<R>) -> Result<R> {
    TRIM.with_borrow_mut(|trim| f(trim.as_mut().ok_or_else(|| Error::new(E_FAIL, "Player window is not open"))?))
}

fn handle(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Result<Option<LRESULT>> {
    match message {
        WM_PAINT => with_trim(|t| t.canvas.paint(window, |p| draw(p, t)))?,
        WM_ERASEBKGND => return Ok(Some(LRESULT(1))),
        WM_SIZE => {
            with_trim(|t| {
                let (width, height) = (low_word(lparam.0), high_word(lparam.0));

                t.canvas.resize(width, height)?;
                t.client = (width as f32 / t.canvas.scale(), height as f32 / t.canvas.scale());
                t.bar_width = t.client.0;
                Ok(())
            })?;
            layout(window)?;
        }
        WM_MOVE => layout(window)?,
        WM_ENTERSIZEMOVE => with_trim(|t| {
            t.moving = true;
            unsafe { _ = ShowWindow(t.bar, SW_HIDE) };
            Ok(())
        })?,
        WM_EXITSIZEMOVE => {
            with_trim(|t| {
                t.moving = false;
                Ok(())
            })?;
            layout(window)?;
        }
        WM_GETMINMAXINFO => {
            let (width, height, scale) = with_trim(|t| {
                Ok(match t.view {
                    View::Edit => (PANEL_WIDTH, PANEL_HEIGHT + BAR_HEIGHT + MIN_EDIT_VIDEO, t.canvas.scale()),
                    View::Player => (MIN_WIDTH, MIN_HEIGHT, t.canvas.scale()),
                })
            })?;
            let (min_width, min_height) = frame_size(width, height, scale)?;
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };

            info.ptMinTrackSize = POINT { x: min_width, y: min_height };
        }
        WM_DPICHANGED => {
            let suggested = unsafe { *(lparam.0 as *const RECT) };
            let dpi = high_word(wparam.0 as isize);
            let fullscreen = with_trim(|t| {
                t.canvas.set_dpi(dpi);
                t.bar_canvas.set_dpi(dpi);
                Ok(t.fullscreen())
            })?;

            if !fullscreen {
                unsafe { SetWindowPos(window, None, suggested.left, suggested.top, suggested.right - suggested.left, suggested.bottom - suggested.top, SWP_NOZORDER | SWP_NOACTIVATE)? };
            }
        }
        WM_CLOSE => close()?,
        WM_CTLCOLOREDIT => return Ok(Some(edit_colors(HDC(wparam.0 as *mut _)))),
        WM_EDIT_DONE => finish_edit(wparam.0 == 1)?,
        WM_DROPFILES => drop_files(window, HDROP(wparam.0 as *mut _))?,
        WM_MOUSEMOVE => {
            poke(window)?;

            let (x, y, docked) = with_trim(|t| {
                if !t.tracking_mouse {
                    let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: window, dwHoverTime: 0 };

                    unsafe { TrackMouseEvent(&mut track)? };
                    t.tracking_mouse = true;
                }

                let (x, y) = mouse_point(lparam.0, t.canvas.scale());

                Ok((x, y, t.docked_point(x, y)))
            })?;

            match docked {
                Some((bar_x, bar_y)) => {
                    with_trim(|t| match t.hover.take() {
                        Some(_) => t.invalidate(),
                        None => Ok(()),
                    })?;
                    bar_move(bar_x, bar_y)?;
                }
                None => with_trim(|t| {
                    if t.bar_hover.take().is_some() {
                        t.invalidate_bar()?;
                    }

                    let hover = t.hit(x, y);

                    if hover != t.hover {
                        t.hover = hover;
                        t.invalidate()?;
                    }

                    Ok(())
                })?,
            }
        }
        WM_MOUSELEAVE => with_trim(|t| {
            t.tracking_mouse = false;
            t.hover = None;

            if t.editing() {
                t.bar_hover = None;
            }

            t.invalidate()
        })?,
        WM_SETCURSOR if low_word(lparam.0) == HTCLIENT => {
            let cursor = with_trim(|t| {
                Ok(match (t.editing() && t.bar_hover.is_some(), t.hover) {
                    (true, _) => Some(Some(t.hand)),
                    (false, Some(Item::Video)) if t.cursor_hidden() => Some(None),
                    (false, Some(Item::Video) | None) => None,
                    (false, Some(item)) if t.enabled(item) => Some(Some(t.hand)),
                    (false, Some(_)) => None,
                })
            })?;
            let Some(cursor) = cursor else {
                return Ok(None);
            };

            unsafe { SetCursor(cursor) };
            return Ok(Some(LRESULT(1)));
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            unsafe { _ = SetFocus(Some(window)) };

            let (docked, video, item) = with_trim(|t| {
                let (x, y) = mouse_point(lparam.0, t.canvas.scale());
                let item = t.hit(x, y).filter(|&item| t.enabled(item));
                let video = message == WM_LBUTTONDBLCLK && item == Some(Item::Video);

                if video && std::mem::take(&mut t.click_pending) {
                    unsafe { KillTimer(Some(window), CLICK_TIMER_ID)? };
                }

                Ok((t.docked_point(x, y), video, item))
            })?;

            match (docked, video) {
                (Some((bar_x, bar_y)), _) => bar_down(window, bar_x, bar_y)?,
                (None, true) => toggle_fullscreen(window)?,
                (None, false) => with_trim(|t| {
                    t.pressed = item;
                    Ok(())
                })?,
            }
        }
        WM_LBUTTONUP => {
            let (docked, released, pressed) = with_trim(|t| {
                let (x, y) = mouse_point(lparam.0, t.canvas.scale());

                Ok((t.docked_point(x, y), t.hit(x, y), t.pressed.take()))
            })?;

            match (docked, released.filter(|&item| Some(item) == pressed)) {
                (Some((bar_x, bar_y)), _) => bar_up(bar_x, bar_y)?,
                (None, Some(Item::Video)) => with_trim(|t| {
                    if unsafe { SetTimer(Some(window), CLICK_TIMER_ID, GetDoubleClickTime(), None) } == 0 {
                        return Err(Error::from_win32());
                    }

                    t.click_pending = true;
                    Ok(())
                })?,
                (None, Some(item)) => activate(window, item)?,
                (None, None) => {}
            }
        }
        WM_MOUSEWHEEL => {
            let delta = high_word(wparam.0 as isize) as u16 as i16;
            let mut point = POINT { x: 0, y: ((lparam.0 >> 16) & 0xffff) as u16 as i16 as i32 };

            unsafe { ScreenToClient(window, &mut point).ok()? };
            with_trim(|t| {
                if !t.editing() || (point.y as f32 / t.canvas.scale()) < t.panel_offset().1 {
                    return t.change_volume(if delta > 0 { VOLUME_STEP } else { -VOLUME_STEP });
                }

                let max = t.segments.len().saturating_sub(VISIBLE_ROWS);

                t.scroll = match delta {
                    d if d < 0 => (t.scroll + 1).min(max),
                    _ => t.scroll.saturating_sub(1),
                };

                t.invalidate()
            })?;
        }
        WM_KEYDOWN => key_down(window, VIRTUAL_KEY(wparam.0 as u16))?,
        WM_TIMER if wparam.0 == HIDE_TIMER_ID => hide_controls(window)?,
        WM_TIMER if wparam.0 == CLICK_TIMER_ID => {
            with_trim(|t| {
                t.click_pending = false;
                unsafe { KillTimer(Some(window), CLICK_TIMER_ID) }
            })?;
            run(window, Item::Play)?;
        }
        WM_TIMER => with_trim(|t| {
            t.invalidate()?;
            t.invalidate_bar()
        })?,
        WM_PLAYER_EVENT => player_event(window, wparam, lparam)?,
        WM_EXPORT_DONE => with_trim(|t| {
            let Some(job) = t.job.take() else {
                return Ok(());
            };

            t.update_timer()?;
            t.invalidate()?;

            match job.finish() {
                Ok(path) => t.status = Status::Saved(path),
                Err(e) if is_cancelled(&e) => t.status = Status::Info("Export cancelled.".to_string()),
                Err(e) => {
                    t.status = Status::Info("Export failed.".to_string());
                    return Err(e);
                }
            }

            Ok(())
        })?,
        _ => return Ok(None),
    }

    Ok(Some(LRESULT(0)))
}

fn handle_bar(bar: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Result<Option<LRESULT>> {
    match message {
        WM_PAINT => with_trim(|t| t.bar_canvas.paint(bar, |p| draw_bar(p, t)))?,
        WM_ERASEBKGND => return Ok(Some(LRESULT(1))),
        WM_MOUSEACTIVATE => return Ok(Some(LRESULT(MA_NOACTIVATE as isize))),
        WM_DPICHANGED => {}
        WM_SIZE => with_trim(|t| t.bar_canvas.resize(low_word(lparam.0), high_word(lparam.0)))?,
        WM_MOUSEMOVE => {
            let window = with_trim(|t| Ok(t.window))?;

            poke(window)?;

            let (x, y) = with_trim(|t| {
                if !t.bar_tracking {
                    let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: bar, dwHoverTime: 0 };

                    unsafe { TrackMouseEvent(&mut track)? };
                    t.bar_tracking = true;
                }

                Ok(mouse_point(lparam.0, t.bar_canvas.scale()))
            })?;

            bar_move(x, y)?;
        }
        WM_MOUSELEAVE => with_trim(|t| {
            t.bar_tracking = false;
            t.bar_hover = None;
            t.invalidate_bar()
        })?,
        WM_SETCURSOR if low_word(lparam.0) == HTCLIENT => {
            let hand = with_trim(|t| Ok(t.bar_hover.map(|_| t.hand)))?;
            let Some(cursor) = hand else {
                return Ok(None);
            };

            unsafe { SetCursor(Some(cursor)) };
            return Ok(Some(LRESULT(1)));
        }
        WM_LBUTTONDOWN => {
            let (x, y) = with_trim(|t| Ok(mouse_point(lparam.0, t.bar_canvas.scale())))?;

            bar_down(bar, x, y)?;
        }
        WM_LBUTTONUP => {
            let (x, y) = with_trim(|t| Ok(mouse_point(lparam.0, t.bar_canvas.scale())))?;

            bar_up(x, y)?;
        }
        WM_MOUSEWHEEL => with_trim(|t| t.change_volume(if high_word(wparam.0 as isize) as u16 as i16 > 0 { VOLUME_STEP } else { -VOLUME_STEP }))?,
        _ => return Ok(None),
    }

    Ok(Some(LRESULT(0)))
}

fn bar_down(capture: HWND, x: f32, y: f32) -> Result<()> {
    with_trim(|t| {
        let item = bar_hit(t.bar_width, x, y);

        match item {
            Some(BarItem::Seek | BarItem::Volume) if t.loaded() => {
                t.bar_drag = item;
                unsafe { SetCapture(capture) };
                t.drag_bar(x)
            }
            _ => {
                t.bar_pressed = item;
                Ok(())
            }
        }
    })
}

fn bar_move(x: f32, y: f32) -> Result<()> {
    with_trim(|t| {
        t.drag_bar(x)?;

        let hover = bar_hit(t.bar_width, x, y);

        if hover != t.bar_hover || t.bar_drag.is_some() {
            t.bar_hover = hover;
            t.invalidate_bar()?;
        }

        Ok(())
    })
}

fn bar_up(x: f32, y: f32) -> Result<()> {
    let (drag, released, pressed) = with_trim(|t| Ok((t.bar_drag.take(), bar_hit(t.bar_width, x, y), t.bar_pressed.take())))?;

    if let Some(drag) = drag {
        unsafe { ReleaseCapture()? };

        return match drag {
            BarItem::Seek => with_trim(|t| {
                t.player.seek(t.bar_time(x), true)?;
                t.invalidate_bar()
            }),
            _ => Ok(()),
        };
    }

    match released.filter(|&item| Some(item) == pressed) {
        Some(item) => bar_activate(item),
        None => Ok(()),
    }
}

fn player_event(window: HWND, wparam: WPARAM, lparam: LPARAM) -> Result<()> {
    let (relayout, loaded, failure) = with_trim(|t| {
        let mut relayout = false;
        let mut loaded = false;
        let mut failure = None;

        match decode_event(wparam, lparam) {
            PlayerEvent::Loaded => {
                t.duration = t.player.duration();
                t.player.set_rate(t.speed)?;
                t.placed = RECT::default();
                relayout = true;
                loaded = true;
            }
            PlayerEvent::Playing => {
                t.playing = true;
                t.arm_hide()?;
            }
            PlayerEvent::Stopped => {
                t.playing = false;
                relayout = !t.controls;
                t.controls = true;
            }
            PlayerEvent::Failed(e) => {
                t.file = None;
                t.keyframes = None;
                t.duration = 0.0;
                relayout = true;
                failure = Some(e);
            }
            PlayerEvent::Other => {}
        }

        t.update_timer()?;
        t.invalidate()?;
        t.invalidate_bar()?;
        Ok((relayout, loaded, failure))
    })?;

    if relayout {
        layout(window)?;
    }

    if loaded {
        with_trim(|t| t.player.attach())?;
    }

    match failure {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn layout(window: HWND) -> Result<()> {
    if unsafe { IsIconic(window) }.as_bool() {
        return Ok(());
    }

    let (video, bar, area, moved, has_file, bar_rect) = with_trim(|t| {
        let scale = t.canvas.scale();
        let area = to_pixels(t.video_rect(), scale);
        let height = (BAR_HEIGHT * scale).round() as i32;
        let mut corner = POINT { x: 0, y: area.bottom - height };

        unsafe { ClientToScreen(window, &mut corner).ok()? };

        let bar_rect = (t.bar_wanted() && !t.moving && area.bottom > height).then_some(RECT { left: corner.x, top: corner.y, right: corner.x + area.right, bottom: corner.y + height });
        let moved = area != t.placed;

        t.placed = area;
        Ok((t.video, t.bar, area, moved, t.file.is_some(), bar_rect))
    })?;

    if moved {
        place_child(video, area)?;
    }

    unsafe { _ = ShowWindow(video, if has_file { SW_SHOWNA } else { SW_HIDE }) };

    if moved {
        with_trim(|t| {
            if t.loaded() && area.right > area.left && area.bottom > area.top {
                t.player.resize(area)?;

                if !t.playing {
                    t.player.seek(t.player.time(), true)?;
                }
            }

            t.invalidate()
        })?;
    }

    match bar_rect {
        Some(frame) => unsafe { SetWindowPos(bar, None, frame.left, frame.top, frame.right - frame.left, frame.bottom - frame.top, SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW)? },
        None => unsafe { _ = ShowWindow(bar, SW_HIDE) },
    }

    with_trim(|t| t.invalidate_bar())
}

fn poke(window: HWND) -> Result<()> {
    let mut point = POINT::default();

    unsafe { GetCursorPos(&mut point)? };

    let shown = with_trim(|t| {
        if t.last_mouse == point {
            return Ok(false);
        }

        t.last_mouse = point;
        t.arm_hide()?;

        let shown = !t.controls;

        if shown {
            t.controls = true;
            unsafe { SetCursor(Some(t.arrow)) };
        }

        Ok(shown)
    })?;

    match shown {
        true => layout(window),
        false => Ok(()),
    }
}

fn hide_controls(window: HWND) -> Result<()> {
    let hidden = with_trim(|t| {
        unsafe { KillTimer(Some(window), HIDE_TIMER_ID)? };

        if !t.playing || t.menu_open || t.bar_drag.is_some() || !t.controls || cursor_over(t.bar)? {
            return Ok(false);
        }

        t.controls = false;
        Ok(true)
    })?;

    if !hidden {
        return Ok(());
    }

    layout(window)?;

    if with_trim(|t| Ok(t.cursor_hidden() && t.hover == Some(Item::Video)))? && cursor_over(window)? {
        unsafe { SetCursor(None) };
    }

    Ok(())
}

fn cursor_over(window: HWND) -> Result<bool> {
    if !unsafe { IsWindowVisible(window) }.as_bool() {
        return Ok(false);
    }

    let mut point = POINT::default();
    let mut frame = RECT::default();

    unsafe {
        GetCursorPos(&mut point)?;
        GetWindowRect(window, &mut frame)?;
    }

    Ok(point.x >= frame.left && point.x < frame.right && point.y >= frame.top && point.y < frame.bottom)
}

fn frame_size(width: f32, height: f32, scale: f32) -> Result<(i32, i32)> {
    let mut frame = RECT { left: 0, top: 0, right: (width * scale).round() as i32, bottom: (height * scale).round() as i32 };

    unsafe { AdjustWindowRectExForDpi(&mut frame, STYLE, false, WINDOW_EX_STYLE(0), (scale * 96.0).round() as u32)? };
    Ok((frame.right - frame.left, frame.bottom - frame.top))
}

fn key_down(window: HWND, key: VIRTUAL_KEY) -> Result<()> {
    let held = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) } < 0;
    let (editing, fullscreen) = with_trim(|t| Ok((t.editing(), t.fullscreen())))?;

    match key {
        VK_SPACE | VK_K => run(window, Item::Play),
        VK_LEFT if held(VK_SHIFT) => run(window, Item::StepBack),
        VK_RIGHT if held(VK_SHIFT) => run(window, Item::StepForward),
        VK_LEFT => with_trim(|t| t.seek_by(-SEEK_STEP)),
        VK_RIGHT => with_trim(|t| t.seek_by(SEEK_STEP)),
        VK_UP => with_trim(|t| t.change_volume(VOLUME_STEP)),
        VK_DOWN => with_trim(|t| t.change_volume(-VOLUME_STEP)),
        VK_M => with_trim(|t| t.toggle_mute()),
        VK_F | VK_F11 => run(window, Item::Fullscreen),
        VK_ESCAPE if fullscreen => toggle_fullscreen(window),
        VK_E => toggle_view(window),
        VK_N => sibling(window, 1),
        VK_P => sibling(window, -1),
        VK_O if held(VK_CONTROL) => run(window, Item::Open),
        VK_I if editing => run(window, Item::SetIn),
        VK_O if editing => run(window, Item::SetOut),
        VK_RETURN if editing => run(window, Item::Add),
        _ => Ok(()),
    }
}

fn run(window: HWND, item: Item) -> Result<()> {
    match with_trim(|t| Ok(t.enabled(item)))? {
        true => activate(window, item),
        false => Ok(()),
    }
}

impl Trim {
    fn invalidate(&self) -> Result<()> {
        unsafe { InvalidateRect(Some(self.window), None, false).ok() }
    }

    fn invalidate_bar(&self) -> Result<()> {
        let target = if self.editing() { self.window } else { self.bar };

        unsafe { InvalidateRect(Some(target), None, false).ok() }
    }

    fn update_timer(&mut self) -> Result<()> {
        let wanted = self.playing || self.job.is_some();

        unsafe {
            match (wanted, self.timer_on) {
                (true, false) => {
                    if SetTimer(Some(self.window), TIMER_ID, TIMER_MS, None) == 0 {
                        return Err(Error::from_win32());
                    }
                }
                (false, true) => KillTimer(Some(self.window), TIMER_ID)?,
                _ => {}
            }
        }

        self.timer_on = wanted;
        Ok(())
    }

    fn arm_hide(&self) -> Result<()> {
        if unsafe { SetTimer(Some(self.window), HIDE_TIMER_ID, HIDE_MS, None) } == 0 {
            return Err(Error::from_win32());
        }

        Ok(())
    }

    fn loaded(&self) -> bool {
        self.file.is_some() && self.duration > 0.0
    }

    fn fullscreen(&self) -> bool {
        self.windowed.is_some()
    }

    fn editing(&self) -> bool {
        self.view == View::Edit && !self.fullscreen()
    }

    fn bar_wanted(&self) -> bool {
        self.loaded() && self.controls && !self.editing()
    }

    fn cursor_hidden(&self) -> bool {
        self.loaded() && self.playing && !self.controls && !self.editing()
    }

    fn video_rect(&self) -> D2D_RECT_F {
        let (width, height) = self.client;

        match self.editing() {
            true => rect(0.0, 0.0, width, (height - PANEL_HEIGHT - BAR_HEIGHT).max(0.0)),
            false => rect(0.0, 0.0, width, height),
        }
    }

    fn panel_offset(&self) -> (f32, f32) {
        let (width, height) = self.client;

        (((width - PANEL_WIDTH) / 2.0).max(0.0), (height - PANEL_HEIGHT).max(BAR_HEIGHT))
    }

    fn bar_top(&self) -> f32 {
        match self.editing() {
            true => self.video_rect().bottom,
            false => self.video_rect().bottom - BAR_HEIGHT,
        }
    }

    fn docked_point(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        if !self.editing() || !self.loaded() {
            return None;
        }

        let top = self.bar_top();
        let inside = y >= top && y < top + BAR_HEIGHT;

        (inside || self.bar_drag.is_some() || self.bar_pressed.is_some()).then_some((x, y - top))
    }

    fn panel_point(&self, x: f32, y: f32) -> (f32, f32) {
        let (dx, dy) = self.panel_offset();

        (x - dx, y - dy)
    }

    fn to_screen_panel(&self, r: D2D_RECT_F) -> D2D_RECT_F {
        let (dx, dy) = self.panel_offset();

        rect(r.left + dx, r.top + dy, r.right + dx, r.bottom + dy)
    }

    fn prompt_rect(&self) -> D2D_RECT_F {
        let video = self.video_rect();
        let (cx, cy) = ((video.left + video.right) / 2.0, (video.top + video.bottom) / 2.0);

        rect(cx - 80.0, cy - 20.0, cx + 80.0, cy + 20.0)
    }

    fn enabled(&self, item: Item) -> bool {
        let idle = self.job.is_none();

        match item {
            Item::Video => self.loaded(),
            Item::Open | Item::Lossless | Item::Exact | Item::Compressed => idle,
            Item::Target | Item::TargetValue => idle && self.mode == Output::Compressed,
            Item::Fullscreen => self.loaded(),
            Item::Export | Item::Status => true,
            Item::Play | Item::StepBack | Item::StepForward | Item::SetIn | Item::SetOut => self.loaded(),
            Item::Add | Item::Start(_) | Item::End(_) | Item::Remove(_) => self.loaded() && idle,
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Item> {
        if self.file.is_none() && contains(self.prompt_rect(), x, y) {
            return Some(Item::Open);
        }

        if contains(self.video_rect(), x, y) {
            return self.loaded().then_some(Item::Video);
        }

        if !self.editing() {
            return None;
        }

        let (x, y) = self.panel_point(x, y);
        let fixed = [
            (STEP_BACK, Item::StepBack),
            (STEP_FORWARD, Item::StepForward),
            (SET_IN, Item::SetIn),
            (SET_OUT, Item::SetOut),
            (ADD, Item::Add),
            (LOSSLESS, Item::Lossless),
            (EXACT, Item::Exact),
            (COMPRESSED, Item::Compressed),
            (EXPORT, Item::Export),
            (self.status_rect(), Item::Status),
            (TARGET, Item::Target),
            (TARGET_VALUE, Item::TargetValue),
        ];

        if let Some((_, item)) = fixed.iter().find(|(r, _)| contains(*r, x, y)) {
            return Some(*item);
        }

        self.visible_rows().find_map(|(index, row)| {
            [(start_field(row), Item::Start(index)), (end_field(row), Item::End(index)), (remove_button(row), Item::Remove(index))]
                .into_iter()
                .find(|(r, _)| contains(*r, x, y))
                .map(|(_, item)| item)
        })
    }

    fn visible_rows(&self) -> impl Iterator<Item = (usize, D2D_RECT_F)> {
        (self.scroll..self.segments.len().min(self.scroll + VISIBLE_ROWS)).map(move |index| (index, row_rect(index - self.scroll)))
    }

    fn bar_x(&self, time: f64) -> f32 {
        let track = bar_layout(self.bar_width).track;

        track.left + (time / self.duration).clamp(0.0, 1.0) as f32 * (track.right - track.left)
    }

    fn bar_time(&self, x: f32) -> f64 {
        let track = bar_layout(self.bar_width).track;

        ((x - track.left) / (track.right - track.left)).clamp(0.0, 1.0) as f64 * self.duration
    }

    fn drag_bar(&mut self, x: f32) -> Result<()> {
        match self.bar_drag {
            Some(BarItem::Seek) => self.player.seek(self.bar_time(x), false),
            Some(BarItem::Volume) => {
                let track = bar_layout(self.bar_width).volume_track;

                self.set_volume(((x - track.left) / (track.right - track.left)).clamp(0.0, 1.0) as f64)
            }
            _ => Ok(()),
        }
    }

    fn seek_by(&mut self, seconds: f64) -> Result<()> {
        if !self.loaded() {
            return Ok(());
        }

        self.player.seek((self.player.time() + seconds).clamp(0.0, self.duration), true)?;
        self.invalidate()?;
        self.invalidate_bar()
    }

    fn set_volume(&mut self, volume: f64) -> Result<()> {
        self.volume = volume;
        self.player.set_volume(volume)?;

        if self.muted && volume > 0.0 {
            self.muted = false;
            self.player.set_muted(false)?;
        }

        self.invalidate_bar()
    }

    fn change_volume(&mut self, delta: f64) -> Result<()> {
        self.set_volume((self.volume + delta).clamp(0.0, 1.0))
    }

    fn toggle_mute(&mut self) -> Result<()> {
        self.muted = !self.muted;
        self.player.set_muted(self.muted)?;
        self.invalidate_bar()
    }

    fn sibling(&self, step: isize) -> Result<Option<PathBuf>> {
        let Some(file) = &self.file else {
            return Ok(None);
        };
        let folder = file.parent().ok_or_else(|| Error::new(E_FAIL, format!("{} has no folder", file.display())))?;
        let unreadable = |e: std::io::Error| Error::new(E_FAIL, format!("Cannot list {}: {e}", folder.display()));
        let mut videos: Vec<PathBuf> = std::fs::read_dir(folder).map_err(unreadable)?.map(|entry| entry.map(|entry| entry.path())).collect::<std::io::Result<_>>().map_err(unreadable)?;

        videos.retain(|path| path.is_file() && is_video(path));
        videos.sort_by_key(|path| path.file_name().map(|name| name.to_string_lossy().to_lowercase()));

        let index = videos.iter().position(|path| path == file).ok_or_else(|| Error::new(E_FAIL, format!("{} is no longer in its folder", file.display())))?;

        Ok(index.checked_add_signed(step).and_then(|next| videos.get(next)).cloned())
    }

    fn status_rect(&self) -> D2D_RECT_F {
        let left = match self.mode {
            Output::Compressed => STATUS_LEFT_WITH_TARGET,
            Output::Lossless | Output::Exact => STATUS_LEFT,
        };

        rect(left, 200.0, STATUS_RIGHT, 236.0)
    }

    fn apply_target_text(&mut self, text: &str) -> Result<()> {
        let mut target = CompressSettings::load()?;
        let parsed = match self.target.fit_size {
            true => parse_size(text).map(|size| target.size = size),
            false => parse_quality(text).map(|quality| target.quality = quality),
        };

        if let Err(e) = parsed {
            self.status = Status::Info(e.message());
            return Ok(());
        }

        target.fit_size = self.target.fit_size;
        target.save()?;
        self.target = target;
        self.status = Status::Hint;
        Ok(())
    }

    fn compress_target(&self) -> Target {
        match self.target.fit_size {
            true => Target::Size(self.target.size),
            false => Target::Quality(self.target.quality),
        }
    }

    fn keyframe_before(&self, time: f64) -> Result<i64> {
        self.keyframes.as_ref().ok_or_else(|| Error::new(E_FAIL, "Open a video first"))?.before(ticks(time))
    }

    fn validate(&self, start: f64, end: f64, skip: Option<usize>) -> std::result::Result<(), String> {
        if start >= end {
            return Err("The end must be after the start.".to_string());
        }

        if end > self.duration + 0.005 {
            return Err(format!("The end is past the video length ({}).", format_time(self.duration)));
        }

        let overlap = self.segments.iter().enumerate().find(|&(index, segment)| Some(index) != skip && start < segment.end && segment.start < end);

        match overlap {
            Some((index, _)) => Err(format!("That overlaps segment {}.", index + 1)),
            None => Ok(()),
        }
    }

    fn put_segment(&mut self, start: f64, end: f64, replace: Option<usize>) -> Result<()> {
        if let Err(message) = self.validate(start, end, replace) {
            self.status = Status::Info(message);
            return Ok(());
        }

        let segment = Segment { start, end, keyframe: self.keyframe_before(start)? };

        match replace {
            Some(index) => self.segments[index] = segment,
            None => self.segments.push(segment),
        }

        self.segments.sort_by(|a, b| a.start.total_cmp(&b.start));
        self.status = Status::Hint;
        Ok(())
    }

    fn output_length(&self) -> f64 {
        self.segments
            .iter()
            .map(|segment| match self.mode {
                Output::Lossless => segment.end - segment.keyframe_time(),
                Output::Exact | Output::Compressed => segment.end - segment.start,
            })
            .sum()
    }

    fn start_export(&mut self) -> Result<()> {
        let file = self.file.clone().ok_or_else(|| Error::new(E_FAIL, "Open a video first"))?;
        let ranges = match (self.segments.is_empty(), self.in_point, self.out_point) {
            (false, _, _) => self.segments.clone(),
            (true, Some(start), Some(end)) if start < end => vec![Segment { start, end, keyframe: self.keyframe_before(start)? }],
            _ => {
                self.status = Status::Info("Set In and Out, then Add segment (or just set In and Out to export one range).".to_string());
                return Ok(());
            }
        };
        let segments: Vec<(i64, i64)> = ranges
            .iter()
            .map(|segment| match self.mode {
                Output::Lossless => (segment.keyframe, ticks(segment.end)),
                Output::Exact | Output::Compressed => (ticks(segment.start), ticks(segment.end)),
            })
            .collect();
        let plan = match self.mode {
            Output::Lossless => Plan::Trim(Mode::Lossless),
            Output::Exact => Plan::Trim(Mode::Exact),
            Output::Compressed => Plan::Compress(self.compress_target()),
        };

        if self.playing {
            self.player.pause()?;
        }

        self.job = Some(Job::start(self.window, WM_EXPORT_DONE, file.clone(), output_path(&file, "trim")?, segments, plan));
        self.update_timer()
    }
}

fn load(window: HWND, path: PathBuf) -> Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| Error::new(E_FAIL, "The selected file has no name"))?;

    with_trim(|t| {
        if t.job.is_some() {
            return Err(Error::new(E_FAIL, "An export is still running. Wait for it or cancel it before opening another video."));
        }

        t.keyframes = Some(Keyframes::open(&path)?);
        t.player.open(&path)?;

        if t.view == View::Player {
            t.player.play()?;
        }

        t.folder = path.parent().map(Path::to_path_buf).ok_or_else(|| Error::new(E_FAIL, "The selected file has no folder"))?;
        t.file = Some(path);
        t.duration = 0.0;
        t.playing = false;
        t.in_point = None;
        t.out_point = None;
        t.segments.clear();
        t.scroll = 0;
        t.status = Status::Hint;
        t.controls = true;
        t.invalidate()
    })?;

    unsafe { SetWindowTextW(window, &HSTRING::from(format!("MiniRec  \u{b7}  {name}")))? };
    layout(window)
}

fn sibling(window: HWND, step: isize) -> Result<()> {
    match with_trim(|t| t.sibling(step))? {
        Some(next) => load(window, next),
        None => Ok(()),
    }
}

fn drop_files(window: HWND, drop: HDROP) -> Result<()> {
    let mut buffer = vec![0u16; 32768];
    let length = unsafe { DragQueryFileW(drop, 0, Some(&mut buffer)) };

    unsafe { DragFinish(drop) };

    if length == 0 {
        return Err(Error::from_win32());
    }

    let path = PathBuf::from(OsString::from_wide(&buffer[..length as usize]));

    if !is_video(&path) {
        return Err(Error::new(E_INVALIDARG, format!("{} is not a video MiniRec can open. Use .mp4, .m4v or .mov.", path.display())));
    }

    load(window, path)
}

fn toggle_view(window: HWND) -> Result<()> {
    let (view, fullscreen) = with_trim(|t| Ok((t.view, t.fullscreen())))?;

    match view {
        View::Player => {
            if fullscreen {
                toggle_fullscreen(window)?;
            }

            set_view(window, View::Edit)
        }
        View::Edit => set_view(window, View::Player),
    }
}

fn set_view(window: HWND, view: View) -> Result<()> {
    let (fullscreen, scale) = with_trim(|t| {
        t.view = view;
        t.controls = true;
        t.hover = None;
        t.invalidate()?;
        Ok((t.fullscreen(), t.canvas.scale()))
    })?;

    if view == View::Edit && !fullscreen && !unsafe { IsZoomed(window) }.as_bool() {
        let (min_width, min_height) = frame_size(PANEL_WIDTH, PANEL_HEIGHT + BAR_HEIGHT + MIN_EDIT_VIDEO, scale)?;
        let mut frame = RECT::default();

        unsafe { GetWindowRect(window, &mut frame)? };

        let (width, height) = (frame.right - frame.left, frame.bottom - frame.top);

        if width < min_width || height < min_height {
            unsafe { SetWindowPos(window, None, 0, 0, width.max(min_width), height.max(min_height), SWP_NOMOVE | SWP_NOZORDER)? };
        }
    }

    layout(window)
}

fn activate(window: HWND, item: Item) -> Result<()> {
    match item {
        Item::Open => {
            let folder = with_trim(|t| Ok(t.folder.clone()))?;
            let Some(path) = pick_video(window, &folder)? else {
                return Ok(());
            };

            load(window, path)
        }
        Item::Fullscreen => toggle_fullscreen(window),
        Item::Target => pick_target(window),
        Item::TargetValue => {
            let (area, text, scale) = with_trim(|t| {
                let text = match t.target.fit_size {
                    true => format_megabytes(t.target.size),
                    false => t.target.quality.to_string(),
                };

                Ok((t.to_screen_panel(TARGET_VALUE), text, t.canvas.scale()))
            })?;

            open_edit(window, area, &text, scale, item)
        }
        Item::Start(index) | Item::End(index) => {
            let (area, text, scale) = with_trim(|t| {
                let row = row_rect(index - t.scroll);
                let (field, value) = match item {
                    Item::Start(_) => (start_field(row), t.segments[index].start),
                    _ => (end_field(row), t.segments[index].end),
                };

                Ok((t.to_screen_panel(field), format_time(value), t.canvas.scale()))
            })?;

            open_edit(window, area, &text, scale, item)
        }
        _ => with_trim(|t| {
            match item {
                Item::Play if t.playing => t.player.pause()?,
                Item::Play => t.player.play()?,
                Item::StepBack | Item::StepForward => {
                    if t.playing {
                        t.player.pause()?;
                    }

                    t.player.step(item == Item::StepForward)?;
                }
                Item::SetIn => {
                    let time = t.player.time();

                    t.in_point = Some(time);
                    t.out_point = t.out_point.filter(|&out| out > time);
                }
                Item::SetOut => {
                    let time = t.player.time();

                    match t.in_point {
                        Some(start) if start >= time => t.status = Status::Info("Out must be after In.".to_string()),
                        _ => t.out_point = Some(time),
                    }
                }
                Item::Add => match (t.in_point, t.out_point) {
                    (Some(start), Some(end)) => {
                        t.put_segment(start, end, None)?;

                        if matches!(t.status, Status::Hint) {
                            t.in_point = None;
                            t.out_point = None;
                            t.scroll = t.segments.len().saturating_sub(VISIBLE_ROWS);
                        }
                    }
                    _ => t.status = Status::Info("Set In (I) and Out (O) first.".to_string()),
                },
                Item::Remove(index) => {
                    t.segments.remove(index);
                    t.scroll = t.scroll.min(t.segments.len().saturating_sub(VISIBLE_ROWS));
                }
                Item::Lossless => t.mode = Output::Lossless,
                Item::Exact => t.mode = Output::Exact,
                Item::Compressed => {
                    t.target = CompressSettings::load()?;
                    t.mode = Output::Compressed;
                }
                Item::Export => match &t.job {
                    Some(job) => job.cancel(),
                    None => t.start_export()?,
                },
                Item::Status => {
                    if let Status::Saved(path) = &t.status {
                        reveal(path)?;
                    }
                }
                Item::Video | Item::Open | Item::Fullscreen | Item::Target | Item::TargetValue | Item::Start(_) | Item::End(_) => {}
            }

            t.invalidate()?;
            t.invalidate_bar()
        }),
    }
}

fn open_edit(window: HWND, area: D2D_RECT_F, text: &str, scale: f32, item: Item) -> Result<()> {
    let edit = InlineEdit::open(window, to_pixels(field_text_area(area), scale), text, scale, item)?;

    with_trim(|t| {
        t.edit = Some(edit);
        t.invalidate()
    })
}

fn bar_activate(item: BarItem) -> Result<()> {
    let window = with_trim(|t| Ok(t.window))?;

    match item {
        BarItem::Play => run(window, Item::Play),
        BarItem::Previous => sibling(window, -1),
        BarItem::Next => sibling(window, 1),
        BarItem::Back => with_trim(|t| t.seek_by(-SEEK_STEP)),
        BarItem::Forward => with_trim(|t| t.seek_by(SEEK_STEP)),
        BarItem::Mute => with_trim(|t| t.toggle_mute()),
        BarItem::Speed => pick_speed(window),
        BarItem::Edit => toggle_view(window),
        BarItem::Fullscreen => toggle_fullscreen(window),
        BarItem::Seek | BarItem::Volume => Ok(()),
    }
}

fn pick_speed(window: HWND) -> Result<()> {
    let (scale, speed, anchor_box) = with_trim(|t| {
        t.menu_open = true;

        let speed_rect = bar_layout(t.bar_width).speed;
        let bar_top = t.bar_top();

        Ok((t.canvas.scale(), t.speed, rect(speed_rect.left, bar_top + speed_rect.top, speed_rect.right, bar_top + speed_rect.bottom)))
    })?;
    let labels: Vec<String> = SPEEDS.iter().map(|&value| speed_label(value)).collect();
    let checked: Vec<usize> = SPEEDS.iter().position(|&value| value == speed).into_iter().collect();
    let anchor = POINT { x: (anchor_box.left * scale).round() as i32, y: (anchor_box.top * scale).round() as i32 };

    unsafe { _ = SetForegroundWindow(window) };

    let chosen = popup_menu(window, anchor, &labels, &checked);

    with_trim(|t| {
        t.menu_open = false;

        if let Some(index) = chosen? {
            t.speed = SPEEDS[index];
            t.player.set_rate(t.speed)?;
        }

        t.arm_hide()?;
        t.invalidate_bar()
    })
}

fn pick_target(window: HWND) -> Result<()> {
    let (scale, target, anchor_box) = with_trim(|t| Ok((t.canvas.scale(), t.target, t.to_screen_panel(TARGET))))?;
    let labels: Vec<String> = QUALITY_PRESETS
        .iter()
        .map(|(name, value)| format!("Quality  \u{b7}  {name} {value}"))
        .chain(std::iter::once(String::new()))
        .chain(SIZE_PRESETS.iter().map(|&bytes| format!("Fit under {}", format_bytes(bytes))))
        .collect();
    let checked: Vec<usize> = match target.fit_size {
        true => SIZE_PRESETS.iter().position(|&bytes| bytes == target.size).map(|index| index + QUALITY_MENU + 1).into_iter().collect(),
        false => QUALITY_PRESETS.iter().position(|&(_, value)| value == target.quality).into_iter().collect(),
    };
    let anchor = POINT { x: (anchor_box.left * scale).round() as i32, y: (anchor_box.top * scale).round() as i32 };
    let Some(index) = popup_menu(window, anchor, &labels, &checked)? else {
        return Ok(());
    };

    with_trim(|t| {
        t.target = CompressSettings::load()?;

        match index.checked_sub(QUALITY_MENU + 1) {
            Some(size) => {
                t.target.fit_size = true;
                t.target.size = SIZE_PRESETS[size];
            }
            None => {
                t.target.fit_size = false;
                t.target.quality = QUALITY_PRESETS[index].1;
            }
        }

        t.target.save()?;
        t.invalidate()
    })
}

fn toggle_fullscreen(window: HWND) -> Result<()> {
    let windowed = with_trim(|t| {
        t.controls = true;
        t.hover = None;
        Ok(t.windowed.take())
    })?;

    match windowed {
        Some(frame) => {
            set_style(window, STYLE)?;
            unsafe { SetWindowPos(window, None, frame.left, frame.top, frame.right - frame.left, frame.bottom - frame.top, SWP_FRAMECHANGED | SWP_NOZORDER)? };
        }
        None => {
            let mut frame = RECT::default();
            let mut monitor = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };

            unsafe {
                GetWindowRect(window, &mut frame)?;
                GetMonitorInfoW(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST), &mut monitor).ok()?;
            }

            let screen = monitor.rcMonitor;

            with_trim(|t| {
                t.windowed = Some(frame);
                Ok(())
            })?;
            set_style(window, WINDOW_STYLE(WS_POPUP.0 | WS_CLIPCHILDREN.0))?;
            unsafe { SetWindowPos(window, Some(HWND_TOP), screen.left, screen.top, screen.right - screen.left, screen.bottom - screen.top, SWP_FRAMECHANGED)? };
        }
    }

    with_trim(|t| t.arm_hide())?;
    layout(window)
}

fn set_style(window: HWND, style: WINDOW_STYLE) -> Result<()> {
    if unsafe { SetWindowLongPtrW(window, GWL_STYLE, (style.0 | WS_VISIBLE.0) as isize) } == 0 {
        return Err(Error::from_win32());
    }

    Ok(())
}

fn finish_edit(commit: bool) -> Result<()> {
    let Some(edit) = with_trim(|t| Ok(t.edit.take()))? else {
        return Ok(());
    };
    let text = edit.text()?;
    let item = edit.target;

    edit.close()?;
    with_trim(|t| {
        t.invalidate()?;

        if !commit {
            return Ok(());
        }

        if item == Item::TargetValue {
            return t.apply_target_text(&text);
        }

        let time = match parse_time(&text) {
            Ok(time) => time,
            Err(e) => {
                t.status = Status::Info(e.message());
                return Ok(());
            }
        };

        match item {
            Item::Start(index) => t.put_segment(time, t.segments[index].end, Some(index)),
            Item::End(index) => t.put_segment(t.segments[index].start, time, Some(index)),
            _ => Ok(()),
        }
    })
}

fn bar_layout(width: f32) -> BarLayout {
    let right = width - 12.0;
    let center = width / 2.0;

    BarLayout {
        seek: rect(12.0, 0.0, right, 22.0),
        track: rect(16.0, 9.0, right - 4.0, 13.0),
        previous: rect(center - 100.0, 24.0, center - 66.0, 56.0),
        back: rect(center - 60.0, 24.0, center - 26.0, 56.0),
        play: rect(center - 20.0, 22.0, center + 20.0, 58.0),
        forward: rect(center + 26.0, 24.0, center + 60.0, 56.0),
        next: rect(center + 66.0, 24.0, center + 100.0, 56.0),
        mute: rect(12.0, 24.0, 44.0, 56.0),
        volume: rect(48.0, 24.0, 140.0, 56.0),
        volume_track: rect(52.0, 38.0, 136.0, 42.0),
        time: rect(152.0, 24.0, center - 108.0, 56.0),
        speed: rect(right - 124.0, 26.0, right - 76.0, 54.0),
        edit: rect(right - 70.0, 24.0, right - 38.0, 56.0),
        fullscreen: rect(right - 32.0, 24.0, right, 56.0),
    }
}

fn bar_hit(width: f32, x: f32, y: f32) -> Option<BarItem> {
    let l = bar_layout(width);

    [
        (l.seek, BarItem::Seek),
        (l.previous, BarItem::Previous),
        (l.back, BarItem::Back),
        (l.play, BarItem::Play),
        (l.forward, BarItem::Forward),
        (l.next, BarItem::Next),
        (l.mute, BarItem::Mute),
        (l.volume, BarItem::Volume),
        (l.speed, BarItem::Speed),
        (l.edit, BarItem::Edit),
        (l.fullscreen, BarItem::Fullscreen),
    ]
    .into_iter()
    .find(|(r, _)| contains(*r, x, y))
    .map(|(_, item)| item)
}

fn speed_label(speed: f64) -> String {
    format!("{speed}\u{d7}")
}

fn draw_bar(p: &Painter, t: &Trim) {
    let l = bar_layout(t.bar_width);
    let hovered = |item: BarItem| t.bar_hover == Some(item);
    let position = if t.loaded() { t.player.time() } else { 0.0 };
    let progress = if t.duration > 0.0 { (position / t.duration).clamp(0.0, 1.0) as f32 } else { 0.0 };
    let knob = l.track.left + progress * (l.track.right - l.track.left);
    let seeking = hovered(BarItem::Seek) || t.bar_drag == Some(BarItem::Seek);
    let level = if t.muted { 0.0 } else { t.volume as f32 };
    let volume_knob = l.volume_track.left + level * (l.volume_track.right - l.volume_track.left);
    let adjusting = hovered(BarItem::Volume) || t.bar_drag == Some(BarItem::Volume);

    p.fill(rect(0.0, 0.0, t.bar_width, BAR_HEIGHT), PANEL);
    p.round(l.track, 2.0, TRACK_OFF);
    p.round(rect(l.track.left, l.track.top, knob, l.track.bottom), 2.0, ACCENT);

    if t.view == View::Edit && t.loaded() {
        let band = |start: f64, end: f64, color: u32| p.round(rect(t.bar_x(start), l.track.top - 3.0, t.bar_x(end).max(t.bar_x(start) + 2.0), l.track.bottom + 3.0), 2.0, color);

        for segment in &t.segments {
            if t.mode == Output::Lossless {
                band(segment.keyframe_time(), segment.start, LEAD_IN_COLOR);
            }

            band(segment.start, segment.end, SEGMENT_COLOR);
        }

        if let Some(start) = t.in_point {
            band(start, t.out_point.map_or(position.max(start), |end| end), KNOB_DIM);
        }
    }

    p.circle(knob, (l.track.top + l.track.bottom) / 2.0, if seeking { 7.0 } else { 5.0 }, KNOB);
    p.icon_button(l.previous, PREVIOUS_GLYPH, hovered(BarItem::Previous));
    p.icon_button(l.back, BACK_GLYPH, hovered(BarItem::Back));
    p.icon_button(l.play, if t.playing { PAUSE_GLYPH } else { PLAY_GLYPH }, hovered(BarItem::Play));
    p.icon_button(l.forward, FORWARD_GLYPH, hovered(BarItem::Forward));
    p.icon_button(l.next, NEXT_GLYPH, hovered(BarItem::Next));
    p.icon_button(l.mute, if t.muted || t.volume == 0.0 { MUTE_GLYPH } else { VOLUME_GLYPH }, hovered(BarItem::Mute));
    p.round(l.volume_track, 2.0, TRACK_OFF);
    p.round(rect(l.volume_track.left, l.volume_track.top, volume_knob, l.volume_track.bottom), 2.0, KNOB);
    p.circle(volume_knob, (l.volume_track.top + l.volume_track.bottom) / 2.0, if adjusting { 6.0 } else { 4.5 }, KNOB);
    p.text(&format!("{}  /  {}", format_time(position), format_time(t.duration)), &p.fonts.body, l.time, TEXT);

    if hovered(BarItem::Speed) {
        p.round(l.speed, 6.0, BUTTON_HOVER);
    }

    p.text(&speed_label(t.speed), &p.fonts.button, l.speed, TEXT);
    if t.view == View::Edit {
        p.round(l.edit, 6.0, BUTTON);
    }

    p.icon_button(l.edit, EDIT_GLYPH, hovered(BarItem::Edit));
    p.icon_button(l.fullscreen, if t.fullscreen() { WINDOWED_GLYPH } else { FULLSCREEN_GLYPH }, hovered(BarItem::Fullscreen));
}

fn draw(p: &Painter, t: &Trim) {
    let hovered = |item: Item| t.hover == Some(item) && t.enabled(item);
    let video = t.video_rect();

    p.clear(BACKGROUND);
    p.fill(video, BLACK);

    if t.file.is_none() {
        let prompt = t.prompt_rect();

        p.button(prompt, "Open video", t.enabled(Item::Open), hovered(Item::Open));
        p.text("or drop a video file here", &p.fonts.small_center, rect(prompt.left - 60.0, prompt.bottom + 8.0, prompt.right + 60.0, prompt.bottom + 30.0), MUTED);
    }

    if !t.editing() {
        return;
    }

    let (dx, dy) = t.panel_offset();
    let point = |value: Option<f64>| value.map_or("\u{2013}".to_string(), format_time);
    let enabled = t.enabled(Item::StepBack);

    p.translate(0.0, t.bar_top());
    draw_bar(p, t);
    p.translate(dx, dy);
    p.button(STEP_BACK, "\u{2212}1f", enabled, hovered(Item::StepBack));
    p.button(STEP_FORWARD, "+1f", enabled, hovered(Item::StepForward));
    p.button(SET_IN, "Set In  (I)", enabled, hovered(Item::SetIn));
    p.button(SET_OUT, "Set Out  (O)", enabled, hovered(Item::SetOut));
    p.primary_button(ADD, "Add segment  \u{21b5}", t.enabled(Item::Add), hovered(Item::Add));
    p.text(&format!("In  {}      Out  {}", point(t.in_point), point(t.out_point)), &p.fonts.body, IN_OUT, MUTED);
    draw_segments(p, t, &hovered);
    draw_bottom(p, t, &hovered);
    p.translate(0.0, 0.0);
}

fn draw_segments(p: &Painter, t: &Trim, hovered: &dyn Fn(Item) -> bool) {
    let count = t.segments.len();
    let header = match count {
        0 => "Segments".to_string(),
        n if n > VISIBLE_ROWS => format!("Segments  \u{b7}  showing {}\u{2013}{} of {n}  \u{b7}  scroll for more", t.scroll + 1, (t.scroll + VISIBLE_ROWS).min(n)),
        n => format!("Segments  \u{b7}  {n}"),
    };

    p.text(&header, &p.fonts.small, rect(20.0, 56.0, 640.0, 76.0), MUTED);

    if count > 0 {
        let label = format!("Output length  {}", format_time(t.output_length()));

        p.text(&label, &p.fonts.small, rect(640.0, 56.0, 940.0, 76.0), MUTED);
    }

    if count == 0 {
        let row = row_rect(0);

        p.round(row, 8.0, PANEL);
        p.text("No segments yet. Set In and Out on the timeline, then Add segment. Add as many as you like.", &p.fonts.body, rect(row.left + 16.0, row.top, row.right - 16.0, row.bottom), MUTED);
        return;
    }

    for (index, row) in t.visible_rows() {
        let segment = t.segments[index];
        let (start, end) = (segment.start, segment.end);
        let enabled = t.enabled(Item::Start(index));
        let field = |r: D2D_RECT_F, item: Item, value: f64| {
            let editing = t.edit.as_ref().is_some_and(|edit| edit.target == item);

            p.field(r, &format_time(value), enabled, hovered(item), editing);
        };

        p.round(row, 8.0, PANEL);
        p.text(&(index + 1).to_string(), &p.fonts.body, rect(row.left + 14.0, row.top, row.left + 40.0, row.bottom), MUTED);
        field(start_field(row), Item::Start(index), start);
        p.text("\u{2192}", &p.fonts.body, rect(row.left + 168.0, row.top, row.left + 190.0, row.bottom), MUTED);
        field(end_field(row), Item::End(index), end);
        p.text(&format!("{:.2} s", end - start), &p.fonts.body, rect(row.left + 330.0, row.top, row.left + 440.0, row.bottom), MUTED);

        let keyframe = segment.keyframe_time();
        let note = match t.mode {
            Output::Lossless if start - keyframe > 0.005 => format!("Starts at keyframe {}  (+{:.2} s)", format_time(keyframe), start - keyframe),
            Output::Lossless => "Starts exactly on a keyframe".to_string(),
            Output::Exact | Output::Compressed => "Frame-exact".to_string(),
        };

        p.text(&note, &p.fonts.small, rect(row.left + 450.0, row.top, row.right - 50.0, row.bottom), MUTED);
        p.button(remove_button(row), "\u{d7}", t.enabled(Item::Remove(index)), hovered(Item::Remove(index)));
    }
}

fn draw_bottom(p: &Painter, t: &Trim, hovered: &dyn Fn(Item) -> bool) {
    p.round(MODES, 8.0, PANEL);

    for (r, item, label, mode) in [(LOSSLESS, Item::Lossless, "Lossless", Output::Lossless), (EXACT, Item::Exact, "Frame-exact", Output::Exact), (COMPRESSED, Item::Compressed, "Compressed", Output::Compressed)] {
        let selected = t.mode == mode;
        let fill = match (selected, hovered(item)) {
            (true, _) => BUTTON_HOVER,
            (false, true) => PANEL_HOVER,
            (false, false) => PANEL,
        };

        p.round(r, 6.0, fill);
        p.text(label, &p.fonts.button, r, if selected { TEXT } else { MUTED });
    }

    if t.mode == Output::Compressed {
        let (value, label) = match t.target.fit_size {
            true => (format_megabytes(t.target.size), "MB max  \u{25be}"),
            false => (t.target.quality.to_string(), "Quality  \u{25be}"),
        };
        let editing = t.edit.as_ref().is_some_and(|edit| edit.target == Item::TargetValue);

        p.field(TARGET_VALUE, &value, t.enabled(Item::TargetValue), hovered(Item::TargetValue), editing);
        p.button(TARGET, label, t.enabled(Item::Target), hovered(Item::Target));
    }

    let (status, color) = match (&t.job, &t.status, &t.file) {
        (Some(job), _, _) => (format!("Exporting\u{2026}  {:.0}%", job.progress() * 100.0), TEXT),
        (None, Status::Saved(path), _) => (format!("Saved {}  \u{b7}  click to show", path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned())), ACCENT_HOVER),
        (None, Status::Info(message), _) => (message.clone(), TEXT),
        (None, Status::Hint, None) => ("Open a video to start.".to_string(), MUTED),
        (None, Status::Hint, Some(_)) => match t.mode {
            Output::Lossless => ("Lossless keeps the original quality. Cuts start on the nearest earlier keyframe.".to_string(), MUTED),
            Output::Exact => ("Frame-exact re-encodes on the GPU at the original bitrate.".to_string(), MUTED),
            Output::Compressed if t.target.fit_size => (format!("Lands just under {}", format_bytes(t.target.size)), MUTED),
            Output::Compressed => ("Lower = smaller file".to_string(), MUTED),
        },
    };

    p.text(&status, &p.fonts.body, t.status_rect(), color);

    let label = match &t.job {
        Some(job) => format!("Cancel  {:.0}%", job.progress() * 100.0),
        None => "Export".to_string(),
    };

    p.primary_button(EXPORT, &label, t.loaded(), hovered(Item::Export));
}

fn row_rect(slot: usize) -> D2D_RECT_F {
    let top = LIST_TOP + slot as f32 * ROW_STEP;

    rect(20.0, top, 940.0, top + ROW_HEIGHT)
}

fn start_field(row: D2D_RECT_F) -> D2D_RECT_F {
    rect(row.left + 44.0, row.top + 3.0, row.left + 164.0, row.bottom - 3.0)
}

fn end_field(row: D2D_RECT_F) -> D2D_RECT_F {
    rect(row.left + 194.0, row.top + 3.0, row.left + 314.0, row.bottom - 3.0)
}

fn remove_button(row: D2D_RECT_F) -> D2D_RECT_F {
    rect(row.right - 40.0, row.top + 3.0, row.right - 6.0, row.bottom - 3.0)
}

fn ticks(seconds: f64) -> i64 {
    (seconds * TICKS).round() as i64
}
