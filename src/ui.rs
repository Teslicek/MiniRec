use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

use crate::draw::*;
use crate::settings::Settings;
use crate::units::{format_amount, format_in, format_length};

pub const WIDTH: f32 = 720.0;
pub const HEIGHT: f32 = 762.0;

const PREVIEW: D2D_RECT_F = rect(20.0, 20.0, 700.0, 402.0);
const FOLDER_ROW: D2D_RECT_F = rect(20.0, 698.0, 700.0, 742.0);
const REPLAY_ROW: D2D_RECT_F = setting_row(1, 3);
const DEVICE_LABEL_WIDTH: f32 = 112.0;
const TOGGLE_WIDTH: f32 = 72.0;

#[derive(Clone, Copy, PartialEq)]
pub enum Element {
    Record,
    Trim,
    Keybinds,
    Latest,
    Monitor,
    Output,
    Fps,
    FpsPresets,
    Bitrate,
    BitrateUnit,
    SystemToggle,
    SystemDevice,
    MicToggle,
    MicDevice,
    Cursor,
    ReplayToggle,
    ReplayLength,
    ReplayUnit,
    Compress,
    FolderChange,
    FolderOpen,
}

impl Element {
    const HIT_ORDER: [Element; 21] = [
        Element::Record,
        Element::Trim,
        Element::Compress,
        Element::Keybinds,
        Element::Latest,
        Element::Monitor,
        Element::Output,
        Element::FpsPresets,
        Element::Fps,
        Element::BitrateUnit,
        Element::Bitrate,
        Element::SystemToggle,
        Element::SystemDevice,
        Element::MicToggle,
        Element::MicDevice,
        Element::Cursor,
        Element::ReplayToggle,
        Element::ReplayLength,
        Element::ReplayUnit,
        Element::FolderChange,
        Element::FolderOpen,
    ];

    pub fn at(x: f32, y: f32) -> Option<Element> {
        Self::HIT_ORDER.into_iter().find(|element| contains(element.rect(), x, y))
    }

    pub fn rect(self) -> D2D_RECT_F {
        match self {
            Element::Record => rect(20.0, 418.0, 76.0, 474.0),
            Element::Trim => rect(606.0, 428.0, 700.0, 464.0),
            Element::Compress => rect(486.0, 428.0, 600.0, 464.0),
            Element::Keybinds => rect(390.0, 428.0, 480.0, 464.0),
            Element::Latest => rect(300.0, 705.0, 548.0, 735.0),
            Element::Monitor => dropdown_rect(setting_row(0, 0)),
            Element::Output => dropdown_rect(setting_row(0, 1)),
            Element::Fps => field_rect(setting_row(0, 2)),
            Element::FpsPresets => pill_rect(setting_row(0, 2)),
            Element::Bitrate => field_rect(setting_row(0, 3)),
            Element::BitrateUnit => pill_rect(setting_row(0, 3)),
            Element::SystemDevice => device_rect(setting_row(1, 0)),
            Element::SystemToggle => right_zone(setting_row(1, 0), TOGGLE_WIDTH),
            Element::MicDevice => device_rect(setting_row(1, 1)),
            Element::MicToggle => right_zone(setting_row(1, 1), TOGGLE_WIDTH),
            Element::Cursor => right_zone(setting_row(1, 2), TOGGLE_WIDTH),
            Element::ReplayLength => rect(REPLAY_ROW.left + 112.0, REPLAY_ROW.top + 8.0, REPLAY_ROW.left + 176.0, REPLAY_ROW.bottom - 8.0),
            Element::ReplayUnit => rect(REPLAY_ROW.left + 182.0, REPLAY_ROW.top + 8.0, REPLAY_ROW.left + 252.0, REPLAY_ROW.bottom - 8.0),
            Element::ReplayToggle => right_zone(REPLAY_ROW, TOGGLE_WIDTH),
            Element::FolderChange => rect(554.0, 705.0, 622.0, 735.0),
            Element::FolderOpen => rect(628.0, 705.0, 693.0, 735.0),
        }
    }

