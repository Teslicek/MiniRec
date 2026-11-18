use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Gdi::{HDC, InvalidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Error, HSTRING, PCWSTR, Result, w};

use crate::draw::*;
use crate::edit::{InlineEdit, WM_EDIT_DONE, edit_colors};
use crate::export::{Job, Plan, Probe, Target, WM_ESTIMATE_DONE, WM_EXPORT_DONE, is_cancelled, output_path, probe};
use crate::settings::{CompressSettings, QUALITY_PRESETS, SIZE_PRESETS};
use crate::shell::{WM_MOUSELEAVE, prepare_window, error_box, fit_client, high_word, low_word, mouse_point, pick_video, popup_menu, recycle, reveal};
use crate::units::{format_bitrate, format_bytes, format_megabytes, format_time, parse_quality, parse_size};

const CLASS: PCWSTR = w!("MiniRecCompress");
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);
const WIDTH: f32 = 640.0;
const HEIGHT: f32 = 368.0;
const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 250;

const OPEN: D2D_RECT_F = rect(20.0, 16.0, 132.0, 48.0);
const INFO: D2D_RECT_F = rect(20.0, 54.0, 620.0, 80.0);
const TARGET_ROW: D2D_RECT_F = rect(20.0, 92.0, 620.0, 136.0);
const TABS: D2D_RECT_F = rect(150.0, 100.0, 400.0, 128.0);
const QUALITY_TAB: D2D_RECT_F = rect(152.0, 102.0, 274.0, 126.0);
const SIZE_TAB: D2D_RECT_F = rect(276.0, 102.0, 398.0, 126.0);
const VALUE_ROW: D2D_RECT_F = rect(20.0, 144.0, 620.0, 188.0);
const VALUE_FIELD: D2D_RECT_F = rect(150.0, 152.0, 230.0, 180.0);
const PRESETS: D2D_RECT_F = rect(238.0, 152.0, 372.0, 180.0);
const REPLACE_ROW: D2D_RECT_F = rect(20.0, 196.0, 620.0, 240.0);
const HINT: D2D_RECT_F = rect(20.0, 248.0, 620.0, 292.0);
const STATUS: D2D_RECT_F = rect(20.0, 312.0, 456.0, 348.0);
const START: D2D_RECT_F = rect(470.0, 312.0, 620.0, 348.0);

#[derive(Clone, Copy, PartialEq)]
enum Item {
    Open,
    QualityTab,
    SizeTab,
    Value,
    Presets,
    Replace,
    Start,
    Status,
}

enum Status {
    Hint,
    Info(String),
    Done(PathBuf, String),
}

struct Compressor {
    window:   HWND,
    canvas:   Canvas,
    folder:   PathBuf,
    file:     Option<PathBuf>,
    probe:    Option<Probe>,
    info:     Option<String>,
    choices:  CompressSettings,
    remember: fn(CompressSettings) -> Result<()>,
    job:      Option<(PathBuf, Job)>,
    estimate: Option<(u32, PathBuf, Job)>,
    expected: Option<(u32, u64)>,
    status:   Status,
    hover:    Option<Item>,
    pressed:  Option<Item>,
    tracking: bool,
    timer_on: bool,
    edit:     Option<InlineEdit<Item>>,
    hand:     HCURSOR,
}

thread_local! {
    static COMPRESSOR: RefCell<Option<Compressor>> = const { RefCell::new(None) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
}

pub fn open(folder: &Path, choices: CompressSettings, remember: fn(CompressSettings) -> Result<()>) -> Result<()> {
    if let Some(window) = COMPRESSOR.with_borrow(|c| c.as_ref().map(|c| c.window)) {
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
                hIcon: LoadIconW(Some(instance), PCWSTR(1 as *const u16))?,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: CLASS,
                ..Default::default()
            };

            if RegisterClassExW(&class) == 0 {
                return Err(Error::from_win32());
            }

            REGISTERED.set(true);
        }

        let window = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("MiniRec Compress"), STYLE, CW_USEDEFAULT, CW_USEDEFAULT, 700, 420, None, None, Some(instance), None)?;

        prepare_window(window)?;

        let dpi = GetDpiForWindow(window);

        COMPRESSOR.set(Some(Compressor {
            window,
            canvas: Canvas::new(dpi)?,
            folder: folder.to_path_buf(),
            file: None,
            probe: None,
            info: None,
            choices,
            remember,
            job: None,
            estimate: None,
            expected: None,
            status: Status::Hint,
            hover: None,
            pressed: None,
            tracking: false,
            timer_on: false,
            edit: None,
            hand: LoadCursorW(None, IDC_HAND)?,
        }));
        fit_client(window, dpi, WIDTH, HEIGHT, STYLE)?;
        _ = ShowWindow(window, SW_SHOW);
        _ = SetForegroundWindow(window);
        Ok(())
    }
}

