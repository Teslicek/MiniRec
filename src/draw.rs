use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{Result, w};
use windows_numerics::Vector2;

pub const BACKGROUND: u32 = 0x0f1012;
pub const BLACK: u32 = 0x000000;
pub const PANEL: u32 = 0x18191d;
pub const PANEL_HOVER: u32 = 0x1b1c20;
pub const BUTTON: u32 = 0x26282d;
pub const BUTTON_HOVER: u32 = 0x31333a;
pub const TEXT: u32 = 0xececef;
pub const MUTED: u32 = 0x8e9099;
pub const DIM: u32 = 0x55575e;
pub const RED: u32 = 0xe5484d;
pub const RED_HOVER: u32 = 0xf2656a;
pub const ACCENT: u32 = 0x3d7bfd;
pub const ACCENT_HOVER: u32 = 0x5a8ffd;
pub const ACCENT_DIM: u32 = 0x2a4378;
pub const TRACK_OFF: u32 = 0x3a3c43;
pub const KNOB: u32 = 0xffffff;
pub const KNOB_DIM: u32 = 0x9a9ca3;
pub const FIELD: u32 = 0x0c0d10;
pub const FIELD_ACTIVE: u32 = 0x08090b;
pub const FIELD_HOVER: u32 = 0x111215;
pub const FIELD_FONT_SIZE: f32 = 14.0;

const FIELD_PADDING: f32 = 10.0;
const CHEVRON_ZONE: f32 = 28.0;

pub struct Fonts {
    pub large:        IDWriteTextFormat,
    pub body:         IDWriteTextFormat,
    pub body_right:   IDWriteTextFormat,
    pub field:        IDWriteTextFormat,
    pub button:       IDWriteTextFormat,
    pub small:        IDWriteTextFormat,
    pub small_center: IDWriteTextFormat,
}

pub struct Canvas {
    factory:   ID2D1Factory,
    target:    RefCell<Option<(ID2D1HwndRenderTarget, ID2D1SolidColorBrush)>>,
    pub fonts: Fonts,
    dpi:       Cell<u32>,
}

impl Canvas {
    pub fn new(dpi: u32) -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

            Ok(Self {
                factory,
                target: RefCell::new(None),
                fonts: Fonts {
                    large: text_format(&dwrite, 26.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING)?,
                    body: text_format(&dwrite, 13.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING)?,
                    body_right: text_format(&dwrite, 13.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING)?,
                    field: text_format(&dwrite, FIELD_FONT_SIZE, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING)?,
                    button: text_format(&dwrite, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER)?,
                    small: text_format(&dwrite, 11.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING)?,
                    small_center: text_format(&dwrite, 11.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER)?,
                },
                dpi: Cell::new(dpi),
            })
        }
    }

    pub fn scale(&self) -> f32 {
        self.dpi.get() as f32 / 96.0
    }

    pub fn set_dpi(&self, dpi: u32) {
        self.dpi.set(dpi);

        if let Some((target, _)) = &*self.target.borrow() {
            unsafe { target.SetDpi(dpi as f32, dpi as f32) };
        }
    }

    pub fn release(&self) {
        self.target.replace(None);
    }

    pub fn resize(&self, width: u32, height: u32) -> Result<()> {
        match &*self.target.borrow() {
            Some((target, _)) => unsafe { target.Resize(&D2D_SIZE_U { width, height }) },
            None => Ok(()),
        }
    }

    pub fn paint(&self, window: HWND, draw: impl FnOnce(&Painter)) -> Result<()> {
        unsafe {
            let mut paint = PAINTSTRUCT::default();

            BeginPaint(window, &mut paint);

            let result = self.render(window, draw);

            EndPaint(window, &paint).ok()?;
            result
        }
    }

    fn render(&self, window: HWND, draw: impl FnOnce(&Painter)) -> Result<()> {
        let (target, brush) = self.render_target(window)?;

        unsafe {
            target.BeginDraw();
            draw(&Painter { target: &target, brush: &brush, fonts: &self.fonts });

            match target.EndDraw(None, None) {
                Err(e) if e.code() == D2DERR_RECREATE_TARGET => {
                    self.target.replace(None);
                    InvalidateRect(Some(window), None, false).ok()
                }
                result => result,
            }
        }
    }

    fn render_target(&self, window: HWND) -> Result<(ID2D1HwndRenderTarget, ID2D1SolidColorBrush)> {
        if let Some(existing) = &*self.target.borrow() {
            return Ok(existing.clone());
        }

        unsafe {
            let mut client = RECT::default();

            GetClientRect(window, &mut client)?;

            let dpi = self.dpi.get() as f32;
            let properties = D2D1_RENDER_TARGET_PROPERTIES { dpiX: dpi, dpiY: dpi, ..Default::default() };
            let hwnd_properties = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: window,
                pixelSize: D2D_SIZE_U { width: client.right as u32, height: client.bottom as u32 },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let target = self.factory.CreateHwndRenderTarget(&properties, &hwnd_properties)?;
            let brush = target.CreateSolidColorBrush(&color(TEXT), None)?;

            self.target.replace(Some((target.clone(), brush.clone())));
            Ok((target, brush))
        }
    }
}

