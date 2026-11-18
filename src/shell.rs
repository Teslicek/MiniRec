use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{E_FAIL, ERROR_CANCELLED, HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Error, HSTRING, PCWSTR, Result, w};

pub fn error_box(window: Option<HWND>, error: &Error) {
    let owner = window.filter(|&w| unsafe { IsWindow(Some(w)) }.as_bool());
    let text = format!("{}\n\nError code 0x{:08X}", error.message(), error.code().0);

    unsafe { MessageBoxW(owner, &HSTRING::from(text), w!("MiniRec"), MB_OK | MB_ICONERROR) };
}

pub fn pick_folder(window: HWND, current: &Path) -> Result<Option<PathBuf>> {
    unsafe {
        let dialog = open_dialog(FOS_PICKFOLDERS, w!("Save recordings to"))?;

        if current.exists() {
            let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(current.as_os_str()), None)?;

            dialog.SetFolder(&item)?;
        }

        show(&dialog, window)
    }
}

pub fn pick_video(window: HWND, folder: &Path) -> Result<Option<PathBuf>> {
    unsafe {
        let dialog = open_dialog(FILEOPENDIALOGOPTIONS(0), w!("Open a video to trim"))?;
        let filters = [COMDLG_FILTERSPEC { pszName: w!("Videos (*.mp4, *.m4v, *.mov)"), pszSpec: w!("*.mp4;*.m4v;*.mov") }];

        dialog.SetFileTypes(&filters)?;

        if folder.exists() {
            let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(folder.as_os_str()), None)?;

            dialog.SetFolder(&item)?;
        }

        show(&dialog, window)
    }
}

pub fn popup_menu(window: HWND, anchor: POINT, labels: &[String], checked: &[usize]) -> Result<Option<usize>> {
    unsafe {
        let mut point = anchor;

        ClientToScreen(window, &mut point).ok()?;

        let menu = CreatePopupMenu()?;

        for (index, label) in labels.iter().enumerate() {
            if label.is_empty() {
                AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
                continue;
            }

            let state = if checked.contains(&index) { MF_CHECKED } else { MF_UNCHECKED };

            AppendMenuW(menu, MF_STRING | state, index + 1, &HSTRING::from(label))?;
        }

        let command = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_LEFTALIGN | TPM_TOPALIGN, point.x, point.y, Some(0), window, None).0 as usize;

        DestroyMenu(menu)?;
        Ok(command.checked_sub(1))
    }
}

pub fn shell_open(file: impl windows::core::Param<PCWSTR>, args: impl windows::core::Param<PCWSTR>) -> Result<()> {
    let result = unsafe { ShellExecuteW(None, w!("open"), file, args, PCWSTR::null(), SW_SHOWNORMAL) };

    if result.0 as isize <= 32 {
        return Err(Error::new(E_FAIL, format!("ShellExecute failed with code {}", result.0 as isize)));
    }

    Ok(())
}

pub fn reveal(path: &Path) -> Result<()> {
    shell_open(w!("explorer.exe"), &HSTRING::from(format!("/select,\"{}\"", path.display())))
}

pub fn low_word(value: isize) -> u32 {
    (value & 0xffff) as u32
}

pub fn high_word(value: isize) -> u32 {
    ((value >> 16) & 0xffff) as u32
}

pub fn mouse_point(lparam: isize, scale: f32) -> (f32, f32) {
    ((lparam & 0xffff) as i16 as f32 / scale, ((lparam >> 16) & 0xffff) as i16 as f32 / scale)
}

unsafe fn open_dialog(options: FILEOPENDIALOGOPTIONS, title: PCWSTR) -> Result<IFileOpenDialog> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;

        dialog.SetOptions(dialog.GetOptions()? | options | FOS_FORCEFILESYSTEM)?;
        dialog.SetTitle(title)?;
        Ok(dialog)
    }
}

unsafe fn show(dialog: &IFileOpenDialog, window: HWND) -> Result<Option<PathBuf>> {
    unsafe {
        match dialog.Show(Some(window)) {
            Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => return Ok(None),
            result => result?,
        }

        let raw = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = raw.to_string();

        CoTaskMemFree(Some(raw.0 as *const _));
        Ok(Some(PathBuf::from(path.map_err(|e| Error::new(E_FAIL, format!("Invalid path: {e}")))?)))
    }
}

pub const VIDEO_CLASS: PCWSTR = w!("MiniRecVideo");
pub const WM_MOUSELEAVE: u32 = 0x02A3;

pub fn fit_client(window: HWND, dpi: u32, width: f32, height: f32, style: WINDOW_STYLE) -> Result<()> {
    let scale = dpi as f32 / 96.0;
    let mut frame = RECT { left: 0, top: 0, right: (width * scale).round() as i32, bottom: (height * scale).round() as i32 };

    unsafe {
        AdjustWindowRectExForDpi(&mut frame, style, false, WINDOW_EX_STYLE(0), dpi)?;
        SetWindowPos(window, None, 0, 0, frame.right - frame.left, frame.bottom - frame.top, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE)
    }
}

pub fn dark_title(window: HWND) -> Result<()> {
    let dark = BOOL(1);

    unsafe { DwmSetWindowAttribute(window, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const BOOL as *const _, size_of::<BOOL>() as u32) }
}

pub fn place_child(child: HWND, area: RECT) -> Result<()> {
    unsafe { SetWindowPos(child, None, area.left, area.top, area.right - area.left, area.bottom - area.top, SWP_NOZORDER | SWP_NOACTIVATE) }
}

pub fn recycle(path: &Path) -> Result<()> {
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();

    from.extend([0, 0]);

    let mut operation = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    let result = unsafe { SHFileOperationW(&mut operation) };

    if result != 0 || operation.fAnyOperationsAborted.as_bool() {
        return Err(Error::new(E_FAIL, format!("Cannot move {} to the Recycle Bin (code {result})", path.display())));
    }

    Ok(())
}
