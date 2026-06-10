//! Vanishing-point grid lines.
//!
//! Draws one line per visible key from the key's center at the keyboard
//! up to the vanishing point at the horizon. The lines define the lane
//! geometry that note trapezoids will follow.

use crate::visualization::perspective::transform::{
    boundary_x_at_depth, boundary_x_at_keyboard, HORIZON_Y, KEYBOARD_TOP_Y, KEY_RANGE, LOWEST_KEY,
};
use crate::visualization::perspective::put_pixel;

const LANE_COLOR: [u8; 4] = [0x22, 0x22, 0x2c, 0xFF];
const OCTAVE_COLOR: [u8; 4] = [0x3a, 0x3a, 0x4a, 0xFF];
const HORIZON_LINE_COLOR: [u8; 4] = [0x40, 0x40, 0x50, 0xFF];

pub fn draw_grid(buf: &mut [u8]) {
    let horizon_y = HORIZON_Y as i32;
    let keyboard_y = KEYBOARD_TOP_Y as i32;

    // One line per key BOUNDARY (= KEY_RANGE + 1 lines for KEY_RANGE
    // keys). This way the lane lines line up with the visible edges of
    // the keys on the keyboard strip, not the centers. C-natural
    // boundaries get a brighter "octave" color.
    let mut horizon_left = i32::MAX;
    let mut horizon_right = i32::MIN;
    for b in 0..=KEY_RANGE {
        let kb_x = boundary_x_at_keyboard(b);
        let horizon_x = boundary_x_at_depth(b, 1.0);
        let hx = horizon_x.round() as i32;
        horizon_left = horizon_left.min(hx);
        horizon_right = horizon_right.max(hx);
        // Octave lines fall at boundaries left-of-C (key index where
        // (LOWEST_KEY + b) is C). Highlight those.
        let pitch_at_boundary = LOWEST_KEY + b;
        let color = if pitch_at_boundary.rem_euclid(12) == 0 {
            OCTAVE_COLOR
        } else {
            LANE_COLOR
        };
        draw_line(buf, kb_x, keyboard_y as f32, horizon_x, horizon_y as f32, color);
    }

    // Horizon line spans only the visible-lane range.
    for x in horizon_left..=horizon_right {
        put_pixel(buf, x, horizon_y, HORIZON_LINE_COLOR);
    }
}

/// Plain Bresenham (integer math, no AA) between two float endpoints —
/// matches the player's "crisp pixel" aesthetic.
fn draw_line(buf: &mut [u8], x0: f32, y0: f32, x1: f32, y1: f32, color: [u8; 4]) {
    let mut x0 = x0.round() as i32;
    let mut y0 = y0.round() as i32;
    let x1 = x1.round() as i32;
    let y1 = y1.round() as i32;

    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        put_pixel(buf, x0, y0, color);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            if x0 == x1 {
                break;
            }
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            if y0 == y1 {
                break;
            }
            err += dx;
            y0 += sy;
        }
    }
}
