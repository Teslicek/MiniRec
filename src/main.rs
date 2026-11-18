#![windows_subsystem = "windows"]

mod audio;
mod compress;
mod cursor;
mod devices;
mod draw;
mod edit;
mod encoder;
mod export;
mod hotkeys;
mod icon;
mod keybinds;
mod player;
mod programs;
mod recorder;
mod replay;
mod settings;
mod shell;
mod trim;
mod ui;
mod units;
mod video;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLACK_BRUSH, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, GetStockObject, HBRUSH, HDC, InvalidateRect};
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoIncrementMTAUsage, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Error, HSTRING, PCWSTR, Result, w};

use devices::{AudioDevice, Flow};
use draw::{Canvas, to_pixels};
use edit::{InlineEdit, WM_EDIT_DONE, edit_colors};
use icon::icon_pixels;
use recorder::{PreviewSession, Session, Target, WM_PREVIEW_FAILED, WM_SESSION_FAILED, recording_path};
use settings::{CompressSettings, OUTPUT_HEIGHTS, Settings, bitrate_presets, create_folder, fps_presets};
use shell::*;
use hotkeys::{Action, Bindings, Hotkey, label_of};
use ui::{Element, HEIGHT, View, WIDTH, preview_rect};
use units::{BitrateUnit, format_amount, TimeUnit, format_bytes, format_duration, format_length, parse_bitrate_input, parse_fps, parse_replay_length};
use video::{MonitorInfo, Source, fixed_size, monitors, output_size};

const APP_NAME: &str = "MiniRec";
const MAIN_CLASS: PCWSTR = w!("MiniRecWindow");
const INSTANCE_WAIT_STEPS: u32 = 50;
const MAIN_STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);
const WM_TRAY: u32 = WM_APP + 1;
const WM_SHOW_APP: u32 = WM_APP + 4;
const WM_CLIP_SAVED: u32 = WM_APP + 5;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;
const TIMER_ID: usize = 1;
const MENU_SHOW: usize = 1;
const MENU_TOGGLE: usize = 2;
const MENU_TRIM: usize = 3;
const MENU_FOLDER: usize = 4;
const MENU_EXIT: usize = 5;
const MENU_CLIP: usize = 6;
const MENU_COMPRESS: usize = 7;
const APP_ICON: PCWSTR = PCWSTR(1 as *const u16);
const RECORDING_ICON_SIZE: u32 = 32;

struct Devices {
    outputs:        Vec<AudioDevice>,
    inputs:         Vec<AudioDevice>,
    default_output: Option<AudioDevice>,
    default_input:  Option<AudioDevice>,
}

impl Devices {
    fn load() -> Result<Self> {
        Ok(Self {
            outputs: devices::list(Flow::Output)?,
            inputs: devices::list(Flow::Input)?,
            default_output: devices::default_device(Flow::Output)?,
            default_input: devices::default_device(Flow::Input)?,
        })
    }

    fn of(&self, flow: Flow) -> (&[AudioDevice], &Option<AudioDevice>) {
        match flow {
            Flow::Output => (&self.outputs, &self.default_output),
            Flow::Input => (&self.inputs, &self.default_input),
        }
    }

    fn label(&self, flow: Flow, choice: &Option<String>) -> String {
        let (list, default) = self.of(flow);

        match (choice, default) {
            (None, Some(device)) => device.name.clone(),
            (None, None) => "None connected".to_string(),
            (Some(id), _) => match list.iter().find(|d| &d.id == id) {
                Some(device) => device.name.clone(),
                None => "Disconnected device".to_string(),
            },
        }
    }
}

struct App {
    window:         HWND,
    preview_window: HWND,
    monitors:       Vec<MonitorInfo>,
    devices:        Devices,
    settings:       Settings,
    session:        Option<Session>,
    recording:      Option<Instant>,
    clip_job:       Option<JoinHandle<Result<PathBuf>>>,
    registered:     Bindings,
    latest:         Option<PathBuf>,
    timer_on:       bool,
    preview:        Option<PreviewSession>,
    preview_failed: bool,
    program_note:   Option<String>,
    show_preview:   Arc<AtomicBool>,
    tray_icon:      HICON,
    recording_icon: HICON,
    hand_cursor:    HCURSOR,
    last_file:      Option<PathBuf>,
    canvas:         Canvas,
    hover:          Option<Element>,
    pressed:        Option<Element>,
    tracking_mouse: bool,
    edit:           Option<InlineEdit<Element>>,
}

impl App {
    fn monitor(&self) -> Result<&MonitorInfo> {
        find_monitor(&self.monitors, &self.settings.monitor)
    }

    fn source(&self) -> Result<Source> {
        match &self.settings.capture_program {
            Some(process) => Ok(Source::Program(programs::find(process)?)),
            None => Ok(Source::Monitor(self.monitor()?.clone())),
        }
    }

    fn capture_label(&self) -> Result<String> {
        match &self.settings.capture_program {
            Some(process) => Ok(format!("Program · {process}")),
            None => Ok(self.monitor()?.label()),
        }
    }

    fn output_size(&self) -> Result<(u32, u32)> {
        let monitor = self.monitor()?;

        Ok(output_size((monitor.width, monitor.height), self.settings.output))
    }

    fn place_preview(&self) -> Result<()> {
        let (width, height) = match (&self.session, &self.preview) {
            (Some(session), _) => session.size,
            (None, Some(preview)) => preview.size,
            (None, None) => self.output_size()?,
        };

        place_child(self.preview_window, to_pixels(preview_rect(width, height), self.canvas.scale()))?;

        let visible = self.preview.is_some() || self.session.is_some();

        unsafe { _ = ShowWindow(self.preview_window, if visible { SW_SHOWNA } else { SW_HIDE }) };
        self.invalidate()
    }

