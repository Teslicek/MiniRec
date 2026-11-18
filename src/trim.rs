use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Gdi::{HDC, InvalidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Error, HSTRING, Result, w};

use crate::draw::*;
use crate::edit::{InlineEdit, WM_EDIT_DONE, edit_colors};
use crate::export::{Job, Mode, Plan, Scan, WM_EXPORT_DONE, WM_KEYFRAMES_DONE, is_cancelled, output_path, scan_keyframes, snap};
use crate::player::{Player, PlayerEvent, WM_PLAYER_EVENT, decode_event};
use crate::shell::{VIDEO_CLASS, WM_MOUSELEAVE, dark_title, error_box, fit_client, high_word, low_word, mouse_point, pick_video, place_child, reveal};
use crate::units::{format_time, parse_time};

const CLASS: windows::core::PCWSTR = w!("MiniRecTrim");
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);
const WIDTH: f32 = 960.0;
const HEIGHT: f32 = 794.0;
const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 33;
const TICKS: f64 = 10_000_000.0;
const VISIBLE_ROWS: usize = 3;

const OPEN: D2D_RECT_F = rect(20.0, 14.0, 132.0, 46.0);
const VIDEO: D2D_RECT_F = rect(20.0, 60.0, 940.0, 480.0);
const PLAY: D2D_RECT_F = rect(20.0, 490.0, 60.0, 530.0);
const STEP_BACK: D2D_RECT_F = rect(336.0, 494.0, 384.0, 526.0);
const STEP_FORWARD: D2D_RECT_F = rect(390.0, 494.0, 438.0, 526.0);
const SET_IN: D2D_RECT_F = rect(462.0, 494.0, 556.0, 526.0);
const SET_OUT: D2D_RECT_F = rect(562.0, 494.0, 662.0, 526.0);
const ADD: D2D_RECT_F = rect(668.0, 494.0, 800.0, 526.0);
const TIMELINE: D2D_RECT_F = rect(20.0, 544.0, 940.0, 590.0);
const TRACK: D2D_RECT_F = rect(20.0, 561.0, 940.0, 573.0);
const LIST_TOP: f32 = 624.0;
const ROW_STEP: f32 = 36.0;
const ROW_HEIGHT: f32 = 32.0;
const MODES: D2D_RECT_F = rect(20.0, 740.0, 280.0, 776.0);
const LOSSLESS: D2D_RECT_F = rect(24.0, 744.0, 150.0, 772.0);
const EXACT: D2D_RECT_F = rect(150.0, 744.0, 276.0, 772.0);
const STATUS: D2D_RECT_F = rect(296.0, 740.0, 764.0, 776.0);
const EXPORT: D2D_RECT_F = rect(780.0, 740.0, 940.0, 776.0);

#[derive(Clone, Copy, PartialEq)]
enum Item {
    Open,
    Play,
    StepBack,
    StepForward,
    SetIn,
    SetOut,
    Add,
    Timeline,
    Start(usize),
    End(usize),
    Remove(usize),
    Lossless,
    Exact,
    Export,
    Status,
}

enum Status {
    Hint,
    Info(String),
    Saved(PathBuf),
}

struct Trim {
    window:         HWND,
    video:          HWND,
    canvas:         Canvas,
    player:         Player,
    folder:         PathBuf,
    file:           Option<PathBuf>,
    duration:       f64,
    playing:        bool,
    keyframes:      Option<Vec<i64>>,
    frames:         u64,
    scan:           Option<(usize, JoinHandle<Result<Scan>>)>,
    scan_id:        usize,
    in_point:       Option<f64>,
    out_point:      Option<f64>,
    segments:       Vec<(f64, f64)>,
    mode:           Mode,
    job:            Option<Job>,
    status:         Status,
    hover:          Option<Item>,
    pressed:        Option<Item>,
    dragging:       bool,
    tracking_mouse: bool,
    timer_on:       bool,
    scroll:         usize,
    edit:           Option<InlineEdit<Item>>,
    hand:           HCURSOR,
}

