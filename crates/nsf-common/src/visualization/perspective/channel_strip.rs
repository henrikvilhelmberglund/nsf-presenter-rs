//! Per-channel "surfboard" strip at the bottom of the canvas.
//!
//! Replicates the classic view's `draw_channel_surfboard` rendering:
//! gradient background colored from the channel, chip label in the
//! upper-left, channel name in the lower-right, and an antialiased
//! waveform trace with a soft glow halo. Channel order matches what
//! the classic view shows (2A03 first, then any expansion chips).
//!
//! Drawing is delegated to `rusticnes_ui_common::drawing` via a local
//! `SimpleBuffer` — same routines the classic view uses, so the look
//! is pixel-identical. The strip is then copied into our main RGBA
//! buffer at the bottom of the canvas.

use std::sync::OnceLock;

use rusticnes_core::apu::{AudioChannelState, Timbre};
use rusticnes_ui_common::drawing::{self, apply_gradient, Color, Font, SimpleBuffer};
use rusticnes_ui_common::piano_roll_window::PianoRollWindow;

use crate::visualization::perspective::transform::{
    CHANNEL_STRIP_BOTTOM_Y, CHANNEL_STRIP_TOP_Y,
};

// Same bitmap font the classic view uses — vendored from the rusticnes
// asset directory so labels render identically.
const FONT_DATA: &[u8] = include_bytes!(
    "../../../../../external/rusticnes-ui-common/src/assets/8x8_font.png"
);

fn font() -> &'static Font {
    static FONT: OnceLock<Font> = OnceLock::new();
    FONT.get_or_init(|| Font::from_raw(FONT_DATA, 8))
}

pub fn draw_channel_strip(
    dst: &mut [u8],
    piano_roll: &PianoRollWindow,
    channels: &[&dyn AudioChannelState],
    canvas_w: i32,
    canvas_h: i32,
) {
    if channels.is_empty() {
        return;
    }
    let strip_top = CHANNEL_STRIP_TOP_Y as u32;
    let strip_bot = CHANNEL_STRIP_BOTTOM_Y as u32;
    let strip_h = strip_bot - strip_top;
    let w = canvas_w as u32;

    // Local SimpleBuffer to draw into. Cheaper than refactoring the
    // whole renderer onto SimpleBuffer just for this one band — we pay
    // one `(w * strip_h * 4)` alloc + copy per frame, ~7 KB for a
    // 960×30 strip.
    let mut sb = SimpleBuffer::new(w, strip_h);

    let disable_aa = piano_roll.disable_aa;
    let cell_w = w / channels.len() as u32;
    let mut cell_x = 0u32;
    for (i, ch) in channels.iter().enumerate() {
        let cell_width = if i + 1 == channels.len() {
            w - cell_x // last cell takes the leftover pixel(s)
        } else {
            cell_w
        };
        draw_surfboard(&mut sb, *ch, piano_roll, cell_x, 0, cell_width, strip_h, disable_aa);
        cell_x += cell_width;
    }

    blit_to_dst(&sb, dst, canvas_w, canvas_h, strip_top);
}

fn draw_surfboard(
    sb: &mut SimpleBuffer,
    ch: &dyn AudioChannelState,
    piano_roll: &PianoRollWindow,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    disable_aa: bool,
) {
    // Per-channel color from the piano roll's user-configured settings,
    // then blended along the timbre axis (duty cycle / LSFR mode / FDS
    // patch index) — same logic the classic view's `channel_color`
    // uses, so a square-wave Pulse 1 vs a 25%-duty Pulse 1 read with
    // different shades, etc.
    let colors = piano_roll.channel_colors(ch);
    let color = pick_color(&colors, ch);

    draw_surfboard_background(sb, x, y, width, height, color);
    draw_channel_labels(sb, ch, x, y, width, height);
    draw_waveform(sb, ch, x, y, width, height, color, disable_aa);
    draw_channel_dividers(sb, x, y, width, height);
}

fn pick_color(colors: &[Color], ch: &dyn AudioChannelState) -> Color {
    if colors.is_empty() {
        return Color::rgb(192, 192, 192);
    }
    let base = colors[0];
    match ch.timbre() {
        Some(Timbre::DutyIndex { index, max })
        | Some(Timbre::LsfrMode { index, max })
        | Some(Timbre::PatchIndex { index, max }) => {
            // `max + 1` matches rusticnes's normalization — without it,
            // an N163 wavetable picking the last patch index would land
            // exactly on the upper gradient stop instead of cycling
            // through.
            let weight = index as f32 / (max + 1) as f32;
            apply_gradient(colors.to_vec(), weight)
        }
        None => base,
    }
}

