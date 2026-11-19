const TILE_TOP: [f32; 3] = [74.0, 124.0, 246.0];
const TILE_BOTTOM: [f32; 3] = [38.0, 80.0, 214.0];
const GLYPH: [f32; 3] = [255.0, 255.0, 255.0];
const BADGE: [f32; 3] = [236.0, 60.0, 64.0];

pub fn icon_pixels(size: u32, recording: bool) -> Vec<[u8; 4]> {
    let full   = size as f32;
    let center = full / 2.0;
    let badge  = full * 0.75;
    let mut pixels = Vec::with_capacity((size * size) as usize);

    for y in 0..size {
        for x in 0..size {
            let dx       = x as f32 + 0.5 - center;
            let dy       = y as f32 + 0.5 - center;
            let distance = (dx * dx + dy * dy).sqrt();
            let to_badge = ((x as f32 + 0.5 - badge).powi(2) + (y as f32 + 0.5 - badge).powi(2)).sqrt();
            let cut      = if recording { coverage(to_badge - full * 0.32) } else { 1.0 };
            let tile     = coverage(-rounded_box(dx, dy, center - full * 0.03, full * 0.24)) * cut;
            let glyph    = coverage(full * 0.045 - (distance - full * 0.25).abs()).max(coverage(full * 0.12 - distance)) * tile;
            let dot      = if recording { coverage(full * 0.24 - to_badge) } else { 0.0 };
            let shade    = (y as f32 + 0.5) / full;
            let tile_color: [f32; 3] = std::array::from_fn(|i| TILE_TOP[i] + (TILE_BOTTOM[i] - TILE_TOP[i]) * shade);
            let mut color = tile_color.map(|c| c * tile);
            let mut alpha = tile;

            for (layer, cover) in [(GLYPH, glyph), (BADGE, dot)] {
                color = std::array::from_fn(|i| color[i] * (1.0 - cover) + layer[i] * cover);
                alpha = alpha * (1.0 - cover) + cover;
            }

            if alpha <= 0.0 {
                pixels.push([0, 0, 0, 0]);
                continue;
            }

            let channel = |i: usize| (color[i] / alpha).round() as u8;

            pixels.push([channel(2), channel(1), channel(0), (alpha * 255.0).round() as u8]);
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
