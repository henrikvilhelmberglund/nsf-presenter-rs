//! Depth-to-screen-space math for the perspective view.
//!
//! Coordinate convention: y=0 is top of canvas, y=CANVAS_H is bottom.
//! `depth=0.0` means "at the keyboard" (just played), `depth=1.0` means
//! "at the horizon" (far in the future / past, depending on direction).

use crate::player::player_thread::{PLAYER_CANVAS_H, PLAYER_CANVAS_W};

/// y-coordinate of the horizon (top of the trapezoidal "highway").
/// Offset above center to leave room at the bottom for the keyboard
/// and the per-channel oscilloscope strip.
pub const HORIZON_Y: f32 = 210.0;

/// y-coordinate of the top of the keyboard strip / bottom of the note
/// area. Lower value = shorter keyboard strip.
pub const KEYBOARD_TOP_Y: f32 = 430.0;

/// y-coordinate of the bottom of the keyboard. Below this sits the
/// per-channel oscilloscope strip; the keyboard no longer reaches the
/// canvas's bottom edge.
pub const KEYBOARD_BOTTOM_Y: f32 = 470.0;

/// y-coordinate of the top of the per-channel oscilloscope strip at the
/// bottom of the canvas (= `KEYBOARD_BOTTOM_Y`).
pub const CHANNEL_STRIP_TOP_Y: f32 = KEYBOARD_BOTTOM_Y;

/// y-coordinate of the bottom of the channel strip (= bottom of canvas).
pub const CHANNEL_STRIP_BOTTOM_Y: f32 = PLAYER_CANVAS_H as f32;

/// Strength of perspective compression. Higher = more dramatic 3D feel
/// (distant notes appear smaller and move slower in screen-space).
/// At depth=1 (horizon), the trapezoid scale is `1 / (1 + PERSPECTIVE_K)`
/// of full width.
pub const PERSPECTIVE_K: f32 = 4.0;

/// First and last visible pitch (inclusive). MIDI-like indices, where 60
/// is middle C (C4). E1..E7 = 28..100, a 73-key range shifted ~8
/// semitones lower than the obvious C2..C8 — gives more room for the
/// low NES notes that real game music tends to use.
pub const LOWEST_KEY: i32 = 28;
pub const HIGHEST_KEY: i32 = 100;
pub const KEY_RANGE: i32 = HIGHEST_KEY - LOWEST_KEY + 1;

/// y-coordinate of a slice at the given depth (0.0 = keyboard, 1.0 = horizon).
///
/// Uses `1/(1+k*d)` perspective compression so distant notes (near
/// horizon) move slowly in screen-space and accelerate as they approach
/// the keyboard — what your eye expects from a 3D perspective view.
/// A linear mapping would make notes appear to *speed up* unnaturally.
pub fn y_at_depth(depth: f32) -> f32 {
    lerp(KEYBOARD_TOP_Y, HORIZON_Y, perspective_t(depth))
}

/// Trapezoid scale factor at a given depth. 1.0 at depth=0 (keyboard),
/// 1/(1+PERSPECTIVE_K) at depth=1 (horizon).
pub fn scale_at_depth(depth: f32) -> f32 {
    1.0 / (1.0 + PERSPECTIVE_K * depth.clamp(0.0, 1.0))
}

/// Nonlinear depth → screen-space `t` (0..1). Used by `y_at_depth`.
/// Matches the same `1/(1+k*d)` curve that `scale_at_depth` uses, so the
/// trapezoid converges geometrically (a note's vertical position and its
/// width compress at the same rate).
fn perspective_t(depth: f32) -> f32 {
    let d = depth.clamp(0.0, 1.0);
    let t = 1.0 - 1.0 / (1.0 + PERSPECTIVE_K * d);
    let t_max = 1.0 - 1.0 / (1.0 + PERSPECTIVE_K);
    t / t_max
}

/// X-coordinate of the *center* of a given pitch's lane at the keyboard
/// (depth=0). For pitches outside the visible range, returns the nearest
/// edge.
pub fn key_center_x_at_keyboard(pitch: i32) -> f32 {
    let clamped = pitch.clamp(LOWEST_KEY, HIGHEST_KEY);
    let normalized = (clamped - LOWEST_KEY) as f32 / KEY_RANGE as f32;
    (normalized + 0.5 / KEY_RANGE as f32) * PLAYER_CANVAS_W as f32
}

/// Width of one key's lane at the keyboard.
pub fn key_width_at_keyboard() -> f32 {
    PLAYER_CANVAS_W as f32 / KEY_RANGE as f32
}

/// X-coordinate of the *center* of a given pitch's lane at the given depth.
/// Lanes converge toward `CANVAS_W / 2` (the vanishing point) as depth → 1.
pub fn lane_center_x(pitch: i32, depth: f32) -> f32 {
    lane_center_x_f(pitch as f32, depth)
}