    fn target(&self) -> Target {
        Target { window: self.window, preview: self.preview_window, show_preview: self.show_preview.clone() }
    }

    fn invalidate(&self) -> Result<()> {
        unsafe { InvalidateRect(Some(self.window), None, false).ok() }
    }

    fn save_settings(&self) -> Result<()> {
        self.settings.save()?;
        self.invalidate()
    }

    fn sync_preview(&mut self) -> Result<()> {
        let visible = unsafe { IsWindowVisible(self.window).as_bool() && !IsIconic(self.window).as_bool() };
        let wanted = visible && self.session.is_none() && !self.preview_failed;

        self.show_preview.store(visible, Ordering::Relaxed);

        match (wanted, self.preview.take()) {
            (true, None) => match self.preview_source()? {
                Some(source) => match PreviewSession::start(&source, self.settings.cursor, &self.target()) {
                    Ok(session) => self.preview = Some(session),
                    Err(e) => {
                        self.preview_failed = true;
                        self.invalidate()?;
                        return Err(e);
                    }
                },
                None => {}
            },
            (false, Some(session)) => session.stop()?,
            (_, session) => self.preview = session,
        }

        self.place_preview()
    }

    fn preview_source(&mut self) -> Result<Option<Source>> {
        let Some(process) = &self.settings.capture_program else {
            self.program_note = None;
            return Ok(Some(Source::Monitor(self.monitor()?.clone())));
        };
        let (note, source) = match programs::running(process)? {
            None => (Some(format!("{process} is not running")), None),
            Some(program) if programs::minimized(&program) => (Some(format!("{process} is minimized")), None),
            Some(program) => (None, Some(Source::Program(program))),
        };

        self.program_note = note;
        self.invalidate()?;
        Ok(source)
    }

    fn restart_preview(&mut self) -> Result<()> {
        if let Some(session) = self.preview.take() {
            session.stop()?;
        }

        self.preview_failed = false;
        self.invalidate()?;
        self.sync_preview()
    }

    fn refresh_monitors(&mut self) -> Result<()> {
        self.monitors = monitors()?;

        if self.monitor().is_err() {
            self.settings.monitor = primary_monitor(&self.monitors)?.name.clone();
            self.save_settings()?;
        }

        Ok(())
    }

