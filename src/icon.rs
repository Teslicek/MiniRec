const ICON_BACK: [f32; 3] = [28.0, 29.0, 33.0];
const ICON_RED: [f32; 3] = [229.0, 72.0, 77.0];

pub fn icon_pixels(size: u32, recording: bool) -> Vec<[u8; 4]> {
    let full   = size as f32;
    let center = full / 2.0;
    let radius = if recording { full * 0.42 } else { full * 0.27 };
    let mut pixels = Vec::with_capacity((size * size) as usize);

    for y in 0..size {
        for x in 0..size {
            let dx   = x as f32 + 0.5 - center;
            let dy   = y as f32 + 0.5 - center;
            let dot  = coverage(radius - (dx * dx + dy * dy).sqrt());
            let back = if recording { 0.0 } else { coverage(-rounded_box(dx, dy, center - full * 0.03, full * 0.22)) };
            let alpha = dot + back * (1.0 - dot);

            if alpha <= 0.0 {
                pixels.push([0, 0, 0, 0]);
                continue;
            }

            let channel = |i: usize| ((ICON_RED[i] * dot + ICON_BACK[i] * back * (1.0 - dot)) / alpha) as u8;

            pixels.push([channel(2), channel(1), channel(0), (alpha * 255.0) as u8]);
        }
    }

    pixels
}

fn coverage(distance_inside: f32) -> f32 {
    (distance_inside + 0.5).clamp(0.0, 1.0)
}

fn rounded_box(x: f32, y: f32, half: f32, radius: f32) -> f32 {
    let qx = x.abs() - half + radius;
    let qy = y.abs() - half + radius;

    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius
}
