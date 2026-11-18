use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Error, PCWSTR, Result, w};

use crate::draw::*;
use crate::hotkeys::{Action, Bindings, Hotkey};
use crate::shell::{WM_MOUSELEAVE, dark_title, error_box, fit_client, high_word, low_word, mouse_point};

const CLASS: PCWSTR = w!("MiniRecKeybinds");
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0);
const WIDTH: f32 = 520.0;
const HEIGHT: f32 = 392.0;
const FIRST_ROW: f32 = 64.0;
const ROW_STEP: f32 = 52.0;
const ROW_HEIGHT: f32 = 44.0;

#[derive(Clone, Copy, PartialEq)]
enum Item {
    Field(Action),
    Clear(Action),
}

pub struct Callbacks {
    pub apply:   fn(Action, Option<Hotkey>) -> Result<Bindings>,
    pub suspend: fn(bool) -> Result<()>,
}

struct Keybinds {
    window:    HWND,
    canvas:    Canvas,
    bindings:  Bindings,
    callbacks: Callbacks,
    capturing: Option<Action>,
    hover:     Option<Item>,
    pressed:   Option<Item>,
    tracking:  bool,
    hand:      HCURSOR,
}

thread_local! {
    static KEYBINDS: RefCell<Option<Keybinds>> = const { RefCell::new(None) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
}

pub fn open(bindings: Bindings, callbacks: Callbacks) -> Result<()> {
    if let Some(window) = KEYBINDS.with_borrow(|k| k.as_ref().map(|k| k.window)) {
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

        let window = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("MiniRec Keybinds"), STYLE, CW_USEDEFAULT, CW_USEDEFAULT, 600, 450, None, None, Some(instance), None)?;

        dark_title(window)?;

        let dpi = GetDpiForWindow(window);

        KEYBINDS.set(Some(Keybinds { window, canvas: Canvas::new(dpi)?, bindings, callbacks, capturing: None, hover: None, pressed: None, tracking: false, hand: LoadCursorW(None, IDC_HAND)? }));
        fit_client(window, dpi, WIDTH, HEIGHT, STYLE)?;
        _ = ShowWindow(window, SW_SHOW);
        _ = SetForegroundWindow(window);
        Ok(())
    }
}

pub fn close() -> Result<()> {
    let Some(keybinds) = KEYBINDS.take() else {
        return Ok(());
    };

    if keybinds.capturing.is_some() {
        (keybinds.callbacks.suspend)(false)?;
    }

    unsafe { DestroyWindow(keybinds.window) }
}

extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ready = KEYBINDS.with(|cell| matches!(cell.try_borrow().as_deref(), Ok(Some(_))));

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

fn with<R>(f: impl FnOnce(&mut Keybinds) -> Result<R>) -> Result<R> {
    KEYBINDS.with_borrow_mut(|k| f(k.as_mut().ok_or_else(|| Error::new(E_FAIL, "Keybinds window is not open"))?))
}

fn handle(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Result<Option<LRESULT>> {
    match message {
        WM_PAINT => with(|k| k.canvas.paint(window, |p| draw(p, k)))?,
        WM_ERASEBKGND => return Ok(Some(LRESULT(1))),
        WM_SIZE => with(|k| k.canvas.resize(low_word(lparam.0), high_word(lparam.0)))?,
        WM_DPICHANGED => {
            let suggested = unsafe { *(lparam.0 as *const RECT) };
            let dpi = high_word(wparam.0 as isize);

            with(|k| {
                k.canvas.set_dpi(dpi);
                Ok(())
            })?;
            unsafe { SetWindowPos(window, None, suggested.left, suggested.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)? };
            fit_client(window, dpi, WIDTH, HEIGHT, STYLE)?;
        }
        WM_CLOSE => close()?,
        WM_ACTIVATE if low_word(wparam.0 as isize) == WA_INACTIVE => stop_capture()?,
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let Some(action) = with(|k| Ok(k.capturing))? else {
                return Ok(None);
            };
            let key = wparam.0 as u32;

            if key == VK_ESCAPE.0 as u32 {
                stop_capture()?;
                return Ok(Some(LRESULT(0)));
            }

            let Some(hotkey) = Hotkey::from_keyboard(key) else {
                return Ok(Some(LRESULT(0)));
            };

            if !hotkey.is_safe() {
                stop_capture()?;
                return Err(Error::new(E_INVALIDARG, format!("{} would block normal typing. Use Ctrl or Alt with a key, or an F-key.", hotkey.label())));
            }

            let available = hotkey.available(window);

            with(|k| {
                k.capturing = None;
                k.invalidate()?;

                let applied = match available {
                    true => (k.callbacks.apply)(action, Some(hotkey)),
                    false => Err(Error::new(E_FAIL, format!("{} is already used by another app.", hotkey.label()))),
                };
                let resumed = (k.callbacks.suspend)(false);

                k.bindings = applied?;
                resumed
            })?;
        }
        WM_SYSCHAR | WM_CHAR if with(|k| Ok(k.capturing.is_some()))? => {}
        WM_MOUSEMOVE => with(|k| {
            if !k.tracking {
                let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: window, dwHoverTime: 0 };

                unsafe { TrackMouseEvent(&mut track)? };
                k.tracking = true;
            }

            let (x, y) = mouse_point(lparam.0, k.canvas.scale());
            let hover = hit(x, y);

            if hover != k.hover {
                k.hover = hover;
                k.invalidate()?;
            }

            Ok(())
        })?,
        WM_MOUSELEAVE => with(|k| {
            k.tracking = false;
            k.hover = None;
            k.invalidate()
        })?,
        WM_SETCURSOR if low_word(lparam.0) == HTCLIENT => {
            let hand = with(|k| Ok(k.hover.filter(|&item| k.enabled(item)).map(|_| k.hand)))?;
            let Some(cursor) = hand else {
                return Ok(None);
            };

            unsafe { SetCursor(Some(cursor)) };
            return Ok(Some(LRESULT(1)));
        }
        WM_LBUTTONDOWN => {
            unsafe { _ = SetFocus(Some(window)) };
            with(|k| {
                let (x, y) = mouse_point(lparam.0, k.canvas.scale());

                k.pressed = hit(x, y).filter(|&item| k.enabled(item));
                Ok(())
            })?;
        }
        WM_LBUTTONUP => {
            let (released, pressed) = with(|k| {
                let (x, y) = mouse_point(lparam.0, k.canvas.scale());

                Ok((hit(x, y), k.pressed.take()))
            })?;

            if let Some(item) = released.filter(|&item| Some(item) == pressed) {
                activate(item)?;
            }
        }
        _ => return Ok(None),
    }

    Ok(Some(LRESULT(0)))
}

