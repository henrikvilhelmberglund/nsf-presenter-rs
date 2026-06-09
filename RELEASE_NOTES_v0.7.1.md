# v0.7.1 — polish

Bugfixes and visual polish over v0.7.0. No new major features; same two binaries.

## Player

- **Pause / resume no longer deadlocks.** The producer thread used to block in `Condvar::wait` from inside the work loop, so the Resume message never reached the request handler. Replaced with a `paused: bool` flag and switched the request queue between `try_recv` (playing) and `recv` (paused / no track).
- **No audio noise on resume.** The cpal callback no longer drains the ring buffer while `audio_expected = false`, so pre-pause samples stay queued and continue cleanly when playback resumes.
- **Repeat behavior reworked.** The "Repeat" checkbox now means "loop the current track indefinitely" rather than "loop the playlist". Default behavior is now: each track plays through one detected FT loop, then auto-advances to the next playlist item. Non-FT NSFs (no loop detection) still play indefinitely — use Next to advance manually.
- **Snap-to-click volume slider.** The std-widgets `Slider` does relative drag from the current value; replaced with a custom Rectangle + TouchArea slider that translates `mouse-x` directly to volume on click and drag.
- **2x scale is now the default.** Visualization window opens at 1920×1080 with exact 2× nearest-neighbor scaling for pixel-perfect output. Toolbar dropdown still has Scaled / 1x / 2x.

## Rendering (`piano-roll-window`)

- **AA-off note width matches AA-on width.** Edge rounding now uses `round + half-open range` so a note of nominal thickness T renders as exactly T pixels regardless of fractional position, matching the fully-opaque core of the AA version.
- **Surfboard waveform respects AA toggle.** When AA is off, the channel-waveform glow halo is no longer drawn — only the main 1-pixel line.
- **Surfboard line snapping fixed.** The vertical-antialiased-line draw uses the same `round + half-open` scheme as the slice/outline paths.

## Workspace

- VS Code task setup polished: `Build nsf-player (release)` panel auto-closes on success; `Run nsf-player` launches detached via PowerShell so re-runs work cleanly.

## Downloads

- **`nsf-player-v0.7.1-windows.zip`** — standalone player, no external dependencies
- **`nsf-presenter-v0.7.1-windows.zip`** — video renderer with FFmpeg 7 DLLs bundled
