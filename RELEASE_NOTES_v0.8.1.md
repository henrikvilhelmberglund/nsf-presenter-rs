# v0.8.1 — perspective view polish

Bugfix + visual polish release on top of v0.8.0. No new toplevel features; same two binaries.

## Perspective view

- **AA support.** The toolbar AA toggle now drives the perspective renderer too. With AA on, note-trapezoid diagonals and grid lines are alpha-blended (Wu-style for lines, fractional-coverage edges for trapezoids); with AA off, both stay crisp 1-pixel paths.
- **Distance fade on the grid.** Lane and octave grid lines fade toward the horizon (1.0 alpha at the keyboard, 0.15 at the vanishing point) so the floor reads as a 3D plane receding into the distance. In non-AA mode the same fade is faked by interpolating the line color toward the canvas background.
- **Note width matches the classic view.** Frequency / Noise note width is now `thickness * lane_width / 2` (same formula as classic's `draw_slice_vert`), scaling linearly across the full 0–6 thickness range. Previously the half-width factor saturated at amplitude≥0.33, flattening every loud note to the same square shape; now the natural attack-decay envelope reads as triangle-shaped notes in the perspective view just like it does in classic.
- **No more gaps near the horizon.** Chunks that collapse to a single scanline (perspective compression near the vanishing point) used to fall through to a non-AA `hline` with the `min/max` of top/bottom edges, producing stair-step jaggies and conflicting overdraw between adjacent chunks. They now use the *average* of top/bottom X edges and call the same AA edge-blender the multi-row path uses.
- **No more gaps under strong vibrato.** Connection between consecutive frequency slices was previously gated on `c.y.round() == n.y.round()`, so a vibrato peak that briefly pushed the rounded pitch across a key boundary (e.g. `59.6` ↔ `60.6`) would skip the trapezoid and leave a hole. Now we connect when the float pitches are within 0.6 semitones — wide enough for real vibrato, tight enough to reject true note jumps.

## Player

- **Close-while-paused no longer hangs.** When the close handler sent `Terminate` to the paused player thread, the inner request-drain loop processed it but then re-entered the blocking `rx.recv()` (because `paused` was still true). No more messages were coming, so the thread blocked forever and `handle.join()` hung the GUI. The drain loop now bails the moment `terminating` is set.

## Downloads

- **`nsf-player-v0.8.1-windows.zip`** — standalone player, no external dependencies
- **`nsf-presenter-v0.8.1-windows.zip`** — video renderer with FFmpeg 7 DLLs bundled
