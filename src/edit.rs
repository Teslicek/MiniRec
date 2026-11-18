use std::cell::OnceCell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Error, HSTRING, Result, w};

use crate::draw::{FIELD_ACTIVE, FIELD_FONT_SIZE, TEXT};

pub const WM_EDIT_DONE: u32 = WM_APP + 20;

const SUBCLASS_ID: usize = 1;
const ES_AUTOHSCROLL: u32 = 0x0080;
const ES_RIGHT: u32 = 0x0002;
const EM_SETSEL: u32 = 0x00B1;
const EM_SETMARGINS: u32 = 0x00D3;
const EC_LEFTMARGIN: usize = 0x0001;
const EC_RIGHTMARGIN: usize = 0x0002;

thread_local! {
    static EDIT_BRUSH: OnceCell<HBRUSH> = const { OnceCell::new() };
}

pub struct InlineEdit<T> {
    window:     HWND,
    font:       HFONT,
    pub target: T,
}

impl<T> InlineEdit<T> {
    pub fn open(parent: HWND, area: RECT, text: &str, scale: f32, target: T) -> Result<Self> {
        unsafe {
            let font = CreateFontW(-((FIELD_FONT_SIZE * scale).round() as i32), 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));

            if font.is_invalid() {
                return Err(Error::from_win32());
            }

            let window = match create_edit(parent, area, text, font) {
                Ok(window) => window,
                Err(e) => {
                    DeleteObject(font.into()).ok()?;
                    return Err(e);
                }
            };

            if !SetWindowSubclass(window, Some(subclass_proc), SUBCLASS_ID, parent.0 as usize).as_bool() {
                DestroyWindow(window)?;
                DeleteObject(font.into()).ok()?;
                return Err(Error::from_win32());
            }

            SendMessageW(window, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            _ = SetFocus(Some(window));
            Ok(Self { window, font, target })
        }
    }

    pub fn text(&self) -> Result<String> {
        unsafe {
            let length = GetWindowTextLengthW(self.window);
            let mut buffer = vec![0u16; length as usize + 1];
            let copied = GetWindowTextW(self.window, &mut buffer);

            Ok(String::from_utf16_lossy(&buffer[..copied as usize]))
        }
    }

    pub fn close(self) -> Result<()> {
        unsafe {
            RemoveWindowSubclass(self.window, Some(subclass_proc), SUBCLASS_ID).ok()?;
            DestroyWindow(self.window)?;
            DeleteObject(self.font.into()).ok()
        }
    }
}

unsafe fn create_edit(parent: HWND, area: RECT, text: &str, font: HFONT) -> Result<HWND> {
    unsafe {
        let height = line_height(font)?;
        let top = (area.top + area.bottom - height + 1) / 2;
        let style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | ES_AUTOHSCROLL | ES_RIGHT);
        let window = CreateWindowExW(WS_EX_NOPARENTNOTIFY, w!("EDIT"), &HSTRING::from(text), style, area.left, top, area.right - area.left, height, Some(parent), None, None, None)?;

        SendMessageW(window, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        SendMessageW(window, EM_SETMARGINS, Some(WPARAM(EC_LEFTMARGIN | EC_RIGHTMARGIN)), Some(LPARAM(0)));
        Ok(window)
    }
}

unsafe fn line_height(font: HFONT) -> Result<i32> {
    unsafe {
        let hdc = GetDC(None);
        let previous = SelectObject(hdc, font.into());
        let mut metrics = TEXTMETRICW::default();
        let measured = GetTextMetricsW(hdc, &mut metrics);

        SelectObject(hdc, previous);
        ReleaseDC(None, hdc);
        measured.ok()?;
        Ok(metrics.tmHeight)
    }
}

pub fn edit_colors(hdc: HDC) -> LRESULT {
    unsafe {
        SetTextColor(hdc, colorref(TEXT));
        SetBkColor(hdc, colorref(FIELD_ACTIVE));
    }

    let brush = EDIT_BRUSH.with(|cell| *cell.get_or_init(|| unsafe { CreateSolidBrush(colorref(FIELD_ACTIVE)) }));

    LRESULT(brush.0 as isize)
}

fn colorref(hex: u32) -> COLORREF {
    COLORREF(((hex & 0xff) << 16) | (hex & 0xff00) | ((hex >> 16) & 0xff))
}

unsafe extern "system" fn subclass_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, parent: usize) -> LRESULT {
    unsafe {
        let parent = HWND(parent as *mut _);
        let key = wparam.0 as u16;

        match message {
            WM_KEYDOWN if key == VK_RETURN.0 || key == VK_ESCAPE.0 => {
                _ = PostMessageW(Some(parent), WM_EDIT_DONE, WPARAM((key == VK_RETURN.0) as usize), LPARAM(0));
                LRESULT(0)
            }
            WM_CHAR if wparam.0 == 0x0d || wparam.0 == 0x1b => LRESULT(0),
            WM_KILLFOCUS => {
                _ = PostMessageW(Some(parent), WM_EDIT_DONE, WPARAM(1), LPARAM(0));
                DefSubclassProc(window, message, wparam, lparam)
            }
            _ => DefSubclassProc(window, message, wparam, lparam),
        }
    }
}