    pub fn enabled(self, recording: bool) -> bool {
        !recording
            || matches!(
                self,
                Element::Record | Element::Trim | Element::Compress | Element::Keybinds | Element::Latest | Element::FolderOpen | Element::ReplayToggle | Element::ReplayLength | Element::ReplayUnit
            )
    }

    pub fn edit_area(self) -> D2D_RECT_F {
        field_text_area(self.rect())
    }
}

pub struct View<'a> {
    pub settings:       &'a Settings,
    pub resolution:     (u32, u32),
    pub output:         String,
    pub system_device:  (String, bool),
    pub mic_device:     (String, bool),
    pub elapsed:        Option<u64>,
    pub record_hotkey:  Option<String>,
    pub capture:        String,
    pub program_note:   Option<String>,
    pub latest:         Option<String>,
    pub activity:       Option<String>,
    pub hover:          Option<Element>,
    pub editing:        Option<Element>,
    pub preview_failed: bool,
}

impl View<'_> {
    fn recording(&self) -> bool {
        self.elapsed.is_some()
    }

    fn hovered(&self, element: Element) -> bool {
        self.hover == Some(element) && element.enabled(self.recording())
    }
}

pub fn draw(p: &Painter, view: &View) {
    let settings = view.settings;

    p.clear(BACKGROUND);
    p.round(PREVIEW, 12.0, BLACK);
    draw_record_bar(p, view);
    selector(p, view, Element::Monitor, setting_row(0, 0), "Capture", &view.capture);

    selector(p, view, Element::Output, setting_row(0, 1), "Output", &view.output);

    if let Some(note) = &view.program_note {
        p.text(note, &p.fonts.button, rect(PREVIEW.left, (PREVIEW.top + PREVIEW.bottom) / 2.0 - 30.0, PREVIEW.right, (PREVIEW.top + PREVIEW.bottom) / 2.0), TEXT);
        p.text("The preview starts when its window is open again.", &p.fonts.small_center, rect(PREVIEW.left, (PREVIEW.top + PREVIEW.bottom) / 2.0, PREVIEW.right, (PREVIEW.top + PREVIEW.bottom) / 2.0 + 24.0), MUTED);
    }
    number_row(p, view, (Element::Fps, Element::FpsPresets), setting_row(0, 2), "Frame rate", &settings.fps.to_string(), "FPS");
    number_row(p, view, (Element::Bitrate, Element::BitrateUnit), setting_row(0, 3), "Bitrate", &format_amount(settings.bitrate, settings.bitrate_unit), settings.bitrate_unit.label());
    device(p, view, Element::SystemDevice, setting_row(1, 0), "System audio", &view.system_device, settings.system_audio);
    device(p, view, Element::MicDevice, setting_row(1, 1), "Microphone", &view.mic_device, settings.microphone);
    toggle(p, view, Element::Cursor, setting_row(1, 2), "Show cursor", settings.cursor);
    switch_row(p, view, "Replay buffer", &format_length(settings.replay_seconds, settings.replay_unit), settings.replay_unit.label(), settings.replay);

    let enabled = !view.recording();

    p.round(FOLDER_ROW, 8.0, PANEL);
    p.text("Save to", &p.fonts.body, rect(FOLDER_ROW.left + 16.0, FOLDER_ROW.top, FOLDER_ROW.left + 90.0, FOLDER_ROW.bottom), MUTED);
    p.text(&settings.folder.display().to_string(), &p.fonts.body, rect(FOLDER_ROW.left + 90.0, FOLDER_ROW.top, Element::Latest.rect().left - 12.0, FOLDER_ROW.bottom), if enabled { TEXT } else { DIM });

    if let Some(latest) = &view.latest {
        let color = if view.hovered(Element::Latest) { ACCENT_HOVER } else { ACCENT };

        p.text(&format!("▶  {latest}"), &p.fonts.body_right, Element::Latest.rect(), color);
    }
    p.button(Element::FolderChange.rect(), "Change", enabled, view.hovered(Element::FolderChange));
    p.button(Element::FolderOpen.rect(), "Open", true, view.hovered(Element::FolderOpen));
}

