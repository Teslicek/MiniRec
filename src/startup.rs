use windows::Win32::Foundation::{E_FAIL, ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};
use windows::core::{Error, PCWSTR, Result, w};

pub const BACKGROUND_ARG: &str = "--background";
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("MiniRec");

pub fn started_in_background() -> bool {
    std::env::args().skip(1).any(|arg| arg == BACKGROUND_ARG)
}

pub fn enabled() -> Result<bool> {
    let mut size = 0u32;

    match unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME, RRF_RT_REG_SZ, None, None, Some(&mut size)) } {
        ERROR_FILE_NOT_FOUND => return Ok(false),
        status => check(status, "read")?,
    }

    let mut data = vec![0u16; size as usize / 2];

    check(unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME, RRF_RT_REG_SZ, None, Some(data.as_mut_ptr().cast()), Some(&mut size)) }, "read")?;

    let stored = String::from_utf16_lossy(&data[..(size as usize / 2).saturating_sub(1)]);

    Ok(stored.eq_ignore_ascii_case(&command()?))
}

pub fn set(enabled: bool) -> Result<()> {
    if !enabled {
        return check(unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME) }, "remove");
    }

    let wide: Vec<u16> = command()?.encode_utf16().chain(std::iter::once(0)).collect();

    check(unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME, REG_SZ.0, Some(wide.as_ptr().cast()), (wide.len() * 2) as u32) }, "write")
}

fn command() -> Result<String> {
    let exe = std::env::current_exe().map_err(|e| Error::new(E_FAIL, format!("Cannot find the MiniRec exe path: {e}")))?;

    Ok(format!("\"{}\" {BACKGROUND_ARG}", exe.display()))
}

fn check(status: WIN32_ERROR, action: &str) -> Result<()> {
    status.ok().map_err(|e| Error::new(e.code(), format!("Cannot {action} the Start with Windows entry: {}", e.message())))
}