fn draw_surfboard_background(
    sb: &mut SimpleBuffer,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    color: Color,
) {
    // Same sin-curve gradient the classic view uses — brighter at the
    // top + bottom edges, dimmest near the middle, all on a heavily
    // dimmed (×0.125) base.
    let bg = scale_color(color, 0.125);
    for row in 0..height {
        let weight = 1.0 - ((row as f32 * std::f32::consts::PI) / (height as f32)).sin();
        let row_color = scale_color(bg, weight);
        drawing::rect(sb, x, y + row, width, 1, row_color);
    }
}

fn draw_channel_labels(
    sb: &mut SimpleBuffer,
    ch: &dyn AudioChannelState,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) {
    let label_color = Color::rgba(0xFF, 0xFF, 0xFF, 0x33);
    // Chip name — upper-left, matching the classic view's offset.
    drawing::text(sb, font(), x + 4, y + 2, &ch.chip(), label_color);
    // Channel name — lower-right, right-aligned within the cell.
    let name = ch.name();
    let name_w = (name.len() as u32) * 8;
    let label_x = x + width.saturating_sub(4 + name_w);
    let label_y = y + height.saturating_sub(2 + 8);
    drawing::text(sb, font(), label_x, label_y, &name, label_color);
}

fn draw_waveform(
    sb: &mut SimpleBuffer,
    ch: &dyn AudioChannelState,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    color: Color,
    disable_aa: bool,
) {
    // 4 samples per pixel — matches the surfboard's `speed = 4`.
    let speed: usize = 4;
    let edge_ring = ch.edge_buffer();
    let window = (width as usize) * speed;
    let first = find_edge(edge_ring.buffer(), edge_ring.index(), window);

    let sample_buffer = ch.sample_buffer().buffer();
    if sample_buffer.is_empty() {
        return;
    }
    let sample_min = ch.min_sample();
    let sample_max = ch.max_sample() + 1;
    let range = (sample_max as i32 - sample_min as i32) as f32;
    if range <= 0.0 {
        return;
    }

    let glow_color = scale_color(color, 0.25);
    let glow_thickness = 2.5_f32;
    let line_thickness = 0.5_f32;

    let mut last_y = ((sample_buffer[first] - sample_min) as f32 * height as f32) / range;
    for i in 0..width {
        let dx = x + i;
        let sample_index = (first + (i as usize) * speed) % sample_buffer.len();
        let sample = sample_buffer[sample_index];
        let current_y = ((sample - sample_min) as f32 * height as f32) / range;
        let (top_edge, bottom_edge) = if last_y < current_y {
            (last_y, current_y)
        } else {
            (current_y, last_y)
        };
        // Glow is only useful with AA — without alpha-blended edges,
        // the "halo" reads as a chunky outline. Classic view skips it
        // when AA is off, so we do too.
        if !disable_aa {
            draw_vaa_line(
                sb,
                dx,
                y as f32 + top_edge - glow_thickness,
                y as f32 + bottom_edge + glow_thickness,
                glow_color,
            );
        }
        // Main trace — always drawn. classic's `draw_vertical_antialiased_line`
        // applies the ±line_thickness in both AA and disable_aa branches,
        // so we do the same here for pixel parity.
        if disable_aa {
            draw_solid_vline(
                sb,
                dx,
                y as f32 + top_edge - line_thickness,
                y as f32 + bottom_edge + line_thickness,
                color,
            );
        } else {
            draw_vaa_line(
                sb,
                dx,
                y as f32 + top_edge - line_thickness,
                y as f32 + bottom_edge + line_thickness,
                color,
            );
        }
        last_y = current_y;
    }
}

/// Crisp (non-AA) vertical line — matches classic's `disable_aa`
/// branch in `draw_vertical_antialiased_line` line-for-line: half-open
/// `top..bottom` iteration, clamped to canvas height, and at least one
/// pixel guaranteed via `bottom.max(top + 1)`.
fn draw_solid_vline(sb: &mut SimpleBuffer, x: u32, top_edge: f32, bottom_edge: f32, color: Color) {
    if bottom_edge < 0.0 || x >= sb.width {
        return;
    }
    let top = top_edge.round().max(0.0) as u32;
    let bottom_clamped = bottom_edge.round().min(sb.height as f32) as u32;
    let bottom = bottom_clamped.max(top + 1);
    for y in top..bottom {
        sb.put_pixel(x, y, color);
    }
}

