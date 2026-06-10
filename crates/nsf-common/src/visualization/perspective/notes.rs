//! Notes layer for the perspective view.
//!
//! Walks the piano-roll's `time_slices` deque, and for each consecutive
//! pair of slices where a channel is playing the same note in both,
//! draws a filled trapezoid connecting them. This eliminates the gaps
//! the per-slice-rectangle approach left behind.
//!
//! Three note-type renderings:
//! - `Frequency` (regular pitched note): falls down the keyboard lane
//!   matching its pitch.
//! - `Waveform` (DMC / sampled): not a pitched note; rendered as a lane
//!   from the LEFT edge of the screen converging on the horizon center.
//! - `Noise`: rendered as a lane from the RIGHT edge converging on the
//!   horizon center.

use rusticnes_ui_common::piano_roll_window::{ChannelSlice, NoteType, PianoRollWindow};

use crate::visualization::perspective::transform::{
    lane_center_x_f, lane_width_at_depth, noise_lane_center_x_f, waveform_rail_at_depth,
    y_above_horizon, y_at_depth, HIGHEST_KEY, HORIZON_Y, KEYBOARD_TOP_Y, LOWEST_KEY,
    NOISE_STRINGS,
};
use crate::visualization::perspective::{blend_pixel, hline};

/// How many recent slices we display. Sized for the larger of the two
/// vertical regions (sky for Wave/Noise = HORIZON_Y pixels, or note
/// area below horizon = KEYBOARD_TOP_Y - HORIZON_Y pixels) so neither
/// area is starved for resolution.
fn depth_capacity() -> usize {
    let above = HORIZON_Y as usize;
    let below = (KEYBOARD_TOP_Y - HORIZON_Y) as usize;
    above.max(below)
}

pub fn draw_notes(buf: &mut [u8], piano_roll: &PianoRollWindow) {
    let depth_cap = depth_capacity();
    let lowest_idx = piano_roll.lowest_index as i32;
    let aa = !piano_roll.disable_aa;

    let slices: Vec<&Vec<ChannelSlice>> =
        piano_roll.time_slices.iter().take(depth_cap + 1).collect();
    if slices.len() < 2 {
        return;
    }

    // Walk consecutive pairs; draw a trapezoid wherever the same channel
    // is playing the same "key" in both. Iterate front-to-back (newest
    // → oldest, equivalently depth 0 → 1).
    for i in 0..slices.len() - 1 {
        let depth_curr = i as f32 / depth_cap as f32;
        let depth_next = (i + 1) as f32 / depth_cap as f32;
        if depth_next > 1.0 {
            break;
        }

        let curr = slices[i];
        let next = slices[i + 1];
        let n_ch = curr.len().min(next.len());

        for ch_idx in 0..n_ch {
            let c = &curr[ch_idx];
            let n = &next[ch_idx];
            if !c.visible || !n.visible {
                continue;
            }
            // Only connect when both slices are the same note-type;
            // mixing Frequency / Noise / Waveform shouldn't merge.
            if c.note_type != n.note_type {
                continue;
            }
            // Connection criteria differ by note type:
            //   - Frequency: connect when the float pitches are close
            //     enough to be "the same note" — required for vibrato
            //     that swings across a key boundary (e.g. 59.6 ↔ 60.6
            //     round to different keys but the X math interpolates
            //     smoothly so the connecting trapezoid is correct).
            //     Threshold is tight enough that a real note-jump
            //     between unrelated pitches still gets skipped.
            //   - Noise: LFSR "strings" are discrete; require exact
            //     same rounded string.
            //   - Waveform: `slice.y` is always 0; always connect.
            match c.note_type {
                NoteType::Frequency => {
                    if (c.y - n.y).abs() > 0.6 {
                        continue;
                    }
                }
                NoteType::Noise => {
                    if c.y.round() as i32 != n.y.round() as i32 {
                        continue;
                    }
                }
                NoteType::Waveform => {}
            }

            let Some((cx_c, half_w_c)) = screen_pos(c, depth_curr, lowest_idx) else { continue };
            let Some((cx_n, half_w_n)) = screen_pos(n, depth_next, lowest_idx) else { continue };

            let y_curr = y_for_slice(c.note_type, depth_curr);
            let y_next = y_for_slice(c.note_type, depth_next);

            rasterize_trapezoid(
                buf,
                y_next, cx_n - half_w_n, cx_n + half_w_n,
                y_curr, cx_c - half_w_c, cx_c + half_w_c,
                color_to_rgba(c.color),
                aa,
            );
        }
    }
}

