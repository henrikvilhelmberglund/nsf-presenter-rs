//! Vanishing-point grid lines.
//!
//! Draws one line per key BOUNDARY from the keyboard up to the horizon
//! vanishing point. Lines fade toward the horizon so the floor reads
//! as a 3D plane receding into the distance. When the AA toggle is on
//! the diagonals are alpha-blended (Wu-style); otherwise they're plain
//! Bresenham with the brightness modulated to simulate fade.

use crate::visualization::perspective::transform::{
    boundary_x_at_depth, boundary_x_at_keyboard, HORIZON_Y, KEYBOARD_TOP_Y, KEY_RANGE, LOWEST_KEY,
};
use crate::visualization::perspective::{blend_pixel, put_pixel};

const LANE_COLOR: [u8; 4] = [0x22, 0x22, 0x2c, 0xFF];
const OCTAVE_COLOR: [u8; 4] = [0x3a, 0x3a, 0x4a, 0xFF];
const HORIZON_LINE_COLOR: [u8; 4] = [0x40, 0x40, 0x50, 0xFF];
/// Canvas background — used to interpolate the line color toward black
/// in non-AA mode to fake a fade without alpha blending.
const BG_COLOR: [u8; 4] = [0x10, 0x10, 0x14, 0xFF];

/// Alpha (or brightness) at the horizon end of each line. 1.0 = no
/// fade, 0.0 = invisible. A small non-zero value keeps the lines just
/// barely visible at the horizon so the lane positions are still
/// readable up there.
const HORIZON_FADE: f32 = 0.15;

pub fn draw_grid(buf: &mut [u8], disable_aa: bool) {
    let horizon_y = HORIZON_Y as i32;
    let keyboard_y = KEYBOARD_TOP_Y as f32;
    let horizon_yf = HORIZON_Y;

    // One line per key BOUNDARY (= KEY_RANGE + 1 lines for KEY_RANGE
    // keys). C-natural boundaries get a brighter "octave" color.
    let mut horizon_left = i32::MAX;
    let mut horizon_right = i32::MIN;
    for b in 0..=KEY_RANGE {
        let kb_x = boundary_x_at_keyboard(b);
        let horizon_x = boundary_x_at_depth(b, 1.0);
        let hx = horizon_x.round() as i32;
        horizon_left = horizon_left.min(hx);
        horizon_right = horizon_right.max(hx);

        let pitch_at_boundary = LOWEST_KEY + b;
        let color = if pitch_at_boundary.rem_euclid(12) == 0 {
            OCTAVE_COLOR
        } else {
            LANE_COLOR
        };

        // alpha0 at the keyboard end (full), alpha1 at the horizon end (fade).
        if disable_aa {
            draw_line_faded(buf, kb_x, keyboard_y, horizon_x, horizon_yf, color);
        } else {
            draw_aa_line_faded(buf, kb_x, keyboard_y, 1.0, horizon_x, horizon_yf, HORIZON_FADE, color);
        }
    }

    // Horizon line spans only the visible-lane range. Faded since it
    // sits at the far end of the perspective.
    let horizon_color = scale_color(HORIZON_LINE_COLOR, HORIZON_FADE);
    for x in horizon_left..=horizon_right {
        put_pixel(buf, x, horizon_y, horizon_color);
    }
}

/// Linearly blend `color` toward black by `fade` (1.0 = full color, 0
/// = background). Used in non-AA mode to fake the distance fade.
fn scale_color(color: [u8; 4], fade: f32) -> [u8; 4] {
    let a = fade.clamp(0.0, 1.0);
    let inv = 1.0 - a;
    [
        (color[0] as f32 * a + BG_COLOR[0] as f32 * inv) as u8,
        (color[1] as f32 * a + BG_COLOR[1] as f32 * inv) as u8,
        (color[2] as f32 * a + BG_COLOR[2] as f32 * inv) as u8,
        color[3],
    ]
}