fn draw_channel_dividers(sb: &mut SimpleBuffer, x: u32, y: u32, width: u32, height: u32) {
    let mut base = Color::rgba(0, 0, 0, 255);
    let divider_width: u32 = 5;
    for dx in 0..divider_width {
        let gradient_index = (255 * (divider_width - dx)) / divider_width;
        let color_weight = (gradient_index * gradient_index) / 255;
        base.set_alpha(color_weight as u8);
        drawing::blend_rect(sb, x + dx, y, 1, height, base);
        if width > dx + 1 {
            drawing::blend_rect(sb, x + width - dx - 1, y, 1, height, base);
        }
    }
}

fn scale_color(c: Color, scale: f32) -> Color {
    Color::rgb(
        (c.r() as f32 * scale) as u8,
        (c.g() as f32 * scale) as u8,
        (c.b() as f32 * scale) as u8,
    )
}

/// Antialiased vertical line — same algorithm the classic view's
/// `draw_vertical_antialiased_line` uses. Top/bottom pixels are
/// alpha-blended to their fractional coverage; the middle is solid.
fn draw_vaa_line(sb: &mut SimpleBuffer, x: u32, top_edge: f32, bottom_edge: f32, color: Color) {
    if bottom_edge < 0.0 {
        return;
    }
    let top_floor = top_edge.floor();
    let bottom_floor = bottom_edge.floor();
    let mut blended = color;
    if top_floor == bottom_floor {
        let alpha = bottom_edge - top_edge;
        blended.set_alpha((alpha * 255.0) as u8);
        if top_floor >= 0.0 && (top_floor as u32) < sb.height {
            sb.blend_pixel(x, top_floor as u32, blended);
        }
        return;
    }
    let top_alpha = 1.0 - (top_edge - top_floor);
    blended.set_alpha((top_alpha * 255.0) as u8);
    if top_floor >= 0.0 && (top_floor as u32) < sb.height {
        sb.blend_pixel(x, top_floor as u32, blended);
    }
    let bottom_alpha = bottom_edge - bottom_floor;
    blended.set_alpha((bottom_alpha * 255.0) as u8);
    if bottom_floor >= 0.0 && (bottom_floor as u32) < sb.height {
        sb.blend_pixel(x, bottom_floor as u32, blended);
    }
    let mid_lo = top_floor as i32 + 1;
    let mid_hi = bottom_floor as i32;
    if mid_lo < mid_hi {
        for yy in mid_lo..mid_hi {
            if yy >= 0 && (yy as u32) < sb.height {
                sb.put_pixel(x, yy as u32, color);
            }
        }
    }
}

/// Find a stable start point in the channel's sample buffer by
/// scanning the edge buffer backward for a recorded zero-crossing.
/// Mirror of rusticnes-ui-common's `find_edge` with safe modular
/// arithmetic. Falls back to `(write_idx - window_size)` when no edge
/// is found (e.g. for DMC, whose edge buffer is mostly empty).
fn find_edge(edges: &[i16], write_idx: usize, window_size: usize) -> usize {
    let len = edges.len();
    if len == 0 {
        return 0;
    }
    let win = window_size.min(len);
    let initial = (write_idx + len - win) % len;
    let mut current = initial;
    let search_limit = (window_size * 4).min(len);
    for _ in 0..search_limit {
        if edges[current] != 0 {
            return (current + len - (win / 2)) % len;
        }
        current = (current + len - 1) % len;
    }
    initial
}

fn blit_to_dst(sb: &SimpleBuffer, dst: &mut [u8], canvas_w: i32, canvas_h: i32, dst_y: u32) {
    let w = sb.width as usize;
    let h = sb.height as usize;
    let canvas_w_u = canvas_w as usize;
    let canvas_h_u = canvas_h as usize;
    for row in 0..h {
        let dst_row = dst_y as usize + row;
        if dst_row >= canvas_h_u {
            break;
        }
        let src_start = row * w * 4;
        let dst_start = dst_row * canvas_w_u * 4;
        let copy_w = w.min(canvas_w_u);
        dst[dst_start..dst_start + copy_w * 4]
            .copy_from_slice(&sb.buffer[src_start..src_start + copy_w * 4]);
    }
}