/// Same as `lane_center_x` but takes a fractional pitch so vibrato /
/// pitch-bend is rendered at sub-key positions.
pub fn lane_center_x_f(pitch: f32, depth: f32) -> f32 {
    let center = PLAYER_CANVAS_W as f32 * 0.5;
    let clamped = pitch.clamp(LOWEST_KEY as f32, HIGHEST_KEY as f32);
    let normalized = (clamped - LOWEST_KEY as f32) / KEY_RANGE as f32;
    let kb_x = (normalized + 0.5 / KEY_RANGE as f32) * PLAYER_CANVAS_W as f32;
    center + (kb_x - center) * scale_at_depth(depth)
}

/// Width of one key's lane at the given depth.
pub fn lane_width_at_depth(depth: f32) -> f32 {
    key_width_at_keyboard() * scale_at_depth(depth)
}

/// Far edge of the sky trapezoid. Conceptually the mirror of the
/// keyboard across the horizon, but capped at the top of the screen
/// (the horizon sits above center now, so a literal mirror would land
/// off-screen above y=0).
pub const MIRROR_TOP_Y: f32 = 0.0;

/// Number of noise "strings" the APU exposes — matches rusticnes's
/// arbitrary mapping of LFSR rates to 16 lanes (see `slice_from_channel`
/// in `piano_roll_window.rs`). The sky spreads these across its full
/// width so distinct noise notes read as distinct positions, not a single
/// stack at the left edge.
pub const NOISE_STRINGS: i32 = 16;

/// How many piano-key lanes wide the waveform rail is. Centered on the
/// rightmost piano keys: the rail spans keys
/// `HIGHEST_KEY - WAVEFORM_RAIL_KEYS + 1 ..= HIGHEST_KEY`.
pub const WAVEFORM_RAIL_KEYS: i32 = 5;

/// Geometry of the waveform rail at a given depth. The rail spans the
/// rightmost `WAVEFORM_RAIL_KEYS` piano-key lanes (mirror of how noise
/// occupies the leftmost 16 lanes). Returns `(center_x, half_width)`
/// using the same perspective scaling as floor lanes — so the rail
/// converges to the same horizon point as the rightmost piano keys.
pub fn waveform_rail_at_depth(depth: f32) -> (f32, f32) {
    let first_pitch = (HIGHEST_KEY - WAVEFORM_RAIL_KEYS + 1) as f32;
    let last_pitch = HIGHEST_KEY as f32;
    let left_edge = lane_center_x_f(first_pitch, depth) - lane_width_at_depth(depth) * 0.5;
    let right_edge = lane_center_x_f(last_pitch, depth) + lane_width_at_depth(depth) * 0.5;
    let center = (left_edge + right_edge) * 0.5;
    let half_width = (right_edge - left_edge) * 0.5;
    (center, half_width)
}

/// X-coordinate of the center of a noise "string" lane in the sky at
/// the given depth. Noise strings share lane geometry with the floor's
/// leftmost 16 piano-key lanes — string `i` lives in the lane of piano
/// key `LOWEST_KEY + i`. So sky-noise lanes are literally the mirror of
/// those floor lanes across the horizon: same X at every depth (using
/// the floor's perspective scale), same horizon convergence point.
pub fn noise_lane_center_x_f(string_idx: f32, depth: f32) -> f32 {
    let clamped = string_idx.clamp(0.0, (NOISE_STRINGS - 1) as f32);
    let pitch = LOWEST_KEY as f32 + clamped;
    lane_center_x_f(pitch, depth)
}

/// X-coordinate of a key BOUNDARY at the keyboard. `boundary_index = 0`
/// is the left edge of the leftmost key; `boundary_index = KEY_RANGE` is
/// the right edge of the rightmost key.
pub fn boundary_x_at_keyboard(boundary_index: i32) -> f32 {
    (boundary_index as f32 / KEY_RANGE as f32) * PLAYER_CANVAS_W as f32
}

/// X-coordinate of a key boundary at the given depth — same perspective
/// scaling as `lane_center_x` so grid + notes share geometry.
pub fn boundary_x_at_depth(boundary_index: i32, depth: f32) -> f32 {
    let center = PLAYER_CANVAS_W as f32 * 0.5;
    let kb_x = boundary_x_at_keyboard(boundary_index);
    center + (kb_x - center) * scale_at_depth(depth)
}

/// Y mapping for the sky (above the horizon). Mirror of the floor's
/// depth semantics: depth=0 is the *far edge from horizon* (top of
/// screen — where new slices appear, mirror of how new floor slices
/// appear at the keyboard), depth=1 is at the horizon.
///
/// Uses the same `perspective_t` curve as the floor's `y_at_depth`, so
/// when combined with the floor's `scale_at_depth` (used by noise +
/// waveform X math), the sky lanes are straight diagonals that perfectly
/// mirror the floor lanes across the horizon.
pub fn y_above_horizon(depth: f32) -> f32 {
    lerp(MIRROR_TOP_Y, HORIZON_Y, perspective_t(depth))
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
