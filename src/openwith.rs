use std::path::{Path, PathBuf};

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows::core::{Error, HSTRING, PCWSTR, Result};

const PROG_ID: &str = "MiniRec.Trim";
const APP_NAME: &str = "MiniRec Trimmer";
const EXTENSIONS: [&str; 3] = [".mp4", ".m4v", ".mov"];

pub fn video_arg() -> Option<PathBuf> {
    std::env::args_os().skip(1).map(PathBuf::from).find(|path| is_video(path))
}

pub fn is_video(path: &Path) -> bool {
    path.extension().is_some_and(|ext| EXTENSIONS.iter().any(|known| ext.eq_ignore_ascii_case(&known[1..])))
}

pub fn register() -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| Error::new(E_FAIL, format!("Cannot find the MiniRec exe path: {e}")))?;
    let exe = exe.display();
    let classes = format!("Software\\Classes\\{PROG_ID}");

    set(&classes, None, "Video")?;
    set(&format!("{classes}\\DefaultIcon"), None, &format!("\"{exe}\",0"))?;
    set(&format!("{classes}\\Application"), Some("ApplicationName"), APP_NAME)?;
    set(&format!("{classes}\\shell\\open"), Some("FriendlyAppName"), APP_NAME)?;
    set(&format!("{classes}\\shell\\open\\command"), None, &format!("\"{exe}\" \"%1\""))?;

    for extension in EXTENSIONS {
        set(&format!("Software\\Classes\\{extension}\\OpenWithProgids"), Some(PROG_ID), "")?;
    }

    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    Ok(())
}

fn set(key: &str, name: Option<&str>, value: &str) -> Result<()> {
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let name = name.map(HSTRING::from);
    let name_ptr = name.as_ref().map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()));

    unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), name_ptr, REG_SZ.0, Some(wide.as_ptr().cast()), (wide.len() * 2) as u32) }
        .ok()
        .map_err(|e| Error::new(e.code(), format!("Cannot write the Open with entry {key}: {}", e.message())))
}