    fn refresh_latest(&mut self) -> Result<()> {
        let folder = &self.settings.folder;
        let latest = match folder.exists() {
            false => None,
            true => std::fs::read_dir(folder)
                .map_err(|e| Error::new(E_FAIL, format!("Cannot read {}: {e}", folder.display())))?
                .map(|entry| entry.map_err(|e| Error::new(E_FAIL, format!("Cannot read {}: {e}", folder.display()))))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext.eq_ignore_ascii_case("mp4")))
                .filter(|entry| !entry.file_name().to_string_lossy().contains("(estimate)") && !entry.file_name().to_string_lossy().contains("(compressing)"))
                .map(|entry| entry.metadata().and_then(|meta| meta.modified()).map(|time| (time, entry.path())).map_err(|e| Error::new(E_FAIL, format!("Cannot read {}: {e}", entry.path().display()))))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .max_by_key(|(time, _)| *time)
                .map(|(_, path)| path),
        };

        if latest != self.latest {
            self.latest = latest;
            self.invalidate()?;
        }

        Ok(())
    }

    fn refresh_devices(&mut self) -> Result<()> {
        self.devices = Devices::load()?;
        self.invalidate()
    }

    fn ensure_session(&mut self) -> Result<()> {
        if self.session.is_some() {
            return Ok(());
        }

        if let Some(preview) = self.preview.take() {
            preview.stop()?;
        }

        let replay = self.settings.replay.then_some(self.settings.replay_seconds);

        match Session::start(&self.source()?, &self.settings, &self.target(), replay) {
            Ok(session) => {
                let missing_mic = session.missing_mic;

                self.session = Some(session);
                self.place_preview()?;

                if missing_mic {
                    tray_balloon(self, "No microphone found", "Capturing without microphone audio.")?;
                }

                self.update_timer()
            }
            Err(e) => self.sync_preview().and(Err(e)),
        }
    }

    fn sync_session(&mut self) -> Result<()> {
        let wanted = self.recording.is_some() || self.settings.replay;

        match (wanted, self.session.take()) {
            (true, None) => self.ensure_session()?,
            (false, Some(session)) => session.stop()?,
            (_, session) => self.session = session,
        }

        self.update_timer()?;
        self.sync_preview()
    }

    fn capture_changed(&mut self, preview: bool) -> Result<()> {
        if self.recording.is_none()
            && let Some(session) = self.session.take()
        {
            session.stop()?;
            return self.sync_session();
        }

        match preview {
            true => self.restart_preview(),
            false => Ok(()),
        }
    }

    fn update_timer(&mut self) -> Result<()> {
        let wanted = self.recording.is_some() || (self.settings.replay && self.session.is_some());

        unsafe {
            match (wanted, self.timer_on) {
                (true, false) => {
                    if SetTimer(Some(self.window), TIMER_ID, 1000, None) == 0 {
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

    fn activity(&self) -> Result<Option<String>> {
        if self.clip_job.is_some() {
            return Ok(Some("Saving replay clip…".to_string()));
        }

        if let (true, Some(session)) = (self.settings.replay, &self.session)
            && let Some(bytes) = session.replay_bytes()?
        {
            let save = label_of(&self.settings.hotkeys, Action::Clip).map_or(String::new(), |hotkey| format!("  ·  {hotkey} saves"));

            return Ok(Some(format!("Replay on  ·  last {}{save}  ·  {} in RAM", format_duration(self.settings.replay_seconds), format_bytes(bytes as u64))));
        }

        Ok(None)
    }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

fn main() {
    if let Err(e) = run() {
        error_box(None, &e);
    }
}

fn show_running_instance() -> Result<()> {
    for _ in 0..INSTANCE_WAIT_STEPS {
        if let Ok(existing) = unsafe { FindWindowW(MAIN_CLASS, None) } {
            return unsafe { PostMessageW(Some(existing), WM_SHOW_APP, WPARAM(0), LPARAM(0)) };
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    Err(Error::new(E_FAIL, "MiniRec is already running but did not open its window within 5 seconds."))
}

fn run() -> Result<()> {
    unsafe {
        let mutex = CreateMutexW(None, false, w!("MiniRec.SingleInstance"))?;

        match WaitForSingleObject(mutex, 0) {
            WAIT_OBJECT_0 | WAIT_ABANDONED => {}
            WAIT_TIMEOUT => return show_running_instance(),
            _ => return Err(Error::from_win32()),
        }

        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;

        let _mta = CoIncrementMTAUsage()?;

        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;

        let result = create_and_run();

        MFShutdown()?;
        result
    }
}

fn create_and_run() -> Result<()> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let arrow = LoadCursorW(None, IDC_ARROW)?;
        let main_class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hIcon: LoadIconW(Some(instance), APP_ICON)?,
            hCursor: arrow,
            lpszClassName: MAIN_CLASS,
            ..Default::default()
        };
        let video_class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(video_proc),
            hInstance: instance,
            hCursor: arrow,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: VIDEO_CLASS,
            ..Default::default()
        };

        if RegisterClassExW(&main_class) == 0 || RegisterClassExW(&video_class) == 0 {
            return Err(Error::from_win32());
        }

        let window = CreateWindowExW(WINDOW_EX_STYLE(0), MAIN_CLASS, w!("MiniRec"), MAIN_STYLE, CW_USEDEFAULT, CW_USEDEFAULT, 800, 800, None, None, Some(instance), None)?;

        dark_title(window)?;

        let preview_window = CreateWindowExW(WINDOW_EX_STYLE(0), VIDEO_CLASS, None, WS_CHILD | WS_VISIBLE, 0, 0, 0, 0, Some(window), None, Some(instance), None)?;
        let dpi = GetDpiForWindow(window);
        let monitors = monitors()?;
        let mut settings = Settings::load(primary_monitor(&monitors)?)?;

        if find_monitor(&monitors, &settings.monitor).is_err() {
            settings.monitor = primary_monitor(&monitors)?.name.clone();
        }

        let (registered, hotkey_failures) = hotkeys::register(window, &settings.hotkeys);
        TASKBAR_CREATED.set(RegisterWindowMessageW(w!("TaskbarCreated")));

        let tray_size = GetSystemMetricsForDpi(SM_CXSMICON, dpi);
        let app = App {
            window,
            preview_window,
            monitors,
            devices: Devices::load()?,
            settings,
            session: None,
            recording: None,
            clip_job: None,
            registered,
            latest: None,
            timer_on: false,
            preview: None,
            preview_failed: false,
            program_note: None,
            show_preview: Arc::new(AtomicBool::new(false)),
            tray_icon: HICON(LoadImageW(Some(instance), APP_ICON, IMAGE_ICON, tray_size, tray_size, LR_DEFAULTCOLOR)?.0),
            recording_icon: create_icon(RECORDING_ICON_SIZE, &icon_pixels(RECORDING_ICON_SIZE, true))?,
            hand_cursor: LoadCursorW(None, IDC_HAND)?,
            last_file: None,
            canvas: Canvas::new(dpi)?,
            hover: None,
            pressed: None,
            tracking_mouse: false,
            edit: None,
        };

        tray_add(&app)?;
        APP.set(Some(app));
        fit_client(window, dpi, WIDTH, HEIGHT, MAIN_STYLE)?;
        place_preview()?;
        show_window(window)?;
        with_app(|app| app.sync_session())?;

        if !hotkey_failures.is_empty() {
            error_box(Some(window), &Error::new(E_FAIL, format!("These shortcuts are used by another app and were not set:\n{}\n\nChange them in Keybinds.", hotkey_failures.join("\n"))));
        }

        let mut message = MSG::default();

        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        Ok(())
    }
}

extern "system" fn video_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ready = APP.with(|cell| matches!(cell.try_borrow().as_deref(), Ok(Some(_))));

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

fn handle(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Result<Option<LRESULT>> {
    match message {
        WM_PAINT => with_app(|app| {
            let view = View {
                settings: &app.settings,
                resolution: match &app.session {
                    Some(session) => session.size,
                    None => app.output_size()?,
                },
                output: output_label(app.settings.output),
                system_device: (app.devices.label(Flow::Output, &app.settings.system_device), app.settings.system_device.is_none()),
                mic_device: (app.devices.label(Flow::Input, &app.settings.microphone_device), app.settings.microphone_device.is_none()),
                elapsed: app.recording.map(|started| started.elapsed().as_secs()),
                record_hotkey: label_of(&app.settings.hotkeys, Action::Record),
                capture: app.capture_label()?,
                program_note: app.program_note.clone().filter(|_| app.session.is_none()),
                latest: app.latest.as_deref().map(file_name).transpose()?,
                activity: app.activity()?,
                hover: app.hover,
                editing: app.edit.as_ref().map(|edit| edit.target),
                preview_failed: app.preview_failed,
            };

            app.canvas.paint(window, |p| ui::draw(p, &view))
        })?,
        WM_ERASEBKGND => return Ok(Some(LRESULT(1))),
        WM_SIZE => with_app(|app| {
            if wparam.0 as u32 != SIZE_MINIMIZED {
                app.canvas.resize(low_word(lparam.0), high_word(lparam.0))?;
            }

            app.sync_preview()
        })?,
        WM_DPICHANGED => {
            let suggested = unsafe { *(lparam.0 as *const RECT) };
            let dpi = high_word(wparam.0 as isize);

            with_app(|app| {
                app.canvas.set_dpi(dpi);
                Ok(())
            })?;
            unsafe { SetWindowPos(window, None, suggested.left, suggested.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)? };
            fit_client(window, dpi, WIDTH, HEIGHT, MAIN_STYLE)?;
            place_preview()?;
        }
        WM_DISPLAYCHANGE => {
            with_app(|app| {
                app.refresh_monitors()?;

                match app.recording {
                    Some(_) => Ok(()),
                    None => app.capture_changed(true),
                }
            })?;
            place_preview()?;
        }
        WM_CTLCOLOREDIT => return Ok(Some(edit_colors(HDC(wparam.0 as *mut _)))),
        WM_EDIT_DONE => finish_edit(wparam.0 == 1)?,
        WM_MOUSEMOVE => {
            let element = hit_test(lparam)?;

            with_app(|app| {
                if !app.tracking_mouse {
                    let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: window, dwHoverTime: 0 };

                    unsafe { TrackMouseEvent(&mut track)? };
                    app.tracking_mouse = true;
                }

                if app.hover != element {
                    app.hover = element;
                    app.invalidate()?;
                }

                Ok(())
            })?;
        }
        WM_MOUSELEAVE => with_app(|app| {
            app.tracking_mouse = false;
            app.hover = None;
            app.invalidate()
        })?,
        WM_SETCURSOR if low_word(lparam.0) == HTCLIENT => {
            let hand = with_app(|app| Ok(app.hover.filter(|e| e.enabled(app.recording.is_some())).map(|_| app.hand_cursor)))?;
            let Some(cursor) = hand else {
                return Ok(None);
            };

            unsafe { SetCursor(Some(cursor)) };
            return Ok(Some(LRESULT(1)));
        }
        WM_LBUTTONDOWN => {
            unsafe { _ = SetFocus(Some(window)) };

            let element = hit_test(lparam)?;

            with_app(|app| {
                app.pressed = element.filter(|e| e.enabled(app.recording.is_some()));
                Ok(())
            })?;
        }
        WM_LBUTTONUP => {
            let element = hit_test(lparam)?;
            let pressed = with_app(|app| Ok(app.pressed.take()))?;

            if let Some(element) = element.filter(|e| Some(*e) == pressed) {
                activate(window, element)?;
            }
        }
        WM_CLOSE => exit(window)?,
        WM_TRAY => on_tray(window, lparam)?,
        WM_HOTKEY => match Action::from_id(wparam.0 as i32) {
            Some(action) => run_action(window, action)?,
            None => return Ok(None),
        },
        WM_ACTIVATE if low_word(wparam.0 as isize) != WA_INACTIVE => with_app(|app| {
            app.refresh_latest()?;

            match app.program_note.is_some() {
                true => app.sync_preview(),
                false => Ok(()),
            }
        })?,
        WM_TIMER => with_app(|app| {
            tray_update(app)?;
            app.invalidate()
        })?,
        WM_SESSION_FAILED => with_app(session_failed)?,
        WM_CLIP_SAVED => with_app(clip_saved)?,
        WM_PREVIEW_FAILED => with_app(|app| {
            app.preview_failed = true;
            app.invalidate()?;

            match app.preview.take() {
                Some(session) => session.stop(),
                None => Ok(()),
            }
        })?,
        WM_SHOW_APP => show_window(window)?,
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        m if m == TASKBAR_CREATED.get() => with_app(|app| tray_add(app))?,
        _ => return Ok(None),
    }

    Ok(Some(LRESULT(0)))
}

fn with_app<R>(f: impl FnOnce(&mut App) -> Result<R>) -> Result<R> {
    APP.with_borrow_mut(|app| f(app.as_mut().ok_or_else(|| Error::new(E_FAIL, "App state is not initialized"))?))
}

fn hit_test(lparam: LPARAM) -> Result<Option<Element>> {
    let (x, y) = mouse_point(lparam.0, with_app(|app| Ok(app.canvas.scale()))?);

    Ok(Element::at(x, y))
}

fn activate(window: HWND, element: Element) -> Result<()> {
    match element {
        Element::Record => with_app(toggle),
        Element::Trim => trim::open(&with_app(|app| Ok(app.settings.folder.clone()))?),
        Element::Compress => open_compressor(),
        Element::Keybinds => open_keybinds(),
        Element::Latest => match with_app(|app| Ok(app.latest.clone()))? {
            Some(path) => shell_open(&HSTRING::from(path.as_os_str()), PCWSTR::null()),
            None => Ok(()),
        },
        Element::Monitor => {
            let (labels, selected, programs, monitor_count) = with_app(|app| {
                app.refresh_monitors()?;

                let programs = programs::list()?;
                let selected: Vec<usize> = match &app.settings.capture_program {
                    Some(process) => programs.iter().position(|p| p.process.eq_ignore_ascii_case(process)).map(|i| i + app.monitors.len() + 1).into_iter().collect(),
                    None => app.monitors.iter().position(|m| m.name == app.settings.monitor).into_iter().collect(),
                };
                let labels: Vec<String> = app.monitors.iter().map(MonitorInfo::label).chain(std::iter::once(String::new())).chain(programs.iter().map(|p| p.label())).collect();

                Ok((labels, selected, programs, app.monitors.len()))
            })?;
            let Some(index) = menu_below(window, element, &labels, &selected)? else {
                return Ok(());
            };

            with_app(|app| {
                match index.checked_sub(monitor_count + 1) {
                    Some(program) => app.settings.capture_program = Some(programs[program].process.clone()),
                    None => {
                        app.settings.capture_program = None;
                        app.settings.monitor = app.monitors[index].name.clone();
                    }
                }

                app.save_settings()?;
                app.capture_changed(true)
            })?;
            place_preview()
        }
        Element::Output => {
            let options: Vec<Option<u32>> = std::iter::once(None).chain(OUTPUT_HEIGHTS.map(Some)).collect();
            let labels: Vec<String> = options.iter().map(|&output| output_label(output)).collect();
            let selected = with_app(|app| Ok(options.iter().position(|&output| output == app.settings.output).into_iter().collect::<Vec<_>>()))?;
            let Some(index) = menu_below(window, element, &labels, &selected)? else {
                return Ok(());
            };

            with_app(|app| {
                app.settings.output = options[index];
                app.save_settings()?;
                app.capture_changed(true)
            })?;
            place_preview()
        }
        Element::Fps | Element::Bitrate | Element::ReplayLength => open_edit(window, element),
        Element::FpsPresets => {
            let presets = with_app(|app| Ok(fps_presets(app.monitor()?)))?;
            let labels: Vec<String> = presets.iter().map(|fps| format!("{fps} FPS")).collect();
            let selected: Vec<usize> = with_app(|app| Ok(presets.iter().position(|&fps| fps == app.settings.fps).into_iter().collect()))?;
            let Some(index) = menu_below(window, element, &labels, &selected)? else {
                return Ok(());
            };

            with_app(|app| {
                app.settings.fps = presets[index];
                app.save_settings()?;
                app.capture_changed(false)
            })
        }
        Element::ReplayUnit => {
            let labels: Vec<String> = TimeUnit::ALL.iter().map(|unit| unit.label().to_string()).collect();
            let selected = with_app(|app| Ok(TimeUnit::ALL.iter().position(|&unit| unit == app.settings.replay_unit).into_iter().collect::<Vec<_>>()))?;
            let Some(index) = menu_below(window, element, &labels, &selected)? else {
                return Ok(());
            };

            with_app(|app| {
                app.settings.replay_unit = TimeUnit::ALL[index];
                app.save_settings()
            })
        }
        Element::BitrateUnit => {
            let (presets, checked) = with_app(|app| {
                let presets = bitrate_presets(app.output_size()?, app.settings.fps, app.settings.bitrate_unit);
                let unit = BitrateUnit::ALL.iter().position(|&unit| unit == app.settings.bitrate_unit);
                let preset = presets.iter().position(|&(_, bits)| bits == app.settings.bitrate).map(|index| index + BitrateUnit::ALL.len() + 1);

                Ok((presets, unit.into_iter().chain(preset).collect::<Vec<_>>()))
            })?;
            let labels: Vec<String> = BitrateUnit::ALL
                .iter()
                .map(|unit| unit.label().to_string())
                .chain(std::iter::once(String::new()))
                .chain(presets.iter().map(|(label, _)| label.clone()))
                .collect();
            let Some(index) = menu_below(window, element, &labels, &checked)? else {
                return Ok(());
            };

            with_app(|app| {
                if let Some(&unit) = BitrateUnit::ALL.get(index) {
                    app.settings.bitrate_unit = unit;
                    return app.save_settings();
                }

                app.settings.bitrate = presets[index - BitrateUnit::ALL.len() - 1].1;
                app.save_settings()?;
                app.capture_changed(false)
            })
        }
        Element::SystemDevice => choose_device(window, element, Flow::Output),
        Element::MicDevice => choose_device(window, element, Flow::Input),
        Element::SystemToggle => with_app(|app| {
            app.settings.system_audio = !app.settings.system_audio;
            app.save_settings()?;
            app.capture_changed(false)
        }),
        Element::MicToggle => with_app(|app| {
            app.settings.microphone = !app.settings.microphone;
            app.save_settings()?;
            app.capture_changed(false)
        }),
        Element::Cursor => with_app(|app| {
            app.settings.cursor = !app.settings.cursor;
            app.save_settings()?;
            app.capture_changed(true)
        }),
        Element::ReplayToggle => with_app(toggle_replay),
        Element::FolderChange => {
            let current = with_app(|app| Ok(app.settings.folder.clone()))?;
            let Some(folder) = pick_folder(window, &current)? else {
                return Ok(());
            };

            with_app(|app| {
                app.settings.folder = folder;
                app.save_settings()?;
                app.refresh_latest()
            })
        }
        Element::FolderOpen => open_folder(&with_app(|app| Ok(app.settings.folder.clone()))?),
    }
}

fn choose_device(window: HWND, element: Element, flow: Flow) -> Result<()> {
    let (labels, ids, selected) = with_app(|app| {
        app.refresh_devices()?;

        let (list, default) = app.devices.of(flow);
        let current = match flow {
            Flow::Output => &app.settings.system_device,
            Flow::Input => &app.settings.microphone_device,
        };
        let default_label = match default {
            Some(device) => format!("Windows default  ({})", device.name),
            None => "Windows default  (none connected)".to_string(),
        };
        let labels: Vec<String> = std::iter::once(default_label).chain(list.iter().map(|d| d.name.clone())).collect();
        let ids: Vec<Option<String>> = std::iter::once(None).chain(list.iter().map(|d| Some(d.id.clone()))).collect();
        let selected: Vec<usize> = ids.iter().position(|id| id == current).into_iter().collect();

        Ok((labels, ids, selected))
    })?;
    let Some(index) = menu_below(window, element, &labels, &selected)? else {
        return Ok(());
    };

    with_app(|app| {
        let choice = ids[index].clone();

        match flow {
            Flow::Output => app.settings.system_device = choice,
            Flow::Input => app.settings.microphone_device = choice,
        }

        app.save_settings()?;
        app.capture_changed(false)
    })
}

fn menu_below(window: HWND, element: Element, labels: &[String], selected: &[usize]) -> Result<Option<usize>> {
    let scale = with_app(|app| Ok(app.canvas.scale()))?;
    let anchor = element.rect();
    let point = POINT { x: (anchor.left * scale).round() as i32, y: ((anchor.bottom + 4.0) * scale).round() as i32 };

    popup_menu(window, point, labels, selected)
}

fn open_edit(window: HWND, element: Element) -> Result<()> {
    let (area, text, scale) = with_app(|app| {
        let text = match element {
            Element::Fps => app.settings.fps.to_string(),
            Element::Bitrate => format_amount(app.settings.bitrate, app.settings.bitrate_unit),
            Element::ReplayLength => format_length(app.settings.replay_seconds, app.settings.replay_unit),
            _ => return Err(Error::new(E_FAIL, "This setting cannot be typed")),
        };

        Ok((element.edit_area(), text, app.canvas.scale()))
    })?;
    let edit = InlineEdit::open(window, to_pixels(area, scale), &text, scale, element)?;

    with_app(|app| {
        app.edit = Some(edit);
        app.invalidate()
    })
}

fn finish_edit(commit: bool) -> Result<()> {
    let Some(edit) = with_app(|app| Ok(app.edit.take()))? else {
        return Ok(());
    };
    let text = edit.text()?;
    let element = edit.target;

    edit.close()?;
    with_app(|app| {
        app.invalidate()?;

        if !commit {
            return Ok(());
        }

        match element {
            Element::Fps => app.settings.fps = parse_fps(&text)?,
            Element::Bitrate => app.settings.bitrate = parse_bitrate_input(&text, app.settings.bitrate_unit)?,
            Element::ReplayLength => app.settings.replay_seconds = parse_replay_length(&text, app.settings.replay_unit)?,
            _ => return Err(Error::new(E_FAIL, "This setting cannot be typed")),
        }

        app.save_settings()?;

        match element {
            Element::Fps | Element::Bitrate => app.capture_changed(false),
            Element::ReplayLength => match &app.session {
                Some(session) if app.settings.replay => session.set_replay(Some(app.settings.replay_seconds)),
                _ => Ok(()),
            },
            _ => Ok(()),
        }
    })
}

fn toggle(app: &mut App) -> Result<()> {
    match app.recording {
        Some(_) => stop_recording(app),
        None => start_recording(app),
    }
}

fn start_recording(app: &mut App) -> Result<()> {
    create_folder(&app.settings.folder)?;
    app.recording = Some(Instant::now());

    if let Err(e) = app.ensure_session() {
        app.recording = None;
        return Err(e);
    }

    app.session.as_ref().ok_or_else(|| Error::new(E_FAIL, "Capture did not start"))?.begin_recording(recording_path(&app.settings.folder, ""))?;
    app.update_timer()?;
    tray_update(app)?;
    app.invalidate()
}

fn stop_recording(app: &mut App) -> Result<()> {
    if app.recording.take().is_none() {
        return Ok(());
    }

    let saved = app.session.as_ref().ok_or_else(|| Error::new(E_FAIL, "Capture is not running"))?.end_recording();

    tray_update(app)?;
    app.invalidate()?;
    app.sync_session()?;

    match saved? {
        Some(path) => {
            app.last_file = Some(path.clone());
            tray_balloon(app, "Recording saved", &format!("{}\nClick to show in folder.", file_name(&path)?))
        }
        None => tray_balloon(app, "Nothing recorded", "The recording was stopped before its first frame was ready."),
    }
}

fn session_failed(app: &mut App) -> Result<()> {
    let Some(session) = app.session.take() else {
        return Ok(());
    };
    let saved = session.end_recording();
    let stopped = session.stop();
    let replay  = app.settings.replay;
    let mut notes = String::new();

    app.recording = None;

    if replay {
        app.settings.replay = false;
        app.save_settings()?;
    }

    tray_update(app)?;
    app.update_timer()?;
    app.sync_preview()?;

    if let Ok(Some(path)) = &saved {
        app.last_file = Some(path.clone());
        notes += &format!("\n\nThe recording up to that point was saved as {}.", file_name(path)?);
        app.refresh_latest()?;
    }

    if replay {
        notes += "\n\nThe replay buffer was turned off.";
    }

    stopped.and(saved.map(|_| ())).map_err(|e| Error::new(e.code(), format!("Capture stopped: {}{notes}", e.message())))
}

fn save_clip(app: &mut App) -> Result<()> {
    if !app.settings.replay {
        return tray_balloon(app, "Replay buffer is off", "Turn on Replay buffer in MiniRec to save clips.");
    }

    if app.clip_job.is_some() {
        return Ok(());
    }

    create_folder(&app.settings.folder)?;

    let path = recording_path(&app.settings.folder, "Clip ");
    let session = app.session.as_ref().ok_or_else(|| Error::new(E_FAIL, "The replay buffer is not running"))?;

    app.clip_job = Some(session.save_clip(app.window, WM_CLIP_SAVED, path)?);
    app.invalidate()
}

fn clip_saved(app: &mut App) -> Result<()> {
    let Some(job) = app.clip_job.take() else {
        return Ok(());
    };

    app.invalidate()?;

    let path = job.join().map_err(|_| Error::new(E_FAIL, "Clip saving thread panicked"))??;

    app.last_file = Some(path.clone());
    tray_balloon(app, "Replay clip saved", &format!("{}\nClick to show in folder.", file_name(&path)?))
}

fn file_name(path: &Path) -> Result<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| Error::new(E_FAIL, format!("{} has no file name", path.display())))
}

fn toggle_replay(app: &mut App) -> Result<()> {
    app.settings.replay = !app.settings.replay;
    app.save_settings()?;

    if let Some(session) = &app.session {
        session.set_replay(app.settings.replay.then_some(app.settings.replay_seconds))?;
    }

    app.sync_session()
}

fn run_action(window: HWND, action: Action) -> Result<()> {
    match action {
        Action::Record => with_app(toggle),
        Action::Clip => with_app(save_clip),
        Action::Replay => with_app(toggle_replay),
        Action::Show => show_window(window),
        Action::Trim => trim::open(&with_app(|app| Ok(app.settings.folder.clone()))?),
        Action::Compress => open_compressor(),
    }
}

fn open_keybinds() -> Result<()> {
    let bindings = with_app(|app| Ok(app.settings.hotkeys))?;

    keybinds::open(bindings, keybinds::Callbacks { apply: apply_hotkey, suspend: suspend_hotkeys })
}

fn apply_hotkey(action: Action, hotkey: Option<Hotkey>) -> Result<Bindings> {
    with_app(|app| {
        if let Some(hotkey) = hotkey
            && let Some(other) = Action::ALL.into_iter().find(|&other| other != action && app.settings.hotkeys[other.index()] == Some(hotkey))
        {
            return Err(Error::new(E_FAIL, format!("{} is already used for \"{}\".", hotkey.label(), other.label())));
        }

        app.settings.hotkeys[action.index()] = hotkey;
        app.save_settings()?;
        tray_update(app)?;
        Ok(app.settings.hotkeys)
    })
}

fn suspend_hotkeys(suspend: bool) -> Result<()> {
    with_app(|app| {
        hotkeys::unregister(app.window, &app.registered)?;
        app.registered = [None; 6];

        if suspend {
            return Ok(());
        }

        let (registered, failures) = hotkeys::register(app.window, &app.settings.hotkeys);

        app.registered = registered;

        match failures.is_empty() {
            true => Ok(()),
            false => Err(Error::new(E_FAIL, format!("These shortcuts are used by another app:\n{}", failures.join("\n")))),
        }
    })
}

fn open_compressor() -> Result<()> {
    let (folder, choices) = with_app(|app| Ok((app.settings.folder.clone(), app.settings.compress)))?;

    compress::open(&folder, choices, remember_compression)
}

fn remember_compression(choices: CompressSettings) -> Result<()> {
    with_app(|app| {
        app.settings.compress = choices;
        app.settings.save()
    })
}

fn show_window(window: HWND) -> Result<()> {
    unsafe {
        _ = ShowWindow(window, if IsIconic(window).as_bool() { SW_RESTORE } else { SW_SHOW });
        _ = SetForegroundWindow(window);
    }

    with_app(|app| {
        app.refresh_devices()?;
        app.sync_preview()
    })
}

fn exit(window: HWND) -> Result<()> {
    let tools = trim::close().and(compress::close()).and(keybinds::close());
    let stopped = with_app(|app| {
        let recording = stop_recording(app);
        let clip = app.clip_job.take().map(|job| job.join().map_err(|_| Error::new(E_FAIL, "Clip saving thread panicked"))?.map(|_| ())).transpose();
        let session = app.session.take().map(Session::stop).transpose();
        let preview = app.preview.take().map(PreviewSession::stop).transpose();

        tray_delete(app)?;
        recording?;
        clip?;
        session?;
        preview.map(|_| ())
    });

    unsafe { DestroyWindow(window)? };
    tools.and(stopped)
}

fn output_label(output: Option<u32>) -> String {
    match output {
        Some(height) => {
            let (width, height) = fixed_size(height);

            format!("{height}p  ·  {width}×{height}")
        }
        None => "Source size".to_string(),
    }
}

fn place_preview() -> Result<()> {
    with_app(|app| app.place_preview())
}

fn on_tray(window: HWND, lparam: LPARAM) -> Result<()> {
    match low_word(lparam.0) {
        NIN_SELECT | NIN_KEYSELECT => show_window(window),
        WM_CONTEXTMENU => tray_menu(window),
        NIN_BALLOONUSERCLICK => match with_app(|app| Ok(app.last_file.clone()))? {
            Some(path) => reveal(&path),
            None => Ok(()),
        },
        _ => Ok(()),
    }
}

fn tray_menu(window: HWND) -> Result<()> {
    let recording = with_app(|app| Ok(app.recording.is_some()))?;
    let (hotkeys, recording) = with_app(|app| Ok((app.settings.hotkeys, recording)))?;
    let shortcut = |action: Action| label_of(&hotkeys, action).map_or(String::new(), |hotkey| format!("\t{hotkey}"));
    let toggle_label = match recording {
        true => format!("Stop recording{}", shortcut(Action::Record)),
        false => format!("Start recording{}", shortcut(Action::Record)),
    };

    unsafe {
        let menu = CreatePopupMenu()?;
        let mut point = POINT::default();

        AppendMenuW(menu, MF_STRING, MENU_SHOW, w!("Show MiniRec"))?;
        AppendMenuW(menu, MF_STRING, MENU_TOGGLE, &HSTRING::from(toggle_label))?;
        AppendMenuW(menu, MF_STRING, MENU_CLIP, &HSTRING::from(format!("Save replay clip{}", shortcut(Action::Clip))))?;
        AppendMenuW(menu, MF_STRING, MENU_TRIM, w!("Trim a video"))?;
        AppendMenuW(menu, MF_STRING, MENU_COMPRESS, w!("Compress a video"))?;
        AppendMenuW(menu, MF_STRING, MENU_FOLDER, w!("Open recordings folder"))?;
        AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
        AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit"))?;
        GetCursorPos(&mut point)?;
        _ = SetForegroundWindow(window);

        let command = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, point.x, point.y, Some(0), window, None).0 as usize;

        DestroyMenu(menu)?;

        match command {
            0 => Ok(()),
            MENU_SHOW => show_window(window),
            MENU_TOGGLE => with_app(toggle),
            MENU_CLIP => with_app(save_clip),
            MENU_TRIM => trim::open(&with_app(|app| Ok(app.settings.folder.clone()))?),
            MENU_COMPRESS => open_compressor(),
            MENU_FOLDER => open_folder(&with_app(|app| Ok(app.settings.folder.clone()))?),
            MENU_EXIT => exit(window),
            id => Err(Error::new(E_FAIL, format!("Unknown menu command {id}"))),
        }
    }
}

