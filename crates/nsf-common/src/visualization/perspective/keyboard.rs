//! Piano keyboard strip at the bottom of the canvas.
//!
//! Each MIDI pitch occupies one equal-width lane (so the keyboard
//! aligns perfectly with the falling-note lanes). The keyboard is
//! drawn in two passes:
//!
//! 1. White keys: each white key occupies its own lane in the upper
//!    half, then extends horizontally in the lower half into any
//!    adjacent lane that has no black key — so the bottom of the
//!    keyboard reads as a continuous white surface with the black
//!    keys "sitting on top," matching real-piano appearance.
//! 2. Black keys: drawn as narrower, half-height rectangles centered
//!    on their lane.
//!
//! Active keys (any pitched-note channel playing that pitch in the
//! newest time slice) are recolored with the playing channel's color
//! — same convention as the classic view.

use rusticnes_ui_common::piano_roll_window::{NoteType, PianoRollWindow};

use crate::visualization::perspective::transform::{
    key_center_x_at_keyboard, key_width_at_keyboard, HIGHEST_KEY, KEYBOARD_BOTTOM_Y,
    KEYBOARD_TOP_Y, LOWEST_KEY,
};
use crate::visualization::perspective::{hline, vline, CANVAS_W_I32};

const WHITE_KEY_COLOR: [u8; 4] = [0xe8, 0xe8, 0xe8, 0xFF];
const BLACK_KEY_COLOR: [u8; 4] = [0x1a, 0x1a, 0x1a, 0xFF];
const KEY_BORDER: [u8; 4] = [0x05, 0x05, 0x05, 0xFF];

/// `true` if a MIDI-style key index is a sharp (black key).
fn is_black_key(pitch: i32) -> bool {
    matches!(pitch.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

pub fn draw_keyboard(buf: &mut [u8], piano_roll: &PianoRollWindow) {
    let kb_top = KEYBOARD_TOP_Y as i32;
    let kb_bottom = KEYBOARD_BOTTOM_Y as i32;
    let kw = key_width_at_keyboard();

    // Upper half ends here; below it the white key extends into
    // adjacent lanes (where there's no black key).
    let upper_bottom = kb_top + (kb_bottom - kb_top) / 2;

    // Build an active-key map from the newest time slice: pitch →
    // playing channel's color (used by both white-key and black-key
    // passes). For now we only track pitched channels (Frequency);
    // Noise / Waveform don't map to a particular keyboard key.
    let active = active_keys(piano_roll);

    // First pass: white keys. Each white key's upper half sits in its
    // own lane (kw wide). Lower half extends into adjacent lanes when
    // there's no black key on that side (or the neighbor is outside
    // our visible range).
    for pitch in LOWEST_KEY..=HIGHEST_KEY {
        if is_black_key(pitch) {
            continue;
        }
        let color = active.get_color(pitch).unwrap_or(WHITE_KEY_COLOR);
        let center = key_center_x_at_keyboard(pitch);
        let upper_x0 = (center - kw * 0.5).round() as i32;
        let upper_x1 = (center + kw * 0.5).round() as i32;

        // Upper half — confined to the white key's own lane.
        for y in kb_top..upper_bottom {
            hline(buf, y, upper_x0, upper_x1, color);
        }

        // Lower half — extends left/right into adjacent lanes that
        // don't have a black key. (Edge keys extend only on the side
        // where a black-key neighbor actually exists in our range.)
        let extend_left = pitch > LOWEST_KEY && is_black_key(pitch - 1);
        let extend_right = pitch < HIGHEST_KEY && is_black_key(pitch + 1);
        let lower_x0 = if extend_left {
            (center - kw).round() as i32
        } else {
            upper_x0
        };
        let lower_x1 = if extend_right {
            (center + kw).round() as i32
        } else {
            upper_x1
        };
        for y in upper_bottom..kb_bottom {
            hline(buf, y, lower_x0, lower_x1, color);
        }

    }

    // Second pass: black keys, narrower and exactly half-height of
    // white keys. Aligning the black-key bottom with the lower-half
    // split makes the white-key border (drawn next) fully visible in
    // the bottom half rather than partially overdrawn.
    let black_bottom = upper_bottom;
    let black_half_w = (kw * 0.5).max(2.0);
    for pitch in LOWEST_KEY..=HIGHEST_KEY {
        if !is_black_key(pitch) {
            continue;
        }
        let color = active.get_color(pitch).unwrap_or(BLACK_KEY_COLOR);
        let center = key_center_x_at_keyboard(pitch);
        let x0 = (center - black_half_w).round() as i32;
        let x1 = (center + black_half_w).round() as i32;
        for y in kb_top..black_bottom {
            hline(buf, y, x0, x1, color);
        }
    }

    // Third pass: white-key borders. Drawn LAST so the next white
    // key's lower-half extension can't overdraw them.
    for pitch in LOWEST_KEY..HIGHEST_KEY {
        if is_black_key(pitch) {
            continue;
        }
        let next = pitch + 1;
        if next > HIGHEST_KEY {
            continue;
        }
        let center = key_center_x_at_keyboard(pitch);
        let upper_x1 = (center + kw * 0.5).round() as i32;
        let (border_x, y_top) = if is_black_key(next) {
            // Lower half only — the upper half is the black key cap.
            let bx = key_center_x_at_keyboard(next).round() as i32;
            (bx, upper_bottom)
        } else {
            // Touching whites (E↔F, B↔C): full-height seam.
            (upper_x1, kb_top)
        };
        vline(buf, border_x, y_top, kb_bottom, KEY_BORDER);
    }

    // Top border of the keyboard strip.
    hline(buf, kb_top, 0, CANVAS_W_I32, KEY_BORDER);
}

/// Pitch → channel-color map sourced from the newest time slice's
/// visible Frequency notes. Walked in slice order so later channels
/// overwrite earlier ones at the same pitch (matches the classic
/// view's "topmost playing wins" behavior).
struct ActiveKeys {
    by_pitch: std::collections::HashMap<i32, [u8; 4]>,
}

impl ActiveKeys {
    fn get_color(&self, pitch: i32) -> Option<[u8; 4]> {
        self.by_pitch.get(&pitch).copied()
    }
}

fn active_keys(piano_roll: &PianoRollWindow) -> ActiveKeys {
    let mut by_pitch = std::collections::HashMap::new();
    let lowest_idx = piano_roll.lowest_index as i32;
    if let Some(slice) = piano_roll.time_slices.front() {
        for ch in slice.iter() {
            if !ch.visible {
                continue;
            }
            if !matches!(ch.note_type, NoteType::Frequency) {
                continue;
            }
            let pitch = lowest_idx + ch.y.round() as i32;
            if pitch < LOWEST_KEY || pitch > HIGHEST_KEY {
                continue;
            }
            by_pitch.insert(pitch, [ch.color.r(), ch.color.g(), ch.color.b(), 0xFF]);
        }
    }
    ActiveKeys { by_pitch }
}
