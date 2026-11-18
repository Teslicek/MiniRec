use std::fs;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::core::{Error, Result};

use crate::hotkeys::{Action, Bindings, DEFAULT_BINDINGS, Hotkey};
use crate::units::{BitrateUnit, MAX_SIZE, MIN_SIZE, TimeUnit, check_replay_seconds, format_in, parse_bitrate, parse_fps, parse_quality};
use crate::video::MonitorInfo;

const DEFAULT_FPS: u32 = 60;
const FPS_PRESETS: [u32; 8] = [24, 30, 60, 120, 144, 165, 240, 360];
const BITRATE_PRESETS: [(&str, u64); 3] = [("Low", 12), ("Medium", 6), ("High", 3)];
const DEFAULT_DEVICE: &str = "default";
const UNBOUND: &str = "none";
const SOURCE_OUTPUT: &str = "source";
pub const OUTPUT_HEIGHTS: [u32; 5] = [2160, 1440, 1080, 720, 480];
const DEFAULT_REPLAY_SECONDS: u32 = 120;
pub const QUALITY_PRESETS: [(&str, u32); 3] = [("Small", 40), ("Balanced", 50), ("High", 60)];
pub const SIZE_PRESETS: [u64; 5] = [10 << 20, 25 << 20, 50 << 20, 100 << 20, 500 << 20];

#[derive(Clone, Copy)]
pub struct CompressSettings {
    pub fit_size: bool,
    pub quality:  u32,
    pub size:     u64,
    pub replace:  bool,
}

pub struct Settings {
    pub monitor:           String,
    pub capture_program:   Option<String>,
    pub output:            Option<u32>,
    pub fps:               u32,
    pub bitrate:           u64,
    pub bitrate_unit:      BitrateUnit,
    pub system_audio:      bool,
    pub system_device:     Option<String>,
    pub microphone:        bool,
    pub microphone_device: Option<String>,
    pub cursor:            bool,
    pub replay:            bool,
    pub replay_seconds:    u32,
    pub replay_unit:       TimeUnit,
    pub compress:          CompressSettings,
    pub hotkeys:           Bindings,
    pub folder:            PathBuf,
}

impl Settings {
    pub fn load(default_monitor: &MonitorInfo) -> Result<Self> {
        let path = settings_path()?;
        let mut settings = Self {
            monitor: default_monitor.name.clone(),
            capture_program: None,
            output: None,
            fps: DEFAULT_FPS,
            bitrate: bitrate_presets((default_monitor.width, default_monitor.height), DEFAULT_FPS, BitrateUnit::Megabits)[1].1,
            bitrate_unit: BitrateUnit::Megabits,
            system_audio: true,
            system_device: None,
            microphone: true,
            microphone_device: None,
            cursor: true,
            replay: false,
            replay_seconds: DEFAULT_REPLAY_SECONDS,
            replay_unit: TimeUnit::Seconds,
            compress: CompressSettings { fit_size: false, quality: QUALITY_PRESETS[1].1, size: SIZE_PRESETS[1], replace: false },
            hotkeys: DEFAULT_BINDINGS,
            folder: default_folder()?,
        };

        if !path.exists() {
            return Ok(settings);
        }

        let text = fs::read_to_string(&path).map_err(|e| file_error("read", &path, e))?;

        for (index, line) in text.lines().enumerate() {
            let line = line.trim();

            if line.is_empty() {
                continue;
            }

            let invalid = || Error::new(E_FAIL, format!("Invalid line {} in {}:\n{line}", index + 1, path.display()));
            let (key, value) = line.split_once('=').ok_or_else(invalid)?;
            let (key, value) = (key.trim(), value.trim());

            if let Some(action) = Action::ALL.into_iter().find(|action| action.key() == key) {
                settings.hotkeys[action.index()] = match value {
                    UNBOUND => None,
                    text => Some(Hotkey::decode(text).ok_or_else(invalid)?),
                };
                continue;
            }

            match key {
                "monitor" => settings.monitor = value.to_string(),
                "capture_program" => settings.capture_program = (value != UNBOUND).then(|| value.to_string()),
                "output" => settings.output = match value {
                    SOURCE_OUTPUT => None,
                    height => Some(height.parse().ok().filter(|height| OUTPUT_HEIGHTS.contains(height)).ok_or_else(invalid)?),
                },
                "fps" => settings.fps = parse_fps(value).map_err(|_| invalid())?,
                "bitrate" => settings.bitrate = parse_bitrate(value).map_err(|_| invalid())?,
                "bitrate_unit" => settings.bitrate_unit = BitrateUnit::parse(value).ok_or_else(invalid)?,
                "system_audio" => settings.system_audio = parse_bool(value).ok_or_else(invalid)?,
                "system_device" => settings.system_device = parse_device(value),
                "microphone" => settings.microphone = parse_bool(value).ok_or_else(invalid)?,
                "microphone_device" => settings.microphone_device = parse_device(value),
                "cursor" => settings.cursor = parse_bool(value).ok_or_else(invalid)?,
                "replay" => settings.replay = parse_bool(value).ok_or_else(invalid)?,
                "replay_seconds" => settings.replay_seconds = value.parse().ok().and_then(|seconds| check_replay_seconds(seconds).ok()).ok_or_else(invalid)?,
                "replay_unit" => settings.replay_unit = TimeUnit::parse(value).ok_or_else(invalid)?,
                "compress_fit_size" => settings.compress.fit_size = parse_bool(value).ok_or_else(invalid)?,
                "compress_quality" => settings.compress.quality = parse_quality(value).map_err(|_| invalid())?,
                "compress_size" => settings.compress.size = value.parse().ok().filter(|bytes| (MIN_SIZE..=MAX_SIZE).contains(bytes)).ok_or_else(invalid)?,
                "compress_replace" => settings.compress.replace = parse_bool(value).ok_or_else(invalid)?,
                "folder" if !value.is_empty() => settings.folder = PathBuf::from(value),
                _ => return Err(invalid()),
            }
        }

        Ok(settings)
    }