pub struct Painter<'a> {
    target:    &'a ID2D1HwndRenderTarget,
    brush:     &'a ID2D1SolidColorBrush,
    pub fonts: &'a Fonts,
}

impl Painter<'_> {
    pub fn clear(&self, hex: u32) {
        unsafe { self.target.Clear(Some(&color(hex))) };
    }

    pub fn round(&self, r: D2D_RECT_F, radius: f32, hex: u32) {
        unsafe {
            self.brush.SetColor(&color(hex));
            self.target.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, self.brush);
        }
    }

    pub fn fill(&self, r: D2D_RECT_F, hex: u32) {
        unsafe {
            self.brush.SetColor(&color(hex));
            self.target.FillRectangle(&r, self.brush);
        }
    }

    pub fn circle(&self, x: f32, y: f32, radius: f32, hex: u32) {
        unsafe {
            self.brush.SetColor(&color(hex));
            self.target.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: x, Y: y }, radiusX: radius, radiusY: radius }, self.brush);
        }
    }

    pub fn text(&self, text: &str, format: &IDWriteTextFormat, r: D2D_RECT_F, hex: u32) {
        let wide: Vec<u16> = text.encode_utf16().collect();

        unsafe {
            self.brush.SetColor(&color(hex));
            self.target.DrawText(&wide, format, &r, self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    pub fn button(&self, r: D2D_RECT_F, label: &str, enabled: bool, hovered: bool) {
        self.round(r, 6.0, if enabled && hovered { BUTTON_HOVER } else { BUTTON });
        self.text(label, &self.fonts.button, r, if enabled { TEXT } else { DIM });
    }

    pub fn dropdown(&self, r: D2D_RECT_F, value: &str, active: bool, hovered: bool) {
        self.round(r, 6.0, if hovered { BUTTON_HOVER } else { BUTTON });
        self.text(value, &self.fonts.body, rect(r.left + FIELD_PADDING, r.top, r.right - CHEVRON_ZONE, r.bottom), if active { TEXT } else { DIM });
        self.text("▾", &self.fonts.button, rect(r.right - CHEVRON_ZONE, r.top, r.right - 4.0, r.bottom), if active { MUTED } else { DIM });
    }

    pub fn field(&self, r: D2D_RECT_F, value: &str, enabled: bool, hovered: bool, editing: bool) {
        let fill = match (editing, enabled && hovered) {
            (true, _) => FIELD_ACTIVE,
            (false, true) => FIELD_HOVER,
            (false, false) => FIELD,
        };

        self.round(r, 6.0, fill);

        if !editing {
            self.text(value, &self.fonts.field, field_text_area(r), if enabled { TEXT } else { DIM });
        }
    }

    pub fn primary_button(&self, r: D2D_RECT_F, label: &str, enabled: bool, hovered: bool) {
        let fill = match (enabled, hovered) {
            (true, true) => ACCENT_HOVER,
            (true, false) => ACCENT,
            (false, _) => ACCENT_DIM,
        };

        self.round(r, 6.0, fill);
        self.text(label, &self.fonts.button, r, if enabled { KNOB } else { KNOB_DIM });
    }

    pub fn switch(&self, track: D2D_RECT_F, on: bool, enabled: bool) {
        let cy = (track.top + track.bottom) / 2.0;
        let radius = (track.bottom - track.top) / 2.0;
        let fill = match (on, enabled) {
            (true, true) => ACCENT,
            (true, false) => ACCENT_DIM,
            (false, _) => TRACK_OFF,
        };
        let knob_x = if on { track.right - radius } else { track.left + radius };

        self.round(track, radius, fill);
        self.circle(knob_x, cy, radius - 3.0, if enabled { KNOB } else { KNOB_DIM });
    }
}

pub const fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F { left, top, right, bottom }
}

pub fn field_text_area(r: D2D_RECT_F) -> D2D_RECT_F {
    rect(r.left + FIELD_PADDING, r.top, r.right - FIELD_PADDING, r.bottom)
}

pub fn contains(r: D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

pub fn to_pixels(r: D2D_RECT_F, scale: f32) -> RECT {
    RECT {
        left: (r.left * scale).round() as i32,
        top: (r.top * scale).round() as i32,
        right: (r.right * scale).round() as i32,
        bottom: (r.bottom * scale).round() as i32,
    }
}

fn color(hex: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

fn text_format(dwrite: &IDWriteFactory, size: f32, weight: DWRITE_FONT_WEIGHT, alignment: DWRITE_TEXT_ALIGNMENT) -> Result<IDWriteTextFormat> {
    unsafe {
        let format = dwrite.CreateTextFormat(w!("Segoe UI"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
        let ellipsis = dwrite.CreateEllipsisTrimmingSign(&format)?;
        let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };

        format.SetTextAlignment(alignment)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        format.SetTrimming(&trimming, &ellipsis)?;
        Ok(format)
    }
}