fn draw_record_bar(p: &Painter, view: &View) {
    let button = Element::Record.rect();
    let hovered = view.hovered(Element::Record);
    let cx = (button.left + button.right) / 2.0;
    let cy = (button.top + button.bottom) / 2.0;

    p.circle(cx, cy, 28.0, if hovered { BUTTON_HOVER } else { BUTTON });

    match view.elapsed {
        Some(_) => p.round(rect(cx - 9.0, cy - 9.0, cx + 9.0, cy + 9.0), 3.0, RED),
        None => p.circle(cx, cy, 17.0, if hovered { RED_HOVER } else { RED }),
    }

    let s = view.settings;
    let (clock, clock_color, status) = match view.elapsed {
        Some(seconds) => (
            format!("{:02}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60),
            TEXT,
            format!("Recording  ·  {}×{}  ·  {} FPS  ·  {}", view.resolution.0, view.resolution.1, s.fps, format_in(s.bitrate, s.bitrate_unit)),
        ),
        None => (
            "00:00:00".to_string(),
            MUTED,
            match &view.activity {
                Some(activity) => activity.clone(),
                None => {
                    let state = if view.preview_failed { "Preview unavailable" } else { "Ready" };

                    match &view.record_hotkey {
                        Some(hotkey) => format!("{state}  ·  {hotkey} to start"),
                        None => state.to_string(),
                    }
                }
            },
        ),
    };

    p.text(&clock, &p.fonts.large, rect(94.0, 416.0, 520.0, 448.0), clock_color);
    p.text(&status, &p.fonts.body, rect(94.0, 448.0, 380.0, 474.0), MUTED);

    if view.recording() {
        p.circle(236.0, 432.0, 5.0, RED);
        p.text("REC", &p.fonts.button, rect(244.0, 418.0, 280.0, 446.0), RED);
    }

    p.button(Element::Keybinds.rect(), "Keybinds", true, view.hovered(Element::Keybinds));
    p.button(Element::Compress.rect(), "Compress video", true, view.hovered(Element::Compress));
    p.button(Element::Trim.rect(), "Trim video", true, view.hovered(Element::Trim));
}

fn selector(p: &Painter, view: &View, element: Element, r: D2D_RECT_F, label: &str, value: &str) {
    let enabled = element.enabled(view.recording());

    p.round(r, 8.0, PANEL);
    p.text(label, &p.fonts.body, rect(r.left + 16.0, r.top, element.rect().left - 8.0, r.bottom), MUTED);
    p.dropdown(element.rect(), value, enabled, view.hovered(element));
}

fn dropdown_rect(row: D2D_RECT_F) -> D2D_RECT_F {
    rect(row.left + 90.0, row.top + 8.0, row.right - 8.0, row.bottom - 8.0)
}

fn device_rect(row: D2D_RECT_F) -> D2D_RECT_F {
    rect(row.left + DEVICE_LABEL_WIDTH, row.top + 8.0, row.right - 64.0, row.bottom - 8.0)
}

fn number_row(p: &Painter, view: &View, (field, pill): (Element, Element), r: D2D_RECT_F, label: &str, value: &str, unit: &str) {
    let enabled = field.enabled(view.recording());

    p.round(r, 8.0, PANEL);
    p.text(label, &p.fonts.body, rect(r.left + 16.0, r.top, field.rect().left - 8.0, r.bottom), MUTED);
    p.field(field.rect(), value, enabled, view.hovered(field), view.editing == Some(field));
    p.button(pill.rect(), &format!("{unit}  ▾"), enabled, view.hovered(pill));
}

fn device(p: &Painter, view: &View, element: Element, r: D2D_RECT_F, label: &str, (name, default): &(String, bool), on: bool) {
    let enabled = element.enabled(view.recording());
    let box_left = element.rect().left;

    p.round(r, 8.0, PANEL);

    match default {
        true => {
            p.text(label, &p.fonts.body, rect(r.left + 16.0, r.top + 4.0, box_left - 6.0, r.top + 24.0), MUTED);
            p.text("Windows default", &p.fonts.small, rect(r.left + 16.0, r.top + 22.0, box_left - 6.0, r.bottom - 4.0), DIM);
        }
        false => p.text(label, &p.fonts.body, rect(r.left + 16.0, r.top, box_left - 6.0, r.bottom), MUTED),
    }

    p.dropdown(element.rect(), name, enabled && on, view.hovered(element));
    p.switch(toggle_track(r), on, enabled);
}

fn toggle(p: &Painter, view: &View, element: Element, r: D2D_RECT_F, label: &str, on: bool) {
    let enabled = element.enabled(view.recording());

    p.round(r, 8.0, PANEL);
    p.text(label, &p.fonts.body, rect(r.left + 16.0, r.top, r.left + 120.0, r.bottom), MUTED);

    p.switch(toggle_track(r), on, enabled);
}

fn toggle_track(r: D2D_RECT_F) -> D2D_RECT_F {
    let cy = (r.top + r.bottom) / 2.0;

    rect(r.right - 56.0, cy - 11.0, r.right - 16.0, cy + 11.0)
}

pub fn preview_rect(width: u32, height: u32) -> D2D_RECT_F {
    fit_rect(PREVIEW, width, height)
}

fn fit_rect(area: D2D_RECT_F, width: u32, height: u32) -> D2D_RECT_F {
    let area_width = area.right - area.left;
    let area_height = area.bottom - area.top;
    let scale = (area_width / width as f32).min(area_height / height as f32);
    let fitted_width = width as f32 * scale;
    let fitted_height = height as f32 * scale;
    let left = area.left + (area_width - fitted_width) / 2.0;
    let top = area.top + (area_height - fitted_height) / 2.0;

    rect(left, top, left + fitted_width, top + fitted_height)
}

fn switch_row(p: &Painter, view: &View, label: &str, value: &str, unit: &str, on: bool) {
    let r = REPLAY_ROW;
    let (field, pill) = (Element::ReplayLength, Element::ReplayUnit);
    let enabled = field.enabled(view.recording());

    p.round(r, 8.0, PANEL);
    p.text(label, &p.fonts.body, rect(r.left + 16.0, r.top, field.rect().left - 8.0, r.bottom), MUTED);
    p.field(field.rect(), value, enabled, view.hovered(field), view.editing == Some(field));
    p.button(pill.rect(), &format!("{unit}  ▾"), enabled, view.hovered(pill));
    p.switch(toggle_track(r), on, enabled);
}

fn field_rect(row: D2D_RECT_F) -> D2D_RECT_F {
    rect(row.left + 130.0, row.top + 8.0, row.right - 94.0, row.bottom - 8.0)
}

fn pill_rect(row: D2D_RECT_F) -> D2D_RECT_F {
    rect(row.right - 88.0, row.top + 8.0, row.right - 8.0, row.bottom - 8.0)
}

fn right_zone(r: D2D_RECT_F, width: f32) -> D2D_RECT_F {
    rect(r.right - width, r.top, r.right, r.bottom)
}

const fn setting_row(column: u32, index: u32) -> D2D_RECT_F {
    let top = 490.0 + index as f32 * 52.0;

    match column {
        0 => rect(20.0, top, 354.0, top + 44.0),
        _ => rect(366.0, top, 700.0, top + 44.0),
    }
}