/// Screen-space center-x and half-width for a slice at the given depth.
/// Returns `None` if the note should be culled (e.g. Frequency note
/// outside our key range).
fn screen_pos(slice: &ChannelSlice, depth: f32, lowest_idx: i32) -> Option<(f32, f32)> {
    // Match the classic view's `draw_slice_vert` width formula
    // exactly: `full_width = thickness * lane_width / 2`, i.e.
    // `half_width = thickness * lane_width / 4`. Thickness is
    // `amplitude * 6`, so this scales linearly from ~0 (silent) up
    // to 3 lane-widths (full-amplitude attack) — preserving the
    // dynamic range that gives notes their attack-decay triangle
    // shape in the classic view.
    let thickness = slice.thickness.clamp(0.2, 6.0);
    let half_w_factor = thickness * 0.25;

    match slice.note_type {
        NoteType::Frequency => {
            // Use fractional pitch (no .round()) so vibrato / pitch-bend
            // show up as sub-lane horizontal motion.
            let float_pitch = lowest_idx as f32 + slice.y;
            // Cull notes whose nominal (rounded) pitch is outside the
            // visible range; for in-range notes we use the float pitch
            // for actual positioning.
            let nominal = lowest_idx + slice.y.round() as i32;
            if nominal < LOWEST_KEY || nominal > HIGHEST_KEY {
                return None;
            }
            let cx = lane_center_x_f(float_pitch, depth);
            let lane_w = lane_width_at_depth(depth);
            Some((cx, lane_w * half_w_factor))
        }
        NoteType::Noise => {
            // rusticnes maps LFSR rates onto 16 arbitrary "strings" in
            // `slice.y` (0..16). Each string occupies the same lane as
            // the floor's piano key `LOWEST_KEY + string_idx` (mirror of
            // the floor lane across the horizon).
            let string_idx = slice.y.clamp(0.0, (NOISE_STRINGS - 1) as f32);
            let cx = noise_lane_center_x_f(string_idx, depth);
            let lane_w = lane_width_at_depth(depth);
            Some((cx, lane_w * half_w_factor))
        }
        NoteType::Waveform => {
            // DPCM: no pitch axis. Falls down the dedicated waveform
            // rail on the right side of the sky. Width is linear in
            // `slice.thickness` (which is `amplitude * 6`, range 0..6)
            // — *not* the shared `volume_factor`, which saturates at
            // thickness=2 and would flatten loud drum hits into a
            // square rectangle. With linear scaling, the natural
            // start-middle-end envelope of a drum hit reads as a
            // tapered shape inside the rail.
            let (cx, rail_hw) = waveform_rail_at_depth(depth);
            let amp = (slice.thickness / 6.0).clamp(0.05, 1.0);
            Some((cx, rail_hw * amp))
        }
    }
}

/// Y-coordinate for a slice based on its note type. Frequency notes use
/// the perspective curve below the horizon; Noise/Waveform use a linear
/// mapping in the sky area above the horizon.
fn y_for_slice(note_type: NoteType, depth: f32) -> f32 {
    match note_type {
        NoteType::Frequency => y_at_depth(depth),
        NoteType::Noise | NoteType::Waveform => y_above_horizon(depth),
    }
}

