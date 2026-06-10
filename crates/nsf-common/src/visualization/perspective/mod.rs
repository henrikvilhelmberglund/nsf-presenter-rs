//! Perspective (3D-like) visualization mode for nsf-player.
//!
//! Renders the same audio data the classic 2D piano roll uses — pulled from
//! the rusticnes `PianoRollWindow`'s `time_slices` deque — but with a
//! trapezoidal/perspective layout: piano keyboard at the bottom, notes
//! falling from a vanishing-point horizon toward the keys.
//!
//! The renderer owns a reusable RGBA buffer (960×540) and exposes a single
//! `render(&PianoRollWindow)` entry point. The GUI calls this in place of
//! `Emulator::get_piano_roll_frame()` when the user selects the perspective
//! view mode.

pub mod channel_strip;
pub mod grid;
pub mod keyboard;
pub mod notes;
pub mod transform;

use rusticnes_core::apu::AudioChannelState;
use rusticnes_ui_common::piano_roll_window::PianoRollWindow;

use crate::player::player_thread::{PLAYER_CANVAS_H, PLAYER_CANVAS_W};

pub(crate) const CANVAS_W_I32: i32 = PLAYER_CANVAS_W as i32;

pub struct PerspectiveRenderer {
    // Reusable 960×540 RGBA buffer; one alloc, repeated overwrites.
    buffer: Vec<u8>,
}

impl PerspectiveRenderer {
    pub fn new() -> Self {
        Self {
            buffer: vec![0u8; (PLAYER_CANVAS_W * PLAYER_CANVAS_H * 4) as usize],
        }
    }

    /// Render a frame into the internal buffer and return a slice for the
    /// caller to copy into a SharedPixelBuffer / arc-swap. The slice is
    /// valid until the next call to `render`.
    pub fn render(
        &mut self,
        piano_roll: &PianoRollWindow,
        channels: &[&dyn AudioChannelState],
    ) -> &[u8] {
        clear(&mut self.buffer, [0x10, 0x10, 0x14, 0xFF]);
        grid::draw_grid(&mut self.buffer);
        notes::draw_notes(&mut self.buffer, piano_roll);
        keyboard::draw_keyboard(&mut self.buffer, piano_roll);
        channel_strip::draw_channel_strip(
            &mut self.buffer,
            piano_roll,
            channels,
            CANVAS_W_I32,
            PLAYER_CANVAS_H as i32,
        );
        &self.buffer
    }
}

/// Fill the entire RGBA buffer with one color.
fn clear(buf: &mut [u8], rgba: [u8; 4]) {
    for chunk in buf.chunks_exact_mut(4) {
        chunk.copy_from_slice(&rgba);
    }
}

/// Write a single RGBA pixel into the buffer at (x, y). Bounds-checked.
pub(crate) fn put_pixel(buf: &mut [u8], x: i32, y: i32, rgba: [u8; 4]) {
    if x < 0 || y < 0 {
        return;
    }
    let (x, y) = (x as u32, y as u32);
    if x >= PLAYER_CANVAS_W || y >= PLAYER_CANVAS_H {
        return;
    }
    let idx = ((y * PLAYER_CANVAS_W + x) * 4) as usize;
    buf[idx..idx + 4].copy_from_slice(&rgba);
}

/// Horizontal solid-color span (inclusive `x0`, exclusive `x1`).
pub(crate) fn hline(buf: &mut [u8], y: i32, x0: i32, x1: i32, rgba: [u8; 4]) {
    if y < 0 || y >= PLAYER_CANVAS_H as i32 {
        return;
    }
    let x0 = x0.max(0);
    let x1 = x1.min(PLAYER_CANVAS_W as i32);
    if x1 <= x0 {
        return;
    }
    for x in x0..x1 {
        let idx = ((y as u32 * PLAYER_CANVAS_W + x as u32) * 4) as usize;
        buf[idx..idx + 4].copy_from_slice(&rgba);
    }
}

/// Vertical solid-color span (inclusive `y0`, exclusive `y1`).
pub(crate) fn vline(buf: &mut [u8], x: i32, y0: i32, y1: i32, rgba: [u8; 4]) {
    if x < 0 || x >= PLAYER_CANVAS_W as i32 {
        return;
    }
    let y0 = y0.max(0);
    let y1 = y1.min(PLAYER_CANVAS_H as i32);
    if y1 <= y0 {
        return;
    }
    for y in y0..y1 {
        let idx = ((y as u32 * PLAYER_CANVAS_W + x as u32) * 4) as usize;
        buf[idx..idx + 4].copy_from_slice(&rgba);
    }
}
