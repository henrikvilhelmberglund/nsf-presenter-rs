# v0.8.0 — perspective view, persistence, real auto-advance

Two big additions over v0.7.1 plus a pile of polish.

## Perspective view (new)

A 3D-style alternative to the classic 2D piano roll. Switch via the **View** dropdown in the toolbar.

- Falling notes converge to a horizon at the top of the screen — same lane geometry as the classic view, just mirrored above and below the horizon so each pitch's lane is a straight diagonal from keyboard to vanishing point.
- **Real piano keyboard** at the bottom: white keys touch with proper black-key overlay, per-white-key vertical seams (full-height on touching E↔F / B↔C, lower-half-only where a black key sits between).
- **Active-key highlighting** colors each currently-sounding key with its channel's color (matches the classic view's piano-strings convention).
- **Noise channel** falls as 16 LFSR-string lanes, sharing X positions with the floor's leftmost 16 piano-key lanes (sky-mirror of the floor geometry — perspective convergence at the horizon).
- **Waveform (DPCM)** channel falls as a single wider lane on the right; width scales linearly with `slice.thickness` so loud drum hits show their natural attack-decay envelope instead of flat-topping.
- **Channel strip** below the keyboard: a faithful port of the classic view's `draw_channel_surfboard`. Per-channel gradient background, antialiased waveform + glow halo, chip name + channel name labels, channel dividers — sourced from the same `PianoRollWindow::channel_colors` so user palette settings carry over. Respects the AA toggle.
- Sub-frame stepping (4 sub-frames per NES frame) for smooth motion of falling notes regardless of view mode.

## Persistence

`config.toml` is written next to the executable on every state change and re-read on launch. Saved fields:

- Playlist contents (full `PlaylistItem` metadata — no NSF re-scan on load)
- View mode (Classic / Perspective)
- Scale mode (Scaled / 1x / 2x)
- Anti-aliasing toggle
- Volume
- Repeat (now playlist-level — see below)

Failure to load or save is non-fatal: a missing or corrupted config falls back to hardcoded defaults; write errors log to stderr.

## Auto-advance

The "Repeat" checkbox was previously wired to `SetRepeatTrack`, which **disabled** FT loop detection — so with Repeat checked, the current track looped forever and the playlist never advanced. Reworked:

- "Repeat" is now playlist-level: when the last track ends, with Repeat checked the playlist wraps to track 0; without it, playback stops. Mid-playlist tracks auto-advance regardless.
- Track-end signals are stacked so songs without metadata still advance:
    1. **FT Cxx** (driver-explicit stop) — unchanged.
    2. **NSFe / M3U duration** metadata — unchanged.
    3. **FT loop detection** — always armed now (was previously gated behind the Repeat toggle).
    4. **FT song-position stall** (new) — for FT NSFs that don't loop and don't have Cxx, the driver's song-data pointer keeps advancing through rests but stops moving when the song genuinely ends. ~2 s of no movement → end.
    5. **Silence fallback** (new) — for non-FT NSFs where no position pointer is available, falls back to ~5 s of all-channels-silent. Long enough that musical pauses don't false-trigger.

Both new detectors only arm after the song has produced at least one note, so silent intros don't end the track prematurely.

## Downloads

- **`nsf-player-v0.8.0-windows.zip`** — standalone player, no external dependencies
- **`nsf-presenter-v0.8.0-windows.zip`** — video renderer with FFmpeg 7 DLLs bundled