pub fn close() -> Result<()> {
    let Some(compressor) = COMPRESSOR.take() else {
        return Ok(());
    };
    let Compressor { window, job, estimate, edit, .. } = compressor;

    if let Some((_, path, estimate)) = estimate {
        estimate.cancel();
        discard_estimate(estimate.finish(), &path)?;
    }

    let cancelled = job.map(|(_, job)| {
        job.cancel();
        job.finish()
    });

    if let Some(edit) = edit {
        edit.close()?;
    }

    unsafe { DestroyWindow(window)? };

    match cancelled {
        Some(Err(e)) if !is_cancelled(&e) => Err(e),
        _ => Ok(()),
    }
}

extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ready = COMPRESSOR.with(|cell| matches!(cell.try_borrow().as_deref(), Ok(Some(_))));

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

fn with<R>(f: impl FnOnce(&mut Compressor) -> Result<R>) -> Result<R> {
    COMPRESSOR.with_borrow_mut(|c| f(c.as_mut().ok_or_else(|| Error::new(E_FAIL, "Compress window is not open"))?))
}

fn handle(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Result<Option<LRESULT>> {
    match message {
        WM_PAINT => with(|c| c.canvas.paint(window, |p| draw(p, c)))?,
        WM_ERASEBKGND => return Ok(Some(LRESULT(1))),
        WM_SIZE => with(|c| c.canvas.resize(low_word(lparam.0), high_word(lparam.0)))?,
        WM_DPICHANGED => {
            let suggested = unsafe { *(lparam.0 as *const RECT) };
            let dpi = high_word(wparam.0 as isize);

            with(|c| {
                c.canvas.set_dpi(dpi);
                Ok(())
            })?;
            unsafe { SetWindowPos(window, None, suggested.left, suggested.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)? };
            fit_client(window, dpi, WIDTH, HEIGHT, STYLE)?;
        }
        WM_CLOSE => close()?,
        WM_CTLCOLOREDIT => return Ok(Some(edit_colors(HDC(wparam.0 as *mut _)))),
        WM_EDIT_DONE => finish_edit(wparam.0 == 1)?,
        WM_MOUSEMOVE => with(|c| {
            if !c.tracking {
                let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: window, dwHoverTime: 0 };

                unsafe { TrackMouseEvent(&mut track)? };
                c.tracking = true;
            }

            let (x, y) = mouse_point(lparam.0, c.canvas.scale());
            let hover = hit(x, y);

            if hover != c.hover {
                c.hover = hover;
                c.invalidate()?;
            }

            Ok(())
        })?,
        WM_MOUSELEAVE => with(|c| {
            c.tracking = false;
            c.hover = None;
            c.invalidate()
        })?,
        WM_SETCURSOR if low_word(lparam.0) == HTCLIENT => {
            let hand = with(|c| Ok(c.hover.filter(|&item| c.enabled(item)).map(|_| c.hand)))?;
            let Some(cursor) = hand else {
                return Ok(None);
            };

            unsafe { SetCursor(Some(cursor)) };
            return Ok(Some(LRESULT(1)));
        }
        WM_LBUTTONDOWN => {
            unsafe { _ = SetFocus(Some(window)) };
            with(|c| {
                let (x, y) = mouse_point(lparam.0, c.canvas.scale());

                c.pressed = hit(x, y).filter(|&item| c.enabled(item));
                Ok(())
            })?;
        }
        WM_LBUTTONUP => {
            let (released, pressed) = with(|c| {
                let (x, y) = mouse_point(lparam.0, c.canvas.scale());

                Ok((hit(x, y), c.pressed.take()))
            })?;

            if let Some(item) = released.filter(|&item| Some(item) == pressed) {
                activate(window, item)?;
            }
        }
        WM_TIMER => with(|c| c.invalidate())?,
        WM_EXPORT_DONE => with(finished)?,
        WM_ESTIMATE_DONE => with(estimated)?,
        _ => return Ok(None),
    }

    Ok(Some(LRESULT(0)))
}