/// Scanline-fill a 4-corner trapezoid where the top and bottom edges are
/// horizontal (parallel to the x-axis). The two `y_*` arguments can be
/// passed in either screen-order — we auto-sort so floor lanes (where
/// the newer slice sits lower on screen) and sky lanes (where the newer
/// slice sits higher on screen) both render correctly.
///
/// When `aa` is true, the diagonal left/right edges are alpha-blended
/// per-row using fractional pixel coverage (same approach as the
/// classic view's `draw_slice_vert`). Top and bottom edges stay crisp
/// — consecutive chunks share those edges, so AA there would
/// double-blend at every chunk boundary.
fn rasterize_trapezoid(
    buf: &mut [u8],
    y_a: f32, a_left_x: f32, a_right_x: f32,
    y_b: f32, b_left_x: f32, b_right_x: f32,
    color: [u8; 4],
    aa: bool,
) {
    let (y_top, top_left_x, top_right_x, y_bot, bot_left_x, bot_right_x) = if y_a <= y_b {
        (y_a, a_left_x, a_right_x, y_b, b_left_x, b_right_x)
    } else {
        (y_b, b_left_x, b_right_x, y_a, a_left_x, a_right_x)
    };

    let y0 = y_top.round() as i32;
    let y1 = y_bot.round() as i32;
    if y1 <= y0 {
        // Degenerate — draw a single row at y0 so the chunk stays
        // visible even where slices stack tightly (e.g. at the horizon).
        // Use the *average* of top/bottom X so neighboring degenerate
        // chunks blend into each other instead of stair-stepping, and
        // AA the edges when AA is on so the row's left/right pixels
        // match the smooth blending the multi-row path produces.
        let lx_f = (top_left_x + bot_left_x) * 0.5;
        let rx_f = (top_right_x + bot_right_x) * 0.5;
        if aa {
            fill_row_aa(buf, y0, lx_f, rx_f, color);
        } else {
            let lx = lx_f.round() as i32;
            let rx = rx_f.round() as i32 + 1;
            hline(buf, y0, lx, rx, color);
        }
        return;
    }

    let dy = (y1 - y0) as f32;
    for y in y0..=y1 {
        let t = (y - y0) as f32 / dy;
        let lx_f = lerp(top_left_x, bot_left_x, t);
        let rx_f = lerp(top_right_x, bot_right_x, t);
        if aa {
            fill_row_aa(buf, y, lx_f, rx_f, color);
        } else {
            // Snap to integer columns. `+1` makes the right edge
            // inclusive so a note of nominal width N renders as
            // exactly N pixels regardless of fractional position.
            let lx = lx_f.round() as i32;
            let rx = rx_f.round() as i32 + 1;
            hline(buf, y, lx, rx, color);
        }
    }
}

/// AA-fill one scanline of a trapezoid: solid interior columns plus
/// alpha-blended left and right edge pixels weighted by fractional
/// coverage. Mirrors classic's `draw_slice_vert` AA path.
fn fill_row_aa(buf: &mut [u8], y: i32, lx_f: f32, rx_f: f32, color: [u8; 4]) {
    if rx_f <= lx_f {
        return;
    }
    let lx_floor = lx_f.floor();
    let rx_floor = rx_f.floor();
    if lx_floor == rx_floor {
        // Sub-pixel row: edges fall in the same column, alpha is the
        // width of the covered region within that pixel.
        let cov = rx_f - lx_f;
        blend_pixel(buf, lx_floor as i32, y, color, cov);
        return;
    }
    // Left edge: pixel `floor(lx_f)` is covered from `frac(lx_f)` to 1.0.
    let left_cov = 1.0 - (lx_f - lx_floor);
    blend_pixel(buf, lx_floor as i32, y, color, left_cov);
    // Right edge: pixel `floor(rx_f)` is covered from 0 to `frac(rx_f)`.
    let right_cov = rx_f - rx_floor;
    blend_pixel(buf, rx_floor as i32, y, color, right_cov);
    // Solid interior between the two edge pixels.
    let interior_start = lx_floor as i32 + 1;
    let interior_end = rx_floor as i32;
    if interior_end > interior_start {
        hline(buf, y, interior_start, interior_end, color);
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn color_to_rgba(c: rusticnes_ui_common::drawing::Color) -> [u8; 4] {
    [c.r(), c.g(), c.b(), 0xFF]
}

