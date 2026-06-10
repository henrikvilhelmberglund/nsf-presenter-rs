# 3D Perspective View — Planning Document

## Goal

Add a second visualization mode to `nsf-player.exe` that renders the NES audio
as **falling notes in a trapezoidal perspective view**: a piano keyboard sits
at the bottom of the screen, notes appear from a vanishing point at the
horizon and grow as they "fall" toward the keys. Synthesia / ToneScope style,
but with explicit perspective scaling (lanes converge toward the horizon, not
straight parallel lanes).

The existing classic 2D piano-roll view stays as-is. Users toggle between
**Classic** and **Perspective** modes from the player toolbar.

## Feasibility

High. We already capture per-channel pitch + volume time-slices through the
rusticnes `PianoRollWindow::update()` pipeline. The 3D view just needs a
different *renderer* that consumes those same time-slices and writes RGBA
into the player's 960×540 canvas via a perspective transform. No 3D engine,
no GPU shaders — pure CPU rasterization of filled trapezoids.

Estimated effort: **1–2 days** of focused work for a working first cut.

## Out of scope for v1

- True 3D camera controls (orbit, zoom, etc.). The perspective transform is
  fixed.
- Per-channel customization of the perspective constants.
- Anti-aliased trapezoid edges. Notes render as solid filled trapezoids
  (matches the player's current AA-off default look).
- Lighting / shading on the keyboard. Keyboard is drawn flat.

## Architecture

```
nsf-common (lib)
  emulator/
    emulator.rs            # exposes piano_roll_window via & accessor
                           # (new) — read-only handle for renderers

[upstream] external/rusticnes-ui-common/src/piano_roll_window.rs
                           # add pub fn time_slices() -> &VecDeque<...>
                           # add pub fn channel_color(...) accessor

nsf-player (bin)
  src/
    perspective/           # NEW module
      mod.rs               # public entry: render_perspective(canvas, time_slices, channel_settings)
      transform.rs         # depth → screen-space helpers
      keyboard.rs          # static keyboard strip rendering
      notes.rs             # quad/trapezoid rasterization for note slices
    gui.rs                 # toggle: viz Mode = Classic | Perspective
                           # when Perspective: instead of get_piano_roll_frame(),
                           # call render_perspective into our canvas buffer
    slint/
      visualization.slint  # unchanged — still just an Image
      player.slint         # add Mode dropdown next to Scale dropdown
```

### Why a renderer in `nsf-player`, not in `rusticnes-ui-common`?

- Keeps the upstream vendored crate minimal — only one small new pub accessor.
- The perspective view is a *player-only* feature; the video renderer doesn't
  need it.
- Easier iteration without touching code shared by both binaries.

The trade-off: we need to expose the piano-roll's `time_slices` deque from
rusticnes-ui-common via a `pub fn` (currently it's a private field). That's
a one-line change.

## Math

### Coordinate system

Canvas is 960×540 (same as classic view). Top of canvas is y=0, bottom is
y=540.

Three regions, top to bottom:
- **Sky** (optional gradient, depth cue): y = 0 .. 100
- **Note area** (the trapezoid where falling notes are drawn): y = 100 .. 460
- **Keyboard strip**: y = 460 .. 540

### Depth → screen-space

Each note slice has a `depth` value in `[0.0, 1.0]`:
- `depth = 0.0` — at the keyboard (just about to play)
- `depth = 1.0` — at the horizon (far away in time)

A given slice's depth = its index in `time_slices` divided by the number of
slices that fit on screen (e.g. 360 slices over the 360-pixel note area at
1 row per slice).

```rust
fn y_at_depth(depth: f32) -> f32 {
    // Lerp from keyboard top to horizon. Optionally non-linear for a
    // stronger "perspective" feel (depth.powf(0.6) bunches motion near
    // the horizon, depth.powf(1.4) bunches near the keyboard).
    lerp(NOTE_AREA_BOTTOM, NOTE_AREA_TOP, depth)
}

fn scale_at_depth(depth: f32) -> f32 {
    // 1.0 at keyboard, ~0.15 at horizon. Tune for taste.
    lerp(1.0, 0.15, depth)
}
```

### Pitch → x

The keyboard spans `KEY_RANGE` keys (e.g. 60 — 5 octaves). At depth=0 the
full keyboard fills the canvas width; at higher depth the lanes converge
toward the horizon's vanishing point (screen-center).

```rust
fn lane_x(pitch: u8, depth: f32) -> f32 {
    let key_index_normalized = (pitch - LOWEST_KEY) as f32 / KEY_RANGE as f32; // 0..1
    let bottom_x = key_index_normalized * CANVAS_W;
    let horizon_center = CANVAS_W * 0.5;
    let scale = scale_at_depth(depth);
    horizon_center + (bottom_x - horizon_center) * scale
}
```

So at depth 0, `lane_x` = `bottom_x` (full spread). At depth 1, all lanes
collapse to `horizon_center` (the vanishing point).

### Note width

Each note has a per-pitch lane. Width at depth `d`:
```rust
let lane_width_at_d = (CANVAS_W / KEY_RANGE as f32) * scale_at_depth(d);
let note_width_at_d = lane_width_at_d * (slice.thickness / max_thickness);
```

Volume (thickness) scales the note within its lane.

### Drawing a note as a trapezoid

A note's "vertical extent" is determined by how many consecutive
`time_slices` the note appears in. For each *pair* of consecutive slices
`(i, i+1)` where the same pitch is present in both:

- Top edge: `y_at_depth(depth_of(i+1))`, x_center: `lane_x(pitch, depth_of(i+1))`, width: `note_width_at_d(i+1)`
- Bottom edge: `y_at_depth(depth_of(i))`, x_center: `lane_x(pitch, depth_of(i))`, width: `note_width_at_d(i)`

These 4 corners define a filled trapezoid. Scanline-rasterize between
`y_top` and `y_bottom`, interpolating left/right x at each row.

For *single*-slice notes (a one-frame attack), draw a thin rectangle at
that depth — basically the bottom-only of the trapezoid.

### Keyboard rendering

Existing rusticnes piano-key drawing code (`draw_left_white_key_horiz`,
`draw_black_key_horiz`, etc. in `piano_roll_window.rs`) already produces a
keyboard strip. We can reuse it for the bottom 80px or just write a small
new keyboard renderer specific to this view. Probably cleaner to write our
own — the existing one assumes a particular orientation and key spacing.

The keyboard also gets highlighted per-key when a note is being played at
depth=0 (the moment of "attack").

## File layout

```
external/rusticnes-ui-common/src/piano_roll_window.rs
  + pub fn time_slices(&self) -> &VecDeque<Vec<ChannelSlice>>
  + pub fn channel_color(&self, channel: &dyn AudioChannelState) -> Color  # if not already pub

crates/nsf-common/src/emulator/emulator.rs
  + pub fn piano_roll_window(&self) -> &PianoRollWindow  # for renderer access

crates/nsf-player/src/perspective/mod.rs              # NEW
  pub struct PerspectiveRenderer {
      buffer: Vec<u8>,
  }
  impl PerspectiveRenderer {
      pub fn new() -> Self
      pub fn render(&mut self, piano_roll: &PianoRollWindow) -> &[u8]
  }

crates/nsf-player/src/perspective/transform.rs        # NEW
  fn y_at_depth, scale_at_depth, lane_x, etc.

crates/nsf-player/src/perspective/notes.rs            # NEW
  fn rasterize_trapezoid(canvas: &mut [u8], corners, color)

crates/nsf-player/src/perspective/keyboard.rs         # NEW
  fn draw_keyboard(canvas: &mut [u8], active_pitches: &[u8])

crates/nsf-player/src/gui.rs
  - Add ViewMode enum (Classic | Perspective)
  - In the frame-publish closure: pick between
      `emulator.get_piano_roll_frame()` and
      `perspective_renderer.render(&emulator.piano_roll_window())`
  - Add a "view-mode" dropdown to the toolbar

crates/nsf-player/src/slint/player.slint
  + ComboBox: "View: Classic / Perspective"
```

## Implementation phases

Each step is self-contained and produces a visible improvement.

1. **Expose accessors** in rusticnes-ui-common and `nsf_common::emulator`.
   `cargo check` is the only acceptance.
2. **Static keyboard strip.** Render just the keyboard at the bottom, all
   keys un-highlighted. View toggle wires up; perspective mode shows the
   keyboard + black background. Acceptance: keyboard appears correctly.
3. **Vanishing-point grid lines.** Draw the lane dividers as straight lines
   from each key's center at the bottom to the vanishing point at the
   horizon. Visualizes the perspective geometry. Acceptance: the trapezoid
   "highway" is visible.
4. **Render single-slice rectangles.** For each slice in `time_slices`,
   draw a small note-rectangle at its depth, x-position, and color. No
   inter-slice connection yet. Acceptance: notes appear and "scroll"
   correctly (slides spawn at horizon and fall toward keyboard).
5. **Connect consecutive slices into trapezoids.** Walk per-channel,
   per-pitch through the deque; group consecutive frames of the same pitch
   into a single trapezoid. Acceptance: held notes appear as continuous
   bars stretching from horizon to keyboard.
6. **Active-key highlighting.** Lit keys at the bottom for currently-played
   pitches. Tiny embellishment; brings the visual to life.
7. **Polish.** Tune perspective constants, sky gradient, line spacing,
   colors. Adjust based on visual feedback.

## Risk register

| risk | severity | mitigation |
|---|---|---|
| Trapezoid rasterizer too slow | medium | Profile at step 5; if 60 Hz is tight, switch to half-precision math, skip occluded rows, or render at lower-res then upscale. |
| Piano keyboard with 88 keys too cramped at 960 px wide | medium | Limit visible range (e.g. C2 — C7, 5 octaves). Pitches outside range are clamped to nearest edge. User won't notice for NES NSFs which rarely exceed this range. |
| Pitch jitter (vibrato) makes lanes wobble | low | Same as classic view — the user has already accepted this trade-off. The 3D view inherits it. |
| `time_slices` deque length tuning | low | Match the height of the note area in pixels (e.g. 360). |
| Adding a mode-dropdown clutters the toolbar | low | Could go in the "Scale:" group as a related setting. |

## Open questions

1. **Keyboard pitch range.** Default to C2–C7 (60 keys), or full 88, or
   dynamic to fit the loaded NSF's notes? Recommendation: C2–C7 fixed for
   simplicity; revisit if NES tracks go significantly outside that range.
2. **Sky gradient / depth haze.** Worth the visual but adds a tiny bit of
   per-frame work. Recommendation: yes, simple vertical lerp from a dark
   horizon color to canvas background.
3. **Single rendering buffer or split?** Could let the perspective renderer
   share the same 960×540 canvas the classic view uses. Recommendation:
   yes, share — saves an allocation.

## Effort estimate

Roughly aligned with the original `PLAYER_MODE_PLAN.md` estimate format:

- Steps 1–3 (accessors + keyboard + grid): a long afternoon.
- Step 4 (per-slice notes): half a day.
- Step 5 (trapezoid connection): half a day.
- Steps 6–7 (polish): half a day.

Total: ~1.5–2 days of focused work for a clean v1.