    pub fn save(&self) -> Result<()> {
        let path = settings_path()?;
        let text = format!(
            "monitor={}\ncapture_program={}\noutput={}\nfps={}\nbitrate={}b\nbitrate_unit={}\nsystem_audio={}\nsystem_device={}\nmicrophone={}\nmicrophone_device={}\ncursor={}\nreplay={}\nreplay_seconds={}\nreplay_unit={}\ncompress_fit_size={}\ncompress_quality={}\ncompress_size={}\ncompress_replace={}\nfolder={}\n",
            self.monitor,
            self.capture_program.as_deref().map_or(UNBOUND, |process| process),
            self.output.map_or(SOURCE_OUTPUT.to_string(), |height| height.to_string()),
            self.fps,
            self.bitrate,
            self.bitrate_unit.label(),
            self.system_audio,
            self.system_device.as_deref().map_or(DEFAULT_DEVICE, |id| id),
            self.microphone,
            self.microphone_device.as_deref().map_or(DEFAULT_DEVICE, |id| id),
            self.cursor,
            self.replay,
            self.replay_seconds,
            self.replay_unit.label(),
            self.compress.fit_size,
            self.compress.quality,
            self.compress.size,
            self.compress.replace,
            self.folder.display(),
        );

        let hotkeys: String = Action::ALL.iter().map(|&action| format!("{}={}\n", action.key(), self.hotkeys[action.index()].map_or(UNBOUND.to_string(), Hotkey::encode))).collect();

        fs::write(&path, text + &hotkeys).map_err(|e| file_error("write", &path, e))
    }

    pub fn system_source(&self) -> Option<Option<String>> {
        self.system_audio.then(|| self.system_device.clone())
    }

    pub fn microphone_source(&self) -> Option<Option<String>> {
        self.microphone.then(|| self.microphone_device.clone())
    }
}

pub fn fps_presets(monitor: &MonitorInfo) -> Vec<u32> {
    let mut presets: Vec<u32> = FPS_PRESETS.into_iter().filter(|&fps| fps <= monitor.refresh).collect();

    if !presets.contains(&monitor.refresh) {
        presets.push(monitor.refresh);
        presets.sort_unstable();
    }

    presets
}

pub fn bitrate_presets((width, height): (u32, u32), fps: u32, unit: BitrateUnit) -> Vec<(String, u64)> {
    BITRATE_PRESETS
        .iter()
        .map(|&(name, divisor)| {
            let bits = width as u64 * height as u64 * fps as u64 / divisor;

            (format!("{name}  ·  {}", format_in(bits, unit)), bits)
        })
        .collect()
}

pub fn create_folder(folder: &Path) -> Result<()> {
    fs::create_dir_all(folder).map_err(|e| file_error("create", folder, e))
}

fn settings_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().map_err(|e| Error::new(E_FAIL, format!("Cannot locate MiniRec.exe: {e}")))?;
    let folder = exe.parent().ok_or_else(|| Error::new(E_FAIL, format!("{} has no parent folder", exe.display())))?;

    Ok(folder.join("MiniRec.ini"))
}

fn default_folder() -> Result<PathBuf> {
    unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None)?;
        let videos = raw.to_string();

        CoTaskMemFree(Some(raw.0 as *const _));
        Ok(PathBuf::from(videos.map_err(|e| Error::new(E_FAIL, format!("Invalid Videos folder path: {e}")))?).join("MiniRec"))
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_device(value: &str) -> Option<String> {
    (value != DEFAULT_DEVICE).then(|| value.to_string())
}

fn file_error(action: &str, path: &Path, error: std::io::Error) -> Error {
    Error::new(E_FAIL, format!("Cannot {action} {}: {error}", path.display()))
}