thread_local! {
    static TRIM: RefCell<Option<Trim>> = const { RefCell::new(None) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
}

pub fn open(folder: &Path) -> Result<()> {
    if let Some(window) = TRIM.with_borrow(|trim| trim.as_ref().map(|t| t.window)) {
        unsafe {
            _ = ShowWindow(window, if IsIconic(window).as_bool() { SW_RESTORE } else { SW_SHOW });
            _ = SetForegroundWindow(window);
        }

        return Ok(());
    }

    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();

        if !REGISTERED.get() {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hIcon: LoadIconW(Some(instance), windows::core::PCWSTR(1 as *const u16))?,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: CLASS,
                ..Default::default()
            };

            if RegisterClassExW(&class) == 0 {
                return Err(Error::from_win32());
            }

            REGISTERED.set(true);
        }

        let window = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("MiniRec Trim"), STYLE, CW_USEDEFAULT, CW_USEDEFAULT, 900, 800, None, None, Some(instance), None)?;

        dark_title(window)?;

        let video = CreateWindowExW(WINDOW_EX_STYLE(0), VIDEO_CLASS, None, WS_CHILD | WS_VISIBLE, 0, 0, 0, 0, Some(window), None, Some(instance), None)?;
        let dpi = GetDpiForWindow(window);
        let trim = Trim {
            window,
            video,
            canvas: Canvas::new(dpi)?,
            player: Player::new(video, window)?,
            folder: folder.to_path_buf(),
            file: None,
            duration: 0.0,
            playing: false,
            keyframes: None,
            frames: 0,
            scan: None,
            scan_id: 0,
            in_point: None,
            out_point: None,
            segments: Vec::new(),
            mode: Mode::Lossless,
            job: None,
            status: Status::Hint,
            hover: None,
            pressed: None,
            dragging: false,
            tracking_mouse: false,
            timer_on: false,
            scroll: 0,
            edit: None,
            hand: LoadCursorW(None, IDC_HAND)?,
        };

        TRIM.set(Some(trim));
        fit_client(window, dpi, WIDTH, HEIGHT, STYLE)?;
        place_child(video, to_pixels(VIDEO, dpi as f32 / 96.0))?;
        _ = ShowWindow(window, SW_SHOW);
        _ = SetForegroundWindow(window);
        Ok(())
    }
}

pub fn close() -> Result<()> {
    let Some(trim) = TRIM.take() else {
        return Ok(());
    };
    let Trim { window, player, job, edit, .. } = trim;
    let export = job.map(|job| {
        job.cancel();
        job.finish()
    });

    if let Some(edit) = edit {
        edit.close()?;
    }

    player.shutdown()?;
    unsafe { DestroyWindow(window)? };

    match export {
        Some(Err(e)) if !is_cancelled(&e) => Err(e),
        _ => Ok(()),
    }
}

extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ready = TRIM.with(|cell| matches!(cell.try_borrow().as_deref(), Ok(Some(_))));

    if !ready {
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    }

    match handle(window, message, wparam, lparam) {
        Ok(Some(result)) => result,
        Ok(None) => unsafe { DefWindowProcW(window, message, wparam, lparam) },
        Err(e) => {
            error_box(Some(window), &e);
            LRESULT(0)
        }
    }
}

fn with_trim<R>(f: impl FnOnce(&mut Trim) -> Result<R>) -> Result<R> {
    TRIM.with_borrow_mut(|trim| f(trim.as_mut().ok_or_else(|| Error::new(E_FAIL, "Trim window is not open"))?))
}