fn open_folder(folder: &Path) -> Result<()> {
    create_folder(folder)?;
    shell_open(&HSTRING::from(folder.as_os_str()), PCWSTR::null())
}

fn find_monitor<'a>(monitors: &'a [MonitorInfo], name: &str) -> Result<&'a MonitorInfo> {
    monitors.iter().find(|m| m.name == name).ok_or_else(|| Error::new(E_FAIL, format!("Monitor {name} is not connected")))
}

fn primary_monitor(monitors: &[MonitorInfo]) -> Result<&MonitorInfo> {
    monitors.iter().find(|m| m.primary).ok_or_else(|| Error::new(E_FAIL, "No primary monitor found"))
}

fn tray_data(app: &App) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: app.window,
        uID: 1,
        ..Default::default()
    }
}

fn tray_status(app: &App, data: &mut NOTIFYICONDATAW) {
    let tip = match &app.recording {
        Some(started) => {
            let seconds = started.elapsed().as_secs();

            format!("{APP_NAME}  REC {:02}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
        }
        None => match label_of(&app.settings.hotkeys, Action::Record) {
            Some(hotkey) => format!("{APP_NAME}  {hotkey} to record"),
            None => APP_NAME.to_string(),
        },
    };

    data.uFlags |= NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    data.hIcon = if app.recording.is_some() { app.recording_icon } else { app.tray_icon };
    copy_wide(&mut data.szTip, &tip);
}

fn tray_add(app: &App) -> Result<()> {
    let mut data = tray_data(app);

    data.uFlags = NIF_MESSAGE;
    data.uCallbackMessage = WM_TRAY;
    tray_status(app, &mut data);
    shell_notify(NIM_ADD, &data)?;
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    shell_notify(NIM_SETVERSION, &data)
}

fn tray_update(app: &App) -> Result<()> {
    let mut data = tray_data(app);

    tray_status(app, &mut data);
    shell_notify(NIM_MODIFY, &data)
}

fn tray_balloon(app: &App, title: &str, text: &str) -> Result<()> {
    let mut data = tray_data(app);

    data.uFlags = NIF_INFO;
    data.dwInfoFlags = NIIF_INFO;
    copy_wide(&mut data.szInfoTitle, title);
    copy_wide(&mut data.szInfo, text);
    shell_notify(NIM_MODIFY, &data)
}

fn tray_delete(app: &App) -> Result<()> {
    shell_notify(NIM_DELETE, &tray_data(app))
}

fn shell_notify(message: NOTIFY_ICON_MESSAGE, data: &NOTIFYICONDATAW) -> Result<()> {
    if !unsafe { Shell_NotifyIconW(message, data) }.as_bool() {
        return Err(Error::new(E_FAIL, format!("Shell_NotifyIcon failed for message {}", message.0)));
    }

    Ok(())
}

fn copy_wide(target: &mut [u16], text: &str) {
    let wide: Vec<u16> = text.encode_utf16().take(target.len() - 1).collect();

    target[..wide.len()].copy_from_slice(&wide);
    target[wide.len()] = 0;
}

fn create_icon(size: u32, pixels: &[[u8; 4]]) -> Result<HICON> {
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;

        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut [u8; 4], pixels.len());

        let mask = CreateBitmap(size as i32, size as i32, 1, 1, None);
        let icon_info = ICONINFO { fIcon: true.into(), hbmMask: mask, hbmColor: color, ..Default::default() };
        let icon = CreateIconIndirect(&icon_info);

        DeleteObject(mask.into()).ok()?;
        DeleteObject(color.into()).ok()?;
        icon
    }
}