fn hit(x: f32, y: f32) -> Option<Item> {
    [
        (OPEN, Item::Open),
        (QUALITY_TAB, Item::QualityTab),
        (SIZE_TAB, Item::SizeTab),
        (VALUE_FIELD, Item::Value),
        (PRESETS, Item::Presets),
        (REPLACE_ROW, Item::Replace),
        (START, Item::Start),
        (STATUS, Item::Status),
    ]
    .into_iter()
    .find(|(r, _)| contains(*r, x, y))
    .map(|(_, item)| item)
}

impl Compressor {
    fn invalidate(&self) -> Result<()> {
        unsafe { InvalidateRect(Some(self.window), None, false).ok() }
    }

    fn enabled(&self, item: Item) -> bool {
        match item {
            Item::Start => self.file.is_some(),
            Item::Status => matches!(self.status, Status::Done(..)),
            _ => self.job.is_none(),
        }
    }

    fn save(&mut self) -> Result<()> {
        (self.remember)(self.choices)?;
        self.status = Status::Hint;
        self.refresh_estimate()?;
        self.invalidate()
    }

    fn refresh_estimate(&mut self) -> Result<()> {
        let wanted = !self.choices.fit_size && self.job.is_none() && self.expected.is_none_or(|(quality, _)| quality != self.choices.quality);

        if let Some((_, _, running)) = &self.estimate {
            running.cancel();
            return Ok(());
        }

        let (true, Some(source), Some(probe)) = (wanted, &self.file, &self.probe) else {
            return Ok(());
        };
        let output = output_path(source, "estimate")?;
        let job = Job::start(self.window, WM_ESTIMATE_DONE, source.clone(), output.clone(), probe.sample_segments(), Plan::Compress(Target::Quality(self.choices.quality)));

        self.estimate = Some((self.choices.quality, output, job));
        self.update_timer()
    }

    fn target(&self) -> Target {
        match self.choices.fit_size {
            true => Target::Size(self.choices.size),
            false => Target::Quality(self.choices.quality),
        }
    }