impl Keybinds {
    fn invalidate(&self) -> Result<()> {
        unsafe { InvalidateRect(Some(self.window), None, false).ok() }
    }

    fn enabled(&self, item: Item) -> bool {
        match item {
            Item::Field(_) => true,
            Item::Clear(action) => self.bindings[action.index()].is_some() && self.capturing.is_none(),
        }
    }
}

fn activate(item: Item) -> Result<()> {
    match item {
        Item::Field(action) => with(|k| {
            if k.capturing.is_none() {
                (k.callbacks.suspend)(true)?;
            }

            k.capturing = Some(action);
            k.invalidate()
        }),
        Item::Clear(action) => with(|k| {
            k.bindings = (k.callbacks.apply)(action, None)?;
            (k.callbacks.suspend)(false)?;
            k.invalidate()
        }),
    }
}

fn stop_capture() -> Result<()> {
    with(|k| {
        if k.capturing.take().is_some() {
            (k.callbacks.suspend)(false)?;
            k.invalidate()?;
        }

        Ok(())
    })
}

fn row_rect(index: usize) -> D2D_RECT_F {
    let top = FIRST_ROW + index as f32 * ROW_STEP;

    rect(20.0, top, 500.0, top + ROW_HEIGHT)
}

fn field_rect(row: D2D_RECT_F) -> D2D_RECT_F {
    rect(row.right - 236.0, row.top + 8.0, row.right - 52.0, row.bottom - 8.0)
}

fn clear_rect(row: D2D_RECT_F) -> D2D_RECT_F {
    rect(row.right - 44.0, row.top + 8.0, row.right - 8.0, row.bottom - 8.0)
}

fn hit(x: f32, y: f32) -> Option<Item> {
    Action::ALL.into_iter().enumerate().find_map(|(index, action)| {
        let row = row_rect(index);

        [(field_rect(row), Item::Field(action)), (clear_rect(row), Item::Clear(action))].into_iter().find(|(r, _)| contains(*r, x, y)).map(|(_, item)| item)
    })
}

fn draw(p: &Painter, k: &Keybinds) {
    let hovered = |item: Item| k.hover == Some(item) && k.enabled(item);

    p.clear(BACKGROUND);
    p.text("Click a shortcut, then press the new keys. Esc cancels, × removes it.", &p.fonts.small, rect(20.0, 16.0, 500.0, 36.0), MUTED);
    p.text("Shortcuts work everywhere, even while you play.", &p.fonts.small, rect(20.0, 34.0, 500.0, 54.0), MUTED);

    for (index, action) in Action::ALL.into_iter().enumerate() {
        let row = row_rect(index);
        let field = field_rect(row);
        let capturing = k.capturing == Some(action);
        let value = match (capturing, k.bindings[action.index()]) {
            (true, _) => "Press keys…".to_string(),
            (false, Some(hotkey)) => hotkey.label(),
            (false, None) => "Not set".to_string(),
        };
        let fill = match (capturing, hovered(Item::Field(action))) {
            (true, _) => ACCENT_DIM,
            (false, true) => FIELD_HOVER,
            (false, false) => FIELD,
        };
        let color = match (capturing, k.bindings[action.index()].is_some()) {
            (true, _) | (false, true) => TEXT,
            (false, false) => DIM,
        };

        p.round(row, 8.0, PANEL);
        p.text(action.label(), &p.fonts.body, rect(row.left + 16.0, row.top, field.left - 8.0, row.bottom), MUTED);
        p.round(field, 6.0, fill);
        p.text(&value, &p.fonts.button, field, color);
        p.button(clear_rect(row), "×", k.enabled(Item::Clear(action)), hovered(Item::Clear(action)));
    }
}
