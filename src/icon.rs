const TILE_TOP: [f32; 3] = [74.0, 124.0, 246.0];
const TILE_BOTTOM: [f32; 3] = [38.0, 80.0, 214.0];
const GLYPH: [f32; 3] = [255.0, 255.0, 255.0];
const RECORDING_BADGE: ([f32; 3], f32) = ([236.0, 60.0, 64.0], 0.75);
const REPLAY_BADGE: ([f32; 3], f32) = ([46.0, 196.0, 98.0], 0.25);
const BADGE_RADIUS: f32 = 0.22;
const BADGE_GAP: f32 = 0.07;

pub fn icon_pixels(size: u32, recording: bool, replay: bool) -> Vec<[u8; 4]> {
    let full   = size as f32;
    let center = full / 2.0;
    let badges: Vec<([f32; 3], f32)> = [(recording, RECORDING_BADGE), (replay, REPLAY_BADGE)].into_iter().filter(|(on, _)| *on).map(|(_, badge)| badge).collect();
    let mut pixels = Vec::with_capacity((size * size) as usize);

    for y in 0..size {
        for x in 0..size {
            let px       = x as f32 + 0.5;
            let py       = y as f32 + 0.5;
            let dx       = px - center;
            let dy       = py - center;
            let distance = (dx * dx + dy * dy).sqrt();
            let dots: Vec<([f32; 3], f32)> = badges.iter().map(|&(tint, top)| (tint, ((px - full * 0.75).powi(2) + (py - full * top).powi(2)).sqrt())).collect();
            let cut      = dots.iter().map(|&(_, to_badge)| coverage(to_badge - full * (BADGE_RADIUS + BADGE_GAP))).product::<f32>();
            let tile     = coverage(-rounded_box(dx, dy, center - full * 0.03, full * 0.24)) * cut;
            let glyph    = coverage(full * 0.045 - (distance - full * 0.25).abs()).max(coverage(full * 0.12 - distance)) * tile;
            let shade    = py / full;
            let tile_color: [f32; 3] = std::array::from_fn(|i| TILE_TOP[i] + (TILE_BOTTOM[i] - TILE_TOP[i]) * shade);
            let mut color = tile_color.map(|c| c * tile);
            let mut alpha = tile;
            let layers = std::iter::once((GLYPH, glyph)).chain(dots.iter().map(|&(tint, to_badge)| (tint, coverage(full * BADGE_RADIUS - to_badge))));

            for (layer, cover) in layers {
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