    fn update_timer(&mut self) -> Result<()> {
        let wanted = self.job.is_some() || self.estimate.is_some();

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

    fn start(&mut self) -> Result<()> {
        let source = self.file.clone().ok_or_else(|| Error::new(E_FAIL, "Open a video first"))?;

        if let (Target::Size(bytes), Some(probe)) = (self.target(), &self.probe)
            && let Err(e) = probe.fit_bitrate(bytes)
        {
            self.status = Status::Info(e.message());
            return Ok(());
        }

        if let Some((_, _, running)) = &self.estimate {
            running.cancel();
        }

        let output = output_path(&source, if self.choices.replace { "compressing" } else { "compressed" })?;

        self.job = Some((output.clone(), Job::start(self.window, WM_EXPORT_DONE, source, output, Vec::new(), Plan::Compress(self.target()))));
        self.status = Status::Hint;
        self.update_timer()
    }
}

fn activate(window: HWND, item: Item) -> Result<()> {
    match item {
        Item::Open => {
            let folder = with(|c| Ok(c.folder.clone()))?;
            let Some(path) = pick_video(window, &folder)? else {
                return Ok(());
            };
            let facts = probe(&path)?;
            let bytes = fs::metadata(&path).map_err(|e| Error::new(E_FAIL, format!("Cannot read {}: {e}", path.display())))?.len();
            let name = file_label(&path)?;

            with(|c| {
                c.folder = path.parent().map(Path::to_path_buf).ok_or_else(|| Error::new(E_FAIL, "The selected file has no folder"))?;
                c.info = Some(format!("{}  ·  {}  ·  video {}", format_bytes(bytes), format_time(facts.seconds), format_bitrate(facts.video_bitrate as u64)));
                c.probe = Some(facts);
                c.file = Some(path);
                c.expected = None;
                c.status = Status::Hint;
                c.refresh_estimate()?;
                c.invalidate()
            })?;

            unsafe { SetWindowTextW(window, &HSTRING::from(format!("MiniRec Compress  ·  {name}"))) }
        }
        Item::QualityTab | Item::SizeTab => with(|c| {
            c.choices.fit_size = item == Item::SizeTab;
            c.save()
        }),
        Item::Value => {
            let (text, scale) = with(|c| {
                let text = match c.choices.fit_size {
                    true => format_megabytes(c.choices.size),
                    false => c.choices.quality.to_string(),
                };

                Ok((text, c.canvas.scale()))
            })?;
            let edit = InlineEdit::open(window, to_pixels(field_text_area(VALUE_FIELD), scale), &text, scale, item)?;

            with(|c| {
                c.edit = Some(edit);
                c.invalidate()
            })
        }
        Item::Presets => {
            let (scale, choices) = with(|c| Ok((c.canvas.scale(), c.choices)))?;
            let (labels, checked): (Vec<String>, Vec<usize>) = match choices.fit_size {
                true => (SIZE_PRESETS.iter().map(|&bytes| format_bytes(bytes)).collect(), SIZE_PRESETS.iter().position(|&bytes| bytes == choices.size).into_iter().collect()),
                false => (
                    QUALITY_PRESETS.iter().map(|(name, value)| format!("{name}  ·  {value}")).collect(),
                    QUALITY_PRESETS.iter().position(|&(_, value)| value == choices.quality).into_iter().collect(),
                ),
            };
            let anchor = POINT { x: (PRESETS.left * scale).round() as i32, y: ((PRESETS.bottom + 4.0) * scale).round() as i32 };
            let Some(index) = popup_menu(window, anchor, &labels, &checked)? else {
                return Ok(());
            };

            with(|c| {
                match c.choices.fit_size {
                    true => c.choices.size = SIZE_PRESETS[index],
                    false => c.choices.quality = QUALITY_PRESETS[index].1,
                }

                c.save()
            })
        }
        Item::Replace => with(|c| {
            c.choices.replace = !c.choices.replace;
            c.save()
        }),
        Item::Start => with(|c| {
            match &c.job {
                Some((_, job)) => job.cancel(),
                None => c.start()?,
            }

            c.invalidate()
        }),
        Item::Status => with(|c| match &c.status {
            Status::Done(path, _) => reveal(path),
            _ => Ok(()),
        }),
    }
}

fn finish_edit(commit: bool) -> Result<()> {
    let Some(edit) = with(|c| Ok(c.edit.take()))? else {
        return Ok(());
    };
    let text = edit.text()?;

    edit.close()?;
    with(|c| {
        c.invalidate()?;

        if !commit {
            return Ok(());
        }

        match c.choices.fit_size {
            true => c.choices.size = parse_size(&text)?,
            false => c.choices.quality = parse_quality(&text)?,
        }

        c.save()
    })
}

fn estimated(c: &mut Compressor) -> Result<()> {
    let Some((quality, path, job)) = c.estimate.take() else {
        return Ok(());
    };
    let sampled = discard_estimate(job.finish(), &path)?;

    if let (Some(bytes), Some(probe)) = (sampled, &c.probe) {
        c.expected = Some((quality, (bytes as f64 / probe.sampled_seconds() * probe.seconds) as u64));
    }

    c.update_timer()?;
    c.refresh_estimate()?;
    c.invalidate()
}

fn discard_estimate(result: Result<PathBuf>, path: &Path) -> Result<Option<u64>> {
    match result {
        Ok(_) => {
            let bytes = fs::metadata(path).map(|m| m.len()).map_err(|e| Error::new(E_FAIL, format!("Cannot read {}: {e}", path.display())))?;

            fs::remove_file(path).map_err(|e| Error::new(E_FAIL, format!("Cannot remove {}: {e}", path.display())))?;
            Ok(Some(bytes))
        }
        Err(e) if is_cancelled(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

fn finished(c: &mut Compressor) -> Result<()> {
    let Some((output, job)) = c.job.take() else {
        return Ok(());
    };

    c.update_timer()?;
    c.invalidate()?;
    c.refresh_estimate()?;

    let source = c.file.clone().ok_or_else(|| Error::new(E_FAIL, "The source video is no longer selected"))?;

    match job.finish() {
        Ok(_) => {}
        Err(e) if is_cancelled(&e) => {
            c.status = Status::Info("Compression cancelled.".to_string());
            return Ok(());
        }
        Err(e) => {
            c.status = Status::Info("Compression failed, the original is untouched.".to_string());
            return Err(e);
        }
    }

    let size = |path: &Path| fs::metadata(path).map(|m| m.len()).map_err(|e| Error::new(E_FAIL, format!("Cannot read {}: {e}", path.display())));
    let (before, after) = (size(&source)?, size(&output)?);

    if after >= before {
        fs::remove_file(&output).map_err(|e| Error::new(E_FAIL, format!("Cannot remove {}: {e}", output.display())))?;
        c.status = Status::Info(format!("That setting would make it bigger ({} vs {}). Original kept, try a lower value.", format_bytes(after), format_bytes(before)));
        return Ok(());
    }

    let saved = match c.choices.replace {
        true => {
            if let Err(e) = recycle(&source) {
                fs::remove_file(&output).map_err(|remove| Error::new(E_FAIL, format!("Cannot remove {}: {remove}", output.display())))?;
                return Err(Error::new(e.code(), format!("{}\n\nThe original was kept and the compressed copy was discarded.", e.message())));
            }

            fs::rename(&output, &source).map_err(|e| Error::new(E_FAIL, format!("Cannot rename {} to {}: {e}", output.display(), source.display())))?;
            source
        }
        false => output,
    };

    c.status = Status::Done(saved, format!("{} to {}, {:.0}% smaller", format_bytes(before), format_bytes(after), (1.0 - after as f64 / before as f64) * 100.0));
    Ok(())
}

fn draw(p: &Painter, c: &Compressor) {
    let hovered = |item: Item| c.hover == Some(item) && c.enabled(item);
    let idle = c.job.is_none();
    let fit_size = c.choices.fit_size;

    p.clear(BACKGROUND);
    p.button(OPEN, "Open video", idle, hovered(Item::Open));

    let name = c.file.as_deref().map(file_label).and_then(Result::ok);

    p.text(name.as_deref().map_or("No video open", |n| n), &p.fonts.body, rect(146.0, 16.0, 620.0, 48.0), if name.is_some() { TEXT } else { MUTED });
    p.text(c.info.as_deref().map_or("", |info| info), &p.fonts.small, INFO, MUTED);

    p.round(TARGET_ROW, 8.0, PANEL);
    p.text("Compress to", &p.fonts.body, rect(TARGET_ROW.left + 16.0, TARGET_ROW.top, TABS.left - 8.0, TARGET_ROW.bottom), MUTED);
    p.round(TABS, 7.0, FIELD);

    for (r, item, label, selected) in [(QUALITY_TAB, Item::QualityTab, "Quality", !fit_size), (SIZE_TAB, Item::SizeTab, "File size", fit_size)] {
        let fill = match (selected, hovered(item)) {
            (true, _) => BUTTON_HOVER,
            (false, true) => PANEL_HOVER,
            (false, false) => FIELD,
        };

        p.round(r, 6.0, fill);
        p.text(label, &p.fonts.button, r, if selected { TEXT } else { MUTED });
    }

    let (label, value, preset, note) = match fit_size {
        true => {
            let note = match &c.probe {
                Some(probe) => match probe.fit_bitrate(c.choices.size) {
                    Ok(bitrate) => format!("about {} video for this file", format_bitrate(bitrate as u64)),
                    Err(e) => e.message(),
                },
                None => "Lands just under this size".to_string(),
            };

            ("File size", format_megabytes(c.choices.size), "MB  ▾".to_string(), note)
        }
        false => {
            let preset = QUALITY_PRESETS.iter().find(|&&(_, value)| value == c.choices.quality).map_or("Custom", |&(name, _)| name);

            let note = match (&c.expected, &c.estimate, &c.file) {
                (Some((quality, bytes)), _, Some(source)) if *quality == c.choices.quality => match fs::metadata(source) {
                    Ok(original) => format!("≈ {}, about {:.0}% {}", format_bytes(*bytes), ((1.0 - *bytes as f64 / original.len() as f64) * 100.0).abs(), if *bytes < original.len() { "smaller" } else { "bigger" }),
                    Err(e) => format!("Cannot read the source: {e}"),
                },
                (_, Some(_), _) => "Estimating size…".to_string(),
                _ => "Lower = smaller file".to_string(),
            };

            ("Quality", c.choices.quality.to_string(), format!("{preset}  ▾"), note)
        }
    };

    p.round(VALUE_ROW, 8.0, PANEL);
    p.text(label, &p.fonts.body, rect(VALUE_ROW.left + 16.0, VALUE_ROW.top, VALUE_FIELD.left - 8.0, VALUE_ROW.bottom), MUTED);
    p.field(VALUE_FIELD, &value, idle, hovered(Item::Value), c.edit.is_some());
    p.button(PRESETS, &preset, idle, hovered(Item::Presets));
    p.text(&note, &p.fonts.small, rect(PRESETS.right + 16.0, VALUE_ROW.top, VALUE_ROW.right - 16.0, VALUE_ROW.bottom), MUTED);

    p.round(REPLACE_ROW, 8.0, if hovered(Item::Replace) { PANEL_HOVER } else { PANEL });
    p.text("Replace original  (moves it to the Recycle Bin)", &p.fonts.body, rect(REPLACE_ROW.left + 16.0, REPLACE_ROW.top, REPLACE_ROW.right - 72.0, REPLACE_ROW.bottom), MUTED);

    let cy = (REPLACE_ROW.top + REPLACE_ROW.bottom) / 2.0;

    p.switch(rect(REPLACE_ROW.right - 56.0, cy - 11.0, REPLACE_ROW.right - 16.0, cy + 11.0), c.choices.replace, idle);

    let explanation = match fit_size {
        true => "Picks the bitrate that fits, and re-encodes once if it lands over.",
        false => "Keeps quality where the picture moves and saves space where it doesn't.",
    };

    p.text(explanation, &p.fonts.small, rect(HINT.left, HINT.top, HINT.right, HINT.top + 20.0), MUTED);
    p.text(
        if c.choices.replace { "The compressed file takes the original's name." } else { "Saved next to the original as \"name (compressed).mp4\"." },
        &p.fonts.small,
        rect(HINT.left, HINT.top + 20.0, HINT.right, HINT.bottom),
        MUTED,
    );

    let (status, color) = match (&c.job, &c.status) {
        (Some((_, job)), _) => (format!("Compressing…  {:.0}%", job.progress() * 100.0), TEXT),
        (None, Status::Done(_, summary)) => (format!("Saved  ·  {summary}  ·  click to show"), ACCENT_HOVER),
        (None, Status::Info(message)) => (message.clone(), TEXT),
        (None, Status::Hint) if c.file.is_none() => ("Open a video to start.".to_string(), MUTED),
        (None, Status::Hint) => ("Ready.".to_string(), MUTED),
    };

    p.text(&status, &p.fonts.body, STATUS, color);

    let label = match &c.job {
        Some((_, job)) => format!("Cancel  {:.0}%", job.progress() * 100.0),
        None => "Compress".to_string(),
    };

    p.primary_button(START, &label, c.file.is_some(), hovered(Item::Start));
}

fn file_label(path: &Path) -> Result<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| Error::new(E_FAIL, format!("{} has no file name", path.display())))
}
