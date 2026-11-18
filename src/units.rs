use windows::Win32::Foundation::E_INVALIDARG;
use windows::core::{Error, Result};

pub const MIN_BITRATE: u64 = 100_000;
pub const MAX_BITRATE: u64 = 500_000_000;
pub const MIN_FPS: u32 = 1;
pub const MAX_FPS: u32 = 360;
pub const MIN_REPLAY_SECONDS: u32 = 10;
pub const MAX_REPLAY_SECONDS: u32 = 20 * 60;
pub const MIN_QUALITY: u32 = 1;
pub const MAX_QUALITY: u32 = 100;

#[derive(Clone, Copy, PartialEq)]
pub enum BitrateUnit {
    Kilobits,
    Kilobytes,
    Megabits,
    Megabytes,
}

impl BitrateUnit {
    pub const ALL: [BitrateUnit; 4] = [BitrateUnit::Kilobits, BitrateUnit::Kilobytes, BitrateUnit::Megabits, BitrateUnit::Megabytes];

    pub fn label(self) -> &'static str {
        match self {
            BitrateUnit::Kilobits => "kb/s",
            BitrateUnit::Kilobytes => "kB/s",
            BitrateUnit::Megabits => "Mb/s",
            BitrateUnit::Megabytes => "MB/s",
        }
    }

    pub fn parse(label: &str) -> Option<BitrateUnit> {
        Self::ALL.into_iter().find(|unit| unit.label() == label)
    }

    fn bits(self) -> f64 {
        match self {
            BitrateUnit::Kilobits => 1e3,
            BitrateUnit::Kilobytes => 8e3,
            BitrateUnit::Megabits => 1e6,
            BitrateUnit::Megabytes => 8e6,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum TimeUnit {
    Seconds,
    Minutes,
}

impl TimeUnit {
    pub const ALL: [TimeUnit; 2] = [TimeUnit::Seconds, TimeUnit::Minutes];

    pub fn label(self) -> &'static str {
        match self {
            TimeUnit::Seconds => "sec",
            TimeUnit::Minutes => "min",
        }
    }

    pub fn parse(label: &str) -> Option<TimeUnit> {
        Self::ALL.into_iter().find(|unit| unit.label() == label)
    }

    fn seconds(self) -> f64 {
        match self {
            TimeUnit::Seconds => 1.0,
            TimeUnit::Minutes => 60.0,
        }
    }
}

pub fn format_length(seconds: u32, unit: TimeUnit) -> String {
    trim_decimal(seconds as f64 / unit.seconds())
}

pub fn format_amount(bits: u64, unit: BitrateUnit) -> String {
    trim_decimal(bits as f64 / unit.bits())
}

pub fn format_in(bits: u64, unit: BitrateUnit) -> String {
    format!("{} {}", format_amount(bits, unit), unit.label())
}

pub fn parse_bitrate_input(text: &str, unit: BitrateUnit) -> Result<u64> {
    match text.trim().replace(',', ".").parse::<f64>() {
        Ok(value) => check_bitrate((value * unit.bits()).round() as u64),
        Err(_) => parse_bitrate(text),
    }
}

pub fn parse_bitrate(text: &str) -> Result<u64> {
    let invalid = || Error::new(E_INVALIDARG, format!("\"{text}\" is not a bitrate.\n\nType a number with a unit, for example 25Mb, 3MB or 800kb.\nb = bits per second, B = bytes per second, k = thousand, M = million."));
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let compact = compact.strip_suffix("/s").or_else(|| compact.strip_suffix("ps")).unwrap_or(&compact).replace(',', ".");
    let split = compact.find(|c: char| !c.is_ascii_digit() && c != '.').ok_or_else(invalid)?;
    let (number, unit) = compact.split_at(split);
    let value: f64 = number.parse().map_err(|_| invalid())?;
    let mut chars = unit.chars();
    let (prefix, kind) = match (chars.next(), chars.next(), chars.next()) {
        (Some(kind), None, None) => (None, kind),
        (Some(prefix), Some(kind), None) => (Some(prefix), kind),
        _ => return Err(invalid()),
    };
    let multiplier = match prefix {
        None => 1.0,
        Some('k' | 'K') => 1e3,
        Some('m' | 'M') => 1e6,
        Some('g' | 'G') => 1e9,
        Some(_) => return Err(invalid()),
    };
    let bits_per_unit = match kind {
        'b' => 1.0,
        'B' => 8.0,
        _ => return Err(invalid()),
    };
    check_bitrate((value * multiplier * bits_per_unit).round() as u64)
}

fn check_bitrate(bits: u64) -> Result<u64> {
    if !(MIN_BITRATE..=MAX_BITRATE).contains(&bits) {
        return Err(Error::new(E_INVALIDARG, format!("{} is out of range. Use {} to {}.", format_bitrate(bits), format_bitrate(MIN_BITRATE), format_bitrate(MAX_BITRATE))));
    }

    Ok(bits)
}

pub fn format_bitrate(bits: u64) -> String {
    match bits {
        b if b >= 1_000_000 => format!("{} Mb/s", trim_decimal(b as f64 / 1e6)),
        b => format!("{} kb/s", trim_decimal(b as f64 / 1e3)),
    }
}

pub fn parse_fps(text: &str) -> Result<u32> {
    let fps: u32 = text.trim().to_ascii_lowercase().trim_end_matches("fps").trim().parse().map_err(|_| Error::new(E_INVALIDARG, format!("\"{text}\" is not a frame rate. Type a whole number like 60.")))?;

    if !(MIN_FPS..=MAX_FPS).contains(&fps) {
        return Err(Error::new(E_INVALIDARG, format!("{fps} FPS is out of range. Use {MIN_FPS} to {MAX_FPS}.")));
    }

    Ok(fps)
}

pub fn parse_quality(text: &str) -> Result<u32> {
    let quality: u32 = text.trim().parse().map_err(|_| Error::new(E_INVALIDARG, format!("\"{text}\" is not a quality level. Type a whole number like 85.")))?;

    if !(MIN_QUALITY..=MAX_QUALITY).contains(&quality) {
        return Err(Error::new(E_INVALIDARG, format!("Quality {quality} is out of range. Use {MIN_QUALITY} to {MAX_QUALITY}.")));
    }

    Ok(quality)
}

pub fn format_duration(seconds: u32) -> String {
    match (seconds / 60, seconds % 60) {
        (0, rest) => format!("{rest} s"),
        (minutes, 0) => format!("{minutes} min"),
        (minutes, rest) => format!("{minutes} min {rest} s"),
    }
}

pub fn parse_replay_length(text: &str, unit: TimeUnit) -> Result<u32> {
    let invalid = || Error::new(E_INVALIDARG, format!("\"{text}\" is not a length. Type seconds like 30, or minutes like 2m or 1:30."));
    let compact = text.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase().replace(',', ".");
    let seconds = match compact.split_once(':') {
        Some((minutes, rest)) => minutes.parse::<u32>().ok().zip(rest.parse::<u32>().ok().filter(|&rest| rest < 60)).map(|(minutes, rest)| minutes * 60 + rest),
        None => {
            let (number, unit) = match (compact.strip_suffix("min").or_else(|| compact.strip_suffix('m')), compact.strip_suffix("sec").or_else(|| compact.strip_suffix('s'))) {
                (Some(minutes), _) => (minutes, TimeUnit::Minutes),
                (None, Some(seconds)) => (seconds, TimeUnit::Seconds),
                (None, None) => (compact.as_str(), unit),
            };

            number.parse::<f64>().ok().filter(|value| value.is_finite() && *value >= 0.0).map(|value| (value * unit.seconds()).round() as u32)
        }
    };

    check_replay_seconds(seconds.ok_or_else(invalid)?)
}

pub fn check_replay_seconds(seconds: u32) -> Result<u32> {
    if !(MIN_REPLAY_SECONDS..=MAX_REPLAY_SECONDS).contains(&seconds) {
        return Err(Error::new(E_INVALIDARG, format!("Use {} to {}.", format_duration(MIN_REPLAY_SECONDS), format_duration(MAX_REPLAY_SECONDS))));
    }

    Ok(seconds)
}

pub const MIN_SIZE: u64 = 1 << 20;
pub const MAX_SIZE: u64 = 100 << 30;

pub fn parse_size(text: &str) -> Result<u64> {
    let invalid = || Error::new(E_INVALIDARG, format!("\"{text}\" is not a file size. Type megabytes like 25, or add a unit: 500MB, 1.5GB."));
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_uppercase().replace(',', ".");
    let (number, multiplier) = match compact.strip_suffix("GB").or_else(|| compact.strip_suffix('G')) {
        Some(number) => (number.to_string(), (1u64 << 30) as f64),
        None => match compact.strip_suffix("MB").or_else(|| compact.strip_suffix('M')) {
            Some(number) => (number.to_string(), (1u64 << 20) as f64),
            None => (compact.clone(), (1u64 << 20) as f64),
        },
    };
    let value: f64 = number.parse().map_err(|_| invalid())?;
    let bytes = (value * multiplier).round() as u64;

    if !(MIN_SIZE..=MAX_SIZE).contains(&bytes) {
        return Err(Error::new(E_INVALIDARG, format!("Use {} to {}.", format_bytes(MIN_SIZE), format_bytes(MAX_SIZE))));
    }

    Ok(bytes)
}

pub fn format_megabytes(bytes: u64) -> String {
    trim_decimal(bytes as f64 / (1u64 << 20) as f64)
}

pub fn format_bytes(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{} GB", trim_decimal(b as f64 / (1u64 << 30) as f64)),
        b => format!("{} MB", trim_decimal(b as f64 / (1u64 << 20) as f64)),
    }
}

pub fn format_time(seconds: f64) -> String {
    let centis = (seconds.max(0.0) * 100.0).round() as u64;
    let (hours, minutes, secs, cs) = (centis / 360_000, centis / 6000 % 60, centis / 100 % 60, centis % 100);

    match hours {
        0 => format!("{minutes}:{secs:02}.{cs:02}"),
        h => format!("{h}:{minutes:02}:{secs:02}.{cs:02}"),
    }
}

pub fn parse_time(text: &str) -> Result<f64> {
    let invalid = || Error::new(E_INVALIDARG, format!("\"{text}\" is not a time. Use seconds (83.5), m:ss (1:23.5) or h:mm:ss (0:01:23.5)."));
    let parts: Vec<&str> = text.trim().split(':').collect();

    if parts.is_empty() || parts.len() > 3 {
        return Err(invalid());
    }

    parts.iter().try_fold(0.0, |total, part| {
        let value: f64 = part.trim().replace(',', ".").parse().map_err(|_| invalid())?;

        if value < 0.0 {
            return Err(invalid());
        }

        Ok(total * 60.0 + value)
    })
}

fn trim_decimal(value: f64) -> String {
    let text = format!("{value:.2}");

    text.trim_end_matches('0').trim_end_matches('.').to_string()
}