fn handle(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Result<Option<LRESULT>> {
    match message {
        WM_PAINT => with_trim(|t| t.canvas.paint(window, |p| draw(p, t)))?,
        WM_ERASEBKGND => return Ok(Some(LRESULT(1))),
        WM_SIZE => with_trim(|t| t.canvas.resize(low_word(lparam.0), high_word(lparam.0)))?,
        WM_DPICHANGED => {
            let suggested = unsafe { *(lparam.0 as *const RECT) };
            let dpi = high_word(wparam.0 as isize);
            let video = with_trim(|t| {
                t.canvas.set_dpi(dpi);
                Ok(t.video)
            })?;

            unsafe { SetWindowPos(window, None, suggested.left, suggested.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)? };
            fit_client(window, dpi, WIDTH, HEIGHT, STYLE)?;
            place_child(video, to_pixels(VIDEO, dpi as f32 / 96.0))?;
        }
        WM_CLOSE => close()?,
        WM_CTLCOLOREDIT => return Ok(Some(edit_colors(HDC(wparam.0 as *mut _)))),
        WM_EDIT_DONE => finish_edit(wparam.0 == 1)?,
        WM_MOUSEMOVE => {
            let (x, y) = with_trim(|t| Ok(mouse_point(lparam.0, t.canvas.scale())))?;

            with_trim(|t| {
                if !t.tracking_mouse {
                    let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: window, dwHoverTime: 0 };

                    unsafe { TrackMouseEvent(&mut track)? };
                    t.tracking_mouse = true;
                }

                if t.dragging {
                    t.player.seek(t.timeline_time(x), false)?;
                }

                let hover = t.hit(x, y);

                if hover != t.hover || t.dragging {
                    t.hover = hover;
                    t.invalidate()?;
                }

                Ok(())
            })?;
        }
        WM_MOUSELEAVE => with_trim(|t| {
            t.tracking_mouse = false;
            t.hover = None;
            t.invalidate()
        })?,
        WM_SETCURSOR if low_word(lparam.0) == HTCLIENT => {
            let hand = with_trim(|t| Ok(t.hover.filter(|&item| t.enabled(item)).map(|_| t.hand)))?;
            let Some(cursor) = hand else {
                return Ok(None);
            };

            unsafe { SetCursor(Some(cursor)) };
            return Ok(Some(LRESULT(1)));
        }
        WM_LBUTTONDOWN => {
            unsafe { _ = SetFocus(Some(window)) };
            with_trim(|t| {
                let (x, y) = mouse_point(lparam.0, t.canvas.scale());
                let item = t.hit(x, y).filter(|&item| t.enabled(item));

                if item == Some(Item::Timeline) {
                    t.dragging = true;
                    unsafe { SetCapture(window) };
                    t.player.seek(t.timeline_time(x), false)?;
                    return t.invalidate();
                }

                t.pressed = item;
                Ok(())
            })?;
        }
        WM_LBUTTONUP => {
            let (dragging, released, pressed) = with_trim(|t| {
                let (x, y) = mouse_point(lparam.0, t.canvas.scale());

                Ok((std::mem::take(&mut t.dragging).then(|| t.timeline_time(x)), t.hit(x, y), t.pressed.take()))
            })?;

            if let Some(time) = dragging {
                unsafe { ReleaseCapture()? };
                with_trim(|t| {
                    t.player.seek(time, true)?;
                    t.invalidate()
                })?;
            } else if let Some(item) = released.filter(|&item| Some(item) == pressed) {
                activate(window, item)?;
            }
        }
        WM_MOUSEWHEEL => with_trim(|t| {
            let delta = (wparam.0 >> 16) as u16 as i16;
            let max = t.segments.len().saturating_sub(VISIBLE_ROWS);

            t.scroll = match delta {
                d if d < 0 => (t.scroll + 1).min(max),
                _ => t.scroll.saturating_sub(1),
            };

            t.invalidate()
        })?,
        WM_KEYDOWN => {
            let key = VIRTUAL_KEY(wparam.0 as u16);
            let item = match key {
                VK_SPACE => Item::Play,
                VK_I => Item::SetIn,
                VK_O => Item::SetOut,
                VK_RETURN => Item::Add,
                VK_LEFT => Item::StepBack,
                VK_RIGHT => Item::StepForward,
                _ => return Ok(None),
            };

            if with_trim(|t| Ok(t.enabled(item)))? {
                activate(window, item)?;
            }
        }
        WM_TIMER => with_trim(|t| t.invalidate())?,
        WM_PLAYER_EVENT => with_trim(|t| {
            match decode_event(wparam, lparam) {
                PlayerEvent::Loaded => t.duration = t.player.duration(),
                PlayerEvent::Playing => t.playing = true,
                PlayerEvent::Stopped => t.playing = false,
                PlayerEvent::Failed(e) => {
                    t.file = None;
                    t.duration = 0.0;
                    t.invalidate()?;
                    return Err(e);
                }
                PlayerEvent::Other => {}
            }

            t.update_timer()?;
            t.invalidate()
        })?,
        WM_KEYFRAMES_DONE => with_trim(|t| {
            let Some((_, scan)) = t.scan.take_if(|(id, _)| *id == wparam.0) else {
                return Ok(());
            };
            let scan = scan.join().map_err(|_| Error::new(E_FAIL, "Keyframe scan panicked"))??;

            t.frames = scan.frames;
            t.keyframes = Some(scan.keyframes);
            t.invalidate()
        })?,
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

impl Trim {
    fn invalidate(&self) -> Result<()> {
        unsafe { InvalidateRect(Some(self.window), None, false).ok() }
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

    fn loaded(&self) -> bool {
        self.file.is_some() && self.duration > 0.0
    }

    fn enabled(&self, item: Item) -> bool {
        let idle = self.job.is_none();

        match item {
            Item::Open | Item::Lossless | Item::Exact => idle,
            Item::Export | Item::Status => true,
            Item::Play | Item::Timeline | Item::StepBack | Item::StepForward | Item::SetIn | Item::SetOut => self.loaded(),
            Item::Add | Item::Start(_) | Item::End(_) | Item::Remove(_) => self.loaded() && idle,
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Item> {
        let fixed = [
            (OPEN, Item::Open),
            (PLAY, Item::Play),
            (STEP_BACK, Item::StepBack),
            (STEP_FORWARD, Item::StepForward),
            (SET_IN, Item::SetIn),
            (SET_OUT, Item::SetOut),
            (ADD, Item::Add),
            (TIMELINE, Item::Timeline),
            (LOSSLESS, Item::Lossless),
            (EXACT, Item::Exact),
            (EXPORT, Item::Export),
            (STATUS, Item::Status),
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

    fn timeline_time(&self, x: f32) -> f64 {
        ((x - TRACK.left) / (TRACK.right - TRACK.left)).clamp(0.0, 1.0) as f64 * self.duration
    }

    fn timeline_x(&self, time: f64) -> f32 {
        TRACK.left + (time / self.duration).clamp(0.0, 1.0) as f32 * (TRACK.right - TRACK.left)
    }

    fn snapped(&self, start: f64) -> Option<f64> {
        self.keyframes.as_deref().map(|k| snap(k, ticks(start)) as f64 / TICKS)
    }

    fn validate(&self, start: f64, end: f64, skip: Option<usize>) -> std::result::Result<(), String> {
        if start >= end {
            return Err("The end must be after the start.".to_string());
        }

        if end > self.duration + 0.005 {
            return Err(format!("The end is past the video length ({}).", format_time(self.duration)));
        }

        let overlap = self.segments.iter().enumerate().find(|&(index, &(a, b))| Some(index) != skip && start < b && a < end);

        match overlap {
            Some((index, _)) => Err(format!("That overlaps segment {}.", index + 1)),
            None => Ok(()),
        }
    }

    fn put_segment(&mut self, start: f64, end: f64, replace: Option<usize>) {
        match self.validate(start, end, replace) {
            Ok(()) => {
                match replace {
                    Some(index) => self.segments[index] = (start, end),
                    None => self.segments.push((start, end)),
                }

                self.segments.sort_by(|a, b| a.0.total_cmp(&b.0));
                self.status = Status::Hint;
            }
            Err(message) => self.status = Status::Info(message),
        }
    }

    fn output_length(&self) -> f64 {
        self.segments
            .iter()
            .map(|&(start, end)| match self.mode {
                Mode::Lossless => end - self.snapped(start).map_or(start, |snapped| snapped),
                Mode::Exact => end - start,
            })
            .sum()
    }

    fn start_export(&mut self) -> Result<()> {
        let file = self.file.clone().ok_or_else(|| Error::new(E_FAIL, "Open a video first"))?;
        let ranges = match (self.segments.is_empty(), self.in_point, self.out_point) {
            (false, _, _) => self.segments.clone(),
            (true, Some(start), Some(end)) if start < end => vec![(start, end)],
            _ => {
                self.status = Status::Info("Set In and Out, then Add segment (or just set In and Out to export one range).".to_string());
                return Ok(());
            }
        };
        let segments: Vec<(i64, i64)> = match self.mode {
            Mode::Lossless => {
                let Some(keyframes) = &self.keyframes else {
                    self.status = Status::Info("Still scanning keyframes, try again in a moment.".to_string());
                    return Ok(());
                };

                ranges.iter().map(|&(start, end)| (snap(keyframes, ticks(start)), ticks(end))).collect()
            }
            Mode::Exact => ranges.iter().map(|&(start, end)| (ticks(start), ticks(end))).collect(),
        };

        if self.playing {
            self.player.pause()?;
        }

        self.job = Some(Job::start(self.window, WM_EXPORT_DONE, file.clone(), output_path(&file, "trim")?, segments, Plan::Trim(self.mode)));
        self.update_timer()
    }
}

fn activate(window: HWND, item: Item) -> Result<()> {
    match item {
        Item::Open => {
            let folder = with_trim(|t| Ok(t.folder.clone()))?;
            let Some(path) = pick_video(window, &folder)? else {
                return Ok(());
            };
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| Error::new(E_FAIL, "The selected file has no name"))?;

            with_trim(|t| {
                t.player.open(&path)?;
                t.scan_id += 1;
                t.scan = Some((t.scan_id, scan_keyframes(window, path.clone(), t.scan_id)));
                t.folder = path.parent().map(Path::to_path_buf).ok_or_else(|| Error::new(E_FAIL, "The selected file has no folder"))?;
                t.file = Some(path);
                t.duration = 0.0;
                t.playing = false;
                t.keyframes = None;
                t.in_point = None;
                t.out_point = None;
                t.segments.clear();
                t.scroll = 0;
                t.status = Status::Hint;
                t.invalidate()
            })?;

            unsafe { SetWindowTextW(window, &HSTRING::from(format!("MiniRec Trim  ·  {name}"))) }
        }
        Item::Start(index) | Item::End(index) => {
            let (area, text, scale) = with_trim(|t| {
                let row = row_rect(index - t.scroll);
                let (field, value) = match item {
                    Item::Start(_) => (start_field(row), t.segments[index].0),
                    _ => (end_field(row), t.segments[index].1),
                };

                Ok((field, format_time(value), t.canvas.scale()))
            })?;
            let edit = InlineEdit::open(window, to_pixels(field_text_area(area), scale), &text, scale, item)?;

            with_trim(|t| {
                t.edit = Some(edit);
                t.invalidate()
            })
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
                        t.put_segment(start, end, None);

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
                Item::Lossless => t.mode = Mode::Lossless,
                Item::Exact => t.mode = Mode::Exact,
                Item::Export => match &t.job {
                    Some(job) => job.cancel(),
                    None => t.start_export()?,
                },
                Item::Status => {
                    if let Status::Saved(path) = &t.status {
                        reveal(path)?;
                    }
                }
                Item::Open | Item::Timeline | Item::Start(_) | Item::End(_) => {}
            }

            t.invalidate()
        }),
    }
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

        let time = match parse_time(&text) {
            Ok(time) => time,
            Err(e) => {
                t.status = Status::Info(e.message());
                return Ok(());
            }
        };

        match item {
            Item::Start(index) => t.put_segment(time, t.segments[index].1, Some(index)),
            Item::End(index) => t.put_segment(t.segments[index].0, time, Some(index)),
            _ => {}
        }

        Ok(())
    })
}

fn draw(p: &Painter, t: &Trim) {
    let hovered = |item: Item| t.hover == Some(item) && t.enabled(item);
    let position = if t.loaded() { t.player.time() } else { 0.0 };

    p.clear(BACKGROUND);
    p.button(OPEN, "Open video", t.enabled(Item::Open), hovered(Item::Open));

    let name = t.file.as_ref().and_then(|f| f.file_name()).map(|n| n.to_string_lossy().into_owned());

    p.text(name.as_deref().map_or("No video open", |n| n), &p.fonts.body, rect(146.0, 14.0, 700.0, 46.0), if name.is_some() { TEXT } else { MUTED });

    let scan_state = match (&t.file, &t.keyframes) {
        (None, _) => String::new(),
        (Some(_), None) => "Scanning keyframes…".to_string(),
        (Some(_), Some(k)) => format!("{} frames  ·  {} keyframes", t.frames, k.len()),
    };

    p.text(&scan_state, &p.fonts.body_right, rect(700.0, 14.0, 940.0, 46.0), MUTED);
    p.round(VIDEO, 10.0, BLACK);
    draw_transport(p, t, position, &hovered);
    draw_timeline(p, t, position);
    draw_segments(p, t, &hovered);
    draw_bottom(p, t, &hovered);
}

fn draw_transport(p: &Painter, t: &Trim, position: f64, hovered: &dyn Fn(Item) -> bool) {
    let enabled = t.enabled(Item::Play);
    let cx = (PLAY.left + PLAY.right) / 2.0;
    let cy = (PLAY.top + PLAY.bottom) / 2.0;

    p.circle(cx, cy, 20.0, if hovered(Item::Play) { BUTTON_HOVER } else { BUTTON });

    match t.playing {
        true => {
            p.round(rect(cx - 6.0, cy - 7.0, cx - 2.0, cy + 7.0), 1.5, TEXT);
            p.round(rect(cx + 2.0, cy - 7.0, cx + 6.0, cy + 7.0), 1.5, TEXT);
        }
        false => p.text("▶", &p.fonts.button, rect(cx - 14.0, cy - 14.0, cx + 16.0, cy + 14.0), if enabled { TEXT } else { DIM }),
    }

    p.text(&format!("{}  /  {}", format_time(position), format_time(t.duration)), &p.fonts.body, rect(72.0, 490.0, 330.0, 530.0), if enabled { TEXT } else { MUTED });
    p.button(STEP_BACK, "−1f", enabled, hovered(Item::StepBack));
    p.button(STEP_FORWARD, "+1f", enabled, hovered(Item::StepForward));
    p.button(SET_IN, "Set In  (I)", enabled, hovered(Item::SetIn));
    p.button(SET_OUT, "Set Out  (O)", enabled, hovered(Item::SetOut));
    p.primary_button(ADD, "Add segment  ↵", t.enabled(Item::Add), hovered(Item::Add));

    let point = |value: Option<f64>| value.map_or("–".to_string(), format_time);

    p.text(&format!("In   {}", point(t.in_point)), &p.fonts.small, rect(812.0, 490.0, 940.0, 510.0), MUTED);
    p.text(&format!("Out  {}", point(t.out_point)), &p.fonts.small, rect(812.0, 510.0, 940.0, 530.0), MUTED);
}

fn draw_timeline(p: &Painter, t: &Trim, position: f64) {
    p.round(TRACK, 6.0, TRACK_OFF);

    if !t.loaded() {
        return;
    }

    for &(start, end) in &t.segments {
        if t.mode == Mode::Lossless
            && let Some(snapped) = t.snapped(start)
        {
            p.round(rect(t.timeline_x(snapped), 557.0, t.timeline_x(start), 577.0), 3.0, ACCENT_DIM);
        }

        p.round(rect(t.timeline_x(start), 557.0, t.timeline_x(end).max(t.timeline_x(start) + 2.0), 577.0), 3.0, ACCENT);
    }

    if let Some(start) = t.in_point {
        let end = t.out_point.map_or(position.max(start), |end| end);

        p.round(rect(t.timeline_x(start), 559.0, t.timeline_x(end).max(t.timeline_x(start) + 2.0), 575.0), 3.0, KNOB_DIM);
    }

    let x = t.timeline_x(position);

    p.fill(rect(x - 1.0, 548.0, x + 1.0, 586.0), RED);
    p.circle(x, 550.0, 5.0, RED);
}

fn draw_segments(p: &Painter, t: &Trim, hovered: &dyn Fn(Item) -> bool) {
    let count = t.segments.len();
    let header = match count {
        0 => "Segments".to_string(),
        n if n > VISIBLE_ROWS => format!("Segments  ·  showing {}–{} of {n}  ·  scroll for more", t.scroll + 1, (t.scroll + VISIBLE_ROWS).min(n)),
        n => format!("Segments  ·  {n}"),
    };

    p.text(&header, &p.fonts.small, rect(20.0, 600.0, 640.0, 620.0), MUTED);

    if count > 0 {
        let label = format!("Output length  {}", format_time(t.output_length()));

        p.text(&label, &p.fonts.small, rect(640.0, 600.0, 940.0, 620.0), MUTED);
    }

    if count == 0 {
        let row = row_rect(0);

        p.round(row, 8.0, PANEL);
        p.text("No segments yet. Set In and Out on the timeline, then Add segment. Add as many as you like.", &p.fonts.body, rect(row.left + 16.0, row.top, row.right - 16.0, row.bottom), MUTED);
        return;
    }

    for (index, row) in t.visible_rows() {
        let (start, end) = t.segments[index];
        let enabled = t.enabled(Item::Start(index));
        let field = |r: D2D_RECT_F, item: Item, value: f64| {
            let editing = t.edit.as_ref().is_some_and(|edit| edit.target == item);

            p.field(r, &format_time(value), enabled, hovered(item), editing);
        };

        p.round(row, 8.0, PANEL);
        p.text(&(index + 1).to_string(), &p.fonts.body, rect(row.left + 14.0, row.top, row.left + 40.0, row.bottom), MUTED);
        field(start_field(row), Item::Start(index), start);
        p.text("→", &p.fonts.body, rect(row.left + 168.0, row.top, row.left + 190.0, row.bottom), MUTED);
        field(end_field(row), Item::End(index), end);
        p.text(&format!("{:.2} s", end - start), &p.fonts.body, rect(row.left + 330.0, row.top, row.left + 440.0, row.bottom), MUTED);

        let note = match (t.mode, t.snapped(start)) {
            (Mode::Lossless, None) => "Waiting for keyframe scan".to_string(),
            (Mode::Lossless, Some(snapped)) if start - snapped > 0.005 => format!("Starts at keyframe {}  (+{:.2} s)", format_time(snapped), start - snapped),
            (Mode::Lossless, Some(_)) => "Starts exactly on a keyframe".to_string(),
            (Mode::Exact, _) => "Frame-exact".to_string(),
        };

        p.text(&note, &p.fonts.small, rect(row.left + 450.0, row.top, row.right - 50.0, row.bottom), MUTED);
        p.button(remove_button(row), "×", t.enabled(Item::Remove(index)), hovered(Item::Remove(index)));
    }
}

fn draw_bottom(p: &Painter, t: &Trim, hovered: &dyn Fn(Item) -> bool) {
    p.round(MODES, 8.0, PANEL);

    for (r, item, label, mode) in [(LOSSLESS, Item::Lossless, "Lossless", Mode::Lossless), (EXACT, Item::Exact, "Frame-exact", Mode::Exact)] {
        let selected = t.mode == mode;
        let fill = match (selected, hovered(item)) {
            (true, _) => BUTTON_HOVER,
            (false, true) => PANEL_HOVER,
            (false, false) => PANEL,
        };

        p.round(r, 6.0, fill);
        p.text(label, &p.fonts.button, r, if selected { TEXT } else { MUTED });
    }

    let (status, color) = match (&t.job, &t.status, &t.file) {
        (Some(job), _, _) => (format!("Exporting…  {:.0}%", job.progress() * 100.0), TEXT),
        (None, Status::Saved(path), _) => (format!("Saved {}  ·  click to show", path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned())), ACCENT_HOVER),
        (None, Status::Info(message), _) => (message.clone(), TEXT),
        (None, Status::Hint, None) => ("Open a video to start.".to_string(), MUTED),
        (None, Status::Hint, Some(_)) => match t.mode {
            Mode::Lossless => ("Lossless keeps the original quality. Cuts start on the nearest earlier keyframe.".to_string(), MUTED),
            Mode::Exact => ("Frame-exact re-encodes on the GPU at the original bitrate.".to_string(), MUTED),
        },
    };

    p.text(&status, &p.fonts.body, STATUS, color);

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
