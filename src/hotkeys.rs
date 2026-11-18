use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

const TEST_ID: i32 = 100;

#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    Record,
    Clip,
    Replay,
    Show,
    Trim,
    Compress,
}

impl Action {
    pub const ALL: [Action; 6] = [Action::Record, Action::Clip, Action::Replay, Action::Show, Action::Trim, Action::Compress];

    pub fn label(self) -> &'static str {
        match self {
            Action::Record => "Start / stop recording",
            Action::Clip => "Save replay clip",
            Action::Replay => "Replay buffer on / off",
            Action::Show => "Show MiniRec",
            Action::Trim => "Open trimmer",
            Action::Compress => "Open compressor",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Action::Record => "hotkey_record",
            Action::Clip => "hotkey_clip",
            Action::Replay => "hotkey_replay",
            Action::Show => "hotkey_show",
            Action::Trim => "hotkey_trim",
            Action::Compress => "hotkey_compress",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Action::Record => 0,
            Action::Clip => 1,
            Action::Replay => 2,
            Action::Show => 3,
            Action::Trim => 4,
            Action::Compress => 5,
        }
    }

    fn id(self) -> i32 {
        self.index() as i32 + 1
    }

    pub fn from_id(id: i32) -> Option<Action> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Hotkey {
    pub modifiers: u32,
    pub key:       u32,
}

pub type Bindings = [Option<Hotkey>; 6];

pub const DEFAULT_BINDINGS: Bindings = [
    Some(Hotkey { modifiers: MOD_CONTROL.0 | MOD_ALT.0, key: b'R' as u32 }),
    Some(Hotkey { modifiers: MOD_CONTROL.0 | MOD_ALT.0, key: b'S' as u32 }),
    None,
    None,
    None,
    None,
];

const MODIFIER_NAMES: [(HOT_KEY_MODIFIERS, &str); 3] = [(MOD_CONTROL, "ctrl"), (MOD_ALT, "alt"), (MOD_SHIFT, "shift")];

impl Hotkey {
    pub fn from_keyboard(key: u32) -> Option<Hotkey> {
        let modifier_keys = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN, VK_LCONTROL, VK_RCONTROL, VK_LMENU, VK_RMENU, VK_LSHIFT, VK_RSHIFT];

        if modifier_keys.iter().any(|vk| vk.0 as u32 == key) {
            return None;
        }

        let held = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) } < 0;
        let modifiers = [(VK_CONTROL, MOD_CONTROL), (VK_MENU, MOD_ALT), (VK_SHIFT, MOD_SHIFT)].into_iter().filter(|&(vk, _)| held(vk)).fold(0, |all, (_, m)| all | m.0);

        Some(Hotkey { modifiers, key })
    }

    pub fn is_safe(self) -> bool {
        self.modifiers & (MOD_CONTROL.0 | MOD_ALT.0) != 0 || (VK_F1.0 as u32..=VK_F24.0 as u32).contains(&self.key)
    }

    pub fn label(self) -> String {
        let mut parts: Vec<String> = MODIFIER_NAMES.iter().filter(|(m, _)| self.modifiers & m.0 != 0).map(|(_, name)| capitalize(name)).collect();

        parts.push(key_name(self.key));
        parts.join("+")
    }

    pub fn encode(self) -> String {
        let mut parts: Vec<String> = MODIFIER_NAMES.iter().filter(|(m, _)| self.modifiers & m.0 != 0).map(|(_, name)| name.to_string()).collect();

        parts.push(self.key.to_string());
        parts.join("+")
    }

    pub fn decode(text: &str) -> Option<Hotkey> {
        let mut parts: Vec<&str> = text.split('+').collect();
        let key = parts.pop()?.parse().ok()?;
        let modifiers = parts.iter().try_fold(0, |all, part| MODIFIER_NAMES.iter().find(|(_, name)| name == part).map(|(m, _)| all | m.0))?;

        Some(Hotkey { modifiers, key })
    }

    pub fn available(self, window: HWND) -> bool {
        unsafe {
            match RegisterHotKey(Some(window), TEST_ID, HOT_KEY_MODIFIERS(self.modifiers) | MOD_NOREPEAT, self.key) {
                Ok(()) => UnregisterHotKey(Some(window), TEST_ID).is_ok(),
                Err(_) => false,
            }
        }
    }
}

pub fn register(window: HWND, bindings: &Bindings) -> (Bindings, Vec<String>) {
    let mut registered: Bindings = [None; 6];
    let mut failures = Vec::new();

    for action in Action::ALL {
        let Some(hotkey) = bindings[action.index()] else {
            continue;
        };

        match unsafe { RegisterHotKey(Some(window), action.id(), HOT_KEY_MODIFIERS(hotkey.modifiers) | MOD_NOREPEAT, hotkey.key) } {
            Ok(()) => registered[action.index()] = Some(hotkey),
            Err(_) => failures.push(format!("{} ({})", hotkey.label(), action.label())),
        }
    }

    (registered, failures)
}

pub fn unregister(window: HWND, registered: &Bindings) -> windows::core::Result<()> {
    for action in Action::ALL {
        if registered[action.index()].is_some() {
            unsafe { UnregisterHotKey(Some(window), action.id())? };
        }
    }

    Ok(())
}

pub fn label_of(bindings: &Bindings, action: Action) -> Option<String> {
    bindings[action.index()].map(Hotkey::label)
}

fn key_name(key: u32) -> String {
    unsafe {
        let scan = MapVirtualKeyW(key, MAPVK_VK_TO_VSC_EX);
        let extended = if scan & 0xff00 == 0xe000 { 1 << 24 } else { 0 };
        let mut buffer = [0u16; 64];
        let length = GetKeyNameTextW((((scan & 0xff) << 16) | extended) as i32, &mut buffer);

        match length {
            0 => format!("Key {key}"),
            n => String::from_utf16_lossy(&buffer[..n as usize]),
        }
    }
}

fn capitalize(name: &str) -> String {
    let mut chars = name.chars();

    chars.next().map_or(String::new(), |first| first.to_uppercase().chain(chars).collect())
}
