# v0.7.0 — NSFPlayer

This release introduces a standalone real-time player (`nsf-player.exe`) alongside the original video renderer (`nsf-presenter.exe`). Same emulator, same piano-roll visualization — different output target.

## What's new

### `nsf-player.exe` — standalone real-time player
- Foobar2000-style transport: play / pause / prev / next, master volume, repeat-playlist
- Playlist supports individual NSF/NSFe files and recursive folder scans; each file's subsongs expand into per-track rows with NSFe / M3U titles
- Separate visualization window with **Scaled / 1x / 2x** modes (1x and 2x snap the window to exact 960×540 / 1920×1080 sizing)
- **AA toggle** — defaults to crisp pixel-art note rendering; flip on for the anti-aliased look the renderer uses
- Sub-frame visualization stepping (~240 Hz) for smooth scroll on high-refresh displays, without distorting NES audio timing
- No FFmpeg dependency — runs standalone

### Workspace restructure
Source is now organized as a cargo workspace:
- `nsf-common/` — shared emulator wrapper, audio engine, playlist
- `nsf-presenter/` — video renderer + GUI + CLI (links FFmpeg)
- `nsf-player/` — standalone player (no FFmpeg dep)

Faster incremental builds when working on just the player.

### Other
- Filter graph added to the renderer to skip the initialization pop at video start
- RusticNES customizations: `disable_aa` flag for pixel-perfect edges, `update_counter` for per-update detection, scanline-level stepping helpers
- Windows multimedia timer resolution bumped to 1 ms at startup so audio pacing is jitter-free

## Downloads

- **`nsf-player-v0.7.0-windows.zip`** (~3 MB) — standalone player, no external dependencies
- **`nsf-presenter-v0.7.0-windows.zip`** (~65 MB) — video renderer with FFmpeg 7 DLLs bundled

Unzip and run the `.exe`. The player binary has no console window; the renderer keeps the console for CLI use.

## Building from source

```
cargo build --release -p nsf-player       # just the player (fast)
cargo build --release -p nsf-presenter    # just the renderer (needs FFmpeg)
cargo build --release                     # both
```