/// Non-AA Bresenham line with per-pixel distance fade. The line's
/// color is interpolated toward the background based on how far each
/// pixel is from the keyboard end.
fn draw_line_faded(buf: &mut [u8], x0: f32, y0: f32, x1: f32, y1: f32, color: [u8; 4]) {
    let x0_r = x0.round() as i32;
    let y0_r = y0.round() as i32;
    let x1_r = x1.round() as i32;
    let y1_r = y1.round() as i32;

    let dx = (x1_r - x0_r).abs();
    let sx = if x0_r < x1_r { 1 } else { -1 };
    let dy = -(y1_r - y0_r).abs();
    let sy = if y0_r < y1_r { 1 } else { -1 };
    let mut err = dx + dy;

    let mut x = x0_r;
    let mut y = y0_r;
    // Total path length for the fade interpolation.
    let total = (dx + (-dy)).max(1) as f32;
    let mut step = 0.0_f32;
    loop {
        let t = (step / total).clamp(0.0, 1.0);
        let fade = lerp(1.0, HORIZON_FADE, t);
        put_pixel(buf, x, y, scale_color(color, fade));
        if x == x1_r && y == y1_r {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            if x == x1_r {
                break;
            }
            err += dy;
            x += sx;
            step += 1.0;
        }
        if e2 <= dx {
            if y == y1_r {
                break;
            }
            err += dx;
            y += sy;
            step += 1.0;
        }
    }
}

/// AA line with per-pixel distance fade. Walks one pixel at a time
/// along the major axis (the longer of dx/dy) and alpha-blends the
/// two pixels straddling the line's true subpixel position. The
/// `alpha0` / `alpha1` parameters multiply the per-pixel coverage so
/// the line fades from keyboard to horizon.
fn draw_aa_line_faded(
    buf: &mut [u8],
    x0: f32, y0: f32, alpha0: f32,
    x1: f32, y1: f32, alpha1: f32,
    color: [u8; 4],
) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let steep = dy.abs() > dx.abs();

    // Iterate along the major axis: shallow lines step x, steep lines
    // step y. Swap so the iteration always runs low→high.
    if steep {
        let (y0, x0, y1, x1, a0, a1) = if y0 <= y1 {
            (y0, x0, y1, x1, alpha0, alpha1)
        } else {
            (y1, x1, y0, x0, alpha1, alpha0)
        };
        let gradient = if (y1 - y0).abs() < f32::EPSILON {
            0.0
        } else {
            (x1 - x0) / (y1 - y0)
        };
        let y_start = y0.round() as i32;
        let y_end = y1.round() as i32;
        let span = (y1 - y0).max(1.0);
        for y in y_start..=y_end {
            let x = x0 + (y as f32 - y0) * gradient;
            let x_floor = x.floor();
            let frac = x - x_floor;
            let t = ((y as f32 - y0) / span).clamp(0.0, 1.0);
            let fade = lerp(a0, a1, t);
            blend_pixel(buf, x_floor as i32, y, color, (1.0 - frac) * fade);
            blend_pixel(buf, x_floor as i32 + 1, y, color, frac * fade);
        }
    } else {
        let (x0, y0, x1, y1, a0, a1) = if x0 <= x1 {
            (x0, y0, x1, y1, alpha0, alpha1)
        } else {
            (x1, y1, x0, y0, alpha1, alpha0)
        };
        let gradient = if (x1 - x0).abs() < f32::EPSILON {
            0.0
        } else {
            (y1 - y0) / (x1 - x0)
        };
        let x_start = x0.round() as i32;
        let x_end = x1.round() as i32;
        let span = (x1 - x0).max(1.0);
        for x in x_start..=x_end {
            let y = y0 + (x as f32 - x0) * gradient;
            let y_floor = y.floor();
            let frac = y - y_floor;
            let t = ((x as f32 - x0) / span).clamp(0.0, 1.0);
            let fade = lerp(a0, a1, t);
            blend_pixel(buf, x, y_floor as i32, color, (1.0 - frac) * fade);
            blend_pixel(buf, x, y_floor as i32 + 1, color, frac * fade);
        }
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
