use windows::Win32::Foundation::{CloseHandle, E_FAIL, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Threading::{GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Error, PWSTR, Result};

#[derive(Clone)]
pub struct Program {
    pub window:  isize,
    pub title:   String,
    pub process: String,
}

impl Program {
    pub fn hwnd(&self) -> HWND {
        HWND(self.window as *mut _)
    }

    pub fn label(&self) -> String {
        format!("{}  ({})", self.title, self.process)
    }
}

pub fn list() -> Result<Vec<Program>> {
    let mut windows: Vec<HWND> = Vec::new();

    unsafe { EnumWindows(Some(collect), LPARAM(&mut windows as *mut Vec<HWND> as isize))? };

    let own = unsafe { GetCurrentProcessId() };
    let mut programs = Vec::new();

    for window in windows {
        if let Some(program) = describe(window, own)? {
            programs.push(program);
        }
    }

    Ok(programs)
}

pub fn find(process: &str) -> Result<Program> {
    running(process)?.ok_or_else(|| Error::new(E_FAIL, format!("{process} is not running, so there is no window to capture. Start it or pick something else under Capture.")))
}

pub fn running(process: &str) -> Result<Option<Program>> {
    Ok(list()?.into_iter().find(|program| program.process.eq_ignore_ascii_case(process)))
}

pub fn minimized(program: &Program) -> bool {
    unsafe { IsIconic(program.hwnd()).as_bool() }
}

unsafe extern "system" fn collect(window: HWND, list: LPARAM) -> BOOL {
    unsafe { (*(list.0 as *mut Vec<HWND>)).push(window) };
    BOOL(1)
}

fn describe(window: HWND, own: u32) -> Result<Option<Program>> {
    unsafe {
        if !IsWindowVisible(window).as_bool() || GetWindow(window, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
            return Ok(None);
        }

        if GetWindowLongW(window, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return Ok(None);
        }

        let mut cloaked = 0u32;

        DwmGetWindowAttribute(window, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, size_of::<u32>() as u32)?;

        if cloaked != 0 {
            return Ok(None);
        }

        let mut bounds = RECT::default();

        GetWindowRect(window, &mut bounds)?;

        if bounds.right - bounds.left < 2 || bounds.bottom - bounds.top < 2 {
            return Ok(None);
        }

        let length = GetWindowTextLengthW(window);

        if length == 0 {
            return Ok(None);
        }

        let mut title = vec![0u16; length as usize + 1];
        let copied = GetWindowTextW(window, &mut title);
        let mut process_id = 0u32;

        GetWindowThreadProcessId(window, Some(&mut process_id));

        if process_id == own {
            return Ok(None);
        }

        let Some(process) = process_name(process_id) else {
            return Ok(None);
        };

        Ok(Some(Program { window: window.0 as isize, title: String::from_utf16_lossy(&title[..copied as usize]), process }))
    }
}

fn process_name(process_id: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()?;
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let queried = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut length);

        _ = CloseHandle(handle);
        queried.ok()?;

        let path = String::from_utf16_lossy(&buffer[..length as usize]);

        path.rsplit('\\').next().map(str::to_string)
    }
}
