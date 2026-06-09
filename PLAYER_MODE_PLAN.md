# Player Mode — Planning Document

## Goal

Add a real-time NSF **player mode** alongside the existing offline **video-render mode**, with a playlist that can hold (a) the subsongs inside a loaded NSF/NSFe and (b) tracks from multiple NSF/NSFe files added via folder pick or drag-and-drop. The existing render pipeline stays untouched and is still the only mode that produces a video file.

## Feasibility

High. The architectural split that matters is already done:

- `Renderer::step()` at [src/renderer/mod.rs:79](src/renderer/mod.rs#L79) already separates *emulator advance* from *sink push*. The sink is `VideoBuilder` (FFmpeg). Replacing it with `cpal` + Slint `Image` is mechanical.
- The emulator exposes exactly the surface a player needs: `step()`, `get_piano_roll_frame() -> Vec<u8>` (RGBA, dimensions set by `set_piano_roll_size`), and `get_audio_samples(count, volume_divisor) -> Option<Vec<i16>>` ([src/emulator/emulator.rs:216, 263, 289](src/emulator/emulator.rs#L216)).
- The GUI already runs rendering on a dedicated thread with mpsc messaging ([src/gui/render_thread.rs:50](src/gui/render_thread.rs#L50)) — the player thread will use the same pattern with different message types.
- M3U parsing and per-track title/duration discovery already exist ([src/emulator/m3u_searcher.rs:30](src/emulator/m3u_searcher.rs#L30)) and are reused in the renderer GUI ([src/gui/mod.rs:52](src/gui/mod.rs#L52)). That logic is directly reusable for the playlist.

The genuinely hard part is timing/threading. Notes on that under "Audio-clocked playback" below.

## Decisions locked in

- **Entry**: existing render window grows an **"Open player…"** button; clicking it opens a separate Slint window. Both windows can coexist; closing one does not close the other.
- **Playlist sources**: subsongs of the loaded module, **plus** multi-file (folder pick or drag-drop of .nsf/.nsfe files). M3U-as-playlist-file is **out of scope** for v1.
- **Render mode is unchanged.** No refactor of `Renderer` or `VideoBuilder` is required to make the player work — the player gets its own pipeline that shares only the `Emulator` and the channel-settings/config plumbing.

## Out of scope for v1

- Gapless playback **across files** (each track change reloads the emulator → a brief gap is expected). Within a single NSF, switching subsongs is cheap and *can* be near-gapless if we just call `select_track` without re-init; whether it actually sounds clean depends on driver behavior — treat as "nice if it works, don't promise."
- Seek within a track.
- Saving/loading .m3u playlist files. (Reading them as a playlist source can be added later; the parser already exists.)
- Equalizer, channel solo/mute live tweaks. (The render GUI already supports channel hide/colors; we can expose the same controls in the player later — not blocking v1.)

## New dependencies

| crate | purpose | notes |
|---|---|---|
| `cpal` | cross-platform audio output | standard choice; works on Windows (WASAPI). |
| `ringbuf` | lock-free SPSC ring buffer | emulator thread → cpal callback. Must be lock-free because the cpal callback runs on a real-time audio thread where allocations and locks are forbidden. |

`rodio` is a higher-level wrapper around cpal and is tempting, but it owns its own mixer/scheduling and doesn't fit the audio-clocked model below. Stick with raw cpal.

## File layout

New module tree, additive — nothing in `src/renderer/` or `src/video_builder/` changes:

```
src/
  player/
    mod.rs              # PlayerEngine: wraps Emulator + ring buffer + clock
    audio.rs            # cpal stream setup, sample-rate negotiation, mono→device-channels expansion
    playlist.rs         # PlaylistItem, Playlist, add-from-file/folder, current-index, advance/prev
    player_thread.rs    # mirror of gui::render_thread for player control messages
  gui/
    player_window.rs    # Slint window wiring (analogue of gui/mod.rs but smaller)
    slint/
      player.slint      # new top-level window: playlist pane, viz pane, transport bar
```

`main.rs` is unchanged. The player window is opened from the existing render window via a new `Open player…` button (Slint callback → spawns the player window and a player thread).

## Data flow

```
            ┌─────────────────────────────────────────────────┐
            │ GUI thread (Slint event loop)                   │
            │ - player_window.slint                           │
            │ - holds latest RGBA frame in an arc-swap        │
            │   and blits it into a Slint Image each tick     │
            └────────────────────┬────────────────────────────┘
              latest-frame swap  │       control messages
                                 │       (Play/Pause/Next/Prev/Load…)
                                 ▼
            ┌─────────────────────────────────────────────────┐
            │ Player thread (owns Emulator)                   │
            │ - loop: step() → push samples into ring buffer  │
            │         → publish latest RGBA frame             │
            │ - blocks on ring-buffer back-pressure           │
            │   (this is the clock: audio consumption paces   │
            │    emulation)                                   │
            └────────────────────┬────────────────────────────┘
                 i16 samples     │
                                 ▼
            ┌─────────────────────────────────────────────────┐
            │ cpal audio callback (real-time thread)          │
            │ - drains ring buffer into device buffer         │
            │ - on underrun: zero-fill + log (do not block)   │
            │ - NEVER allocates, NEVER locks                  │
            └─────────────────────────────────────────────────┘
```

Three threads, two queues:
1. **Ring buffer** (SPSC, `ringbuf`): player thread → audio callback. Holds i16 samples. Capacity sized to ~200 ms of audio at the device sample rate.
2. **Latest-frame slot** (single-producer, single-consumer, overwriting): player thread → GUI. An `Arc<ArcSwap<Vec<u8>>>` is enough; the GUI just reads "whatever is current" at its own redraw rate. No queueing — we don't care about dropping frames.
3. **Control channel** (mpsc): GUI → player thread. `PlayerRequest::{Play, Pause, NextTrack, PrevTrack, Seek(index), LoadFile(path), AppendFiles(Vec<path>), ApplyChannelSettings(…), Terminate}`.

## Audio-clocked playback (the actual hard part)

The current render loop runs flat-out. The player must instead run at NES NTSC rate (60.0988 Hz, [src/emulator/mod.rs:11](src/emulator/mod.rs#L11)) **on average**, with the audio device's consumption as the pacing signal.

Approach:

1. Pick the device sample rate at stream-open time. Ask cpal for the device's default config. Configure the emulator with that exact rate via `Emulator::config_audio(rate, …)` so we don't need a resampler. The NES APU is fine being asked for arbitrary sample rates — the renderer already does this.
2. The cpal callback pulls N samples per call. If the buffer is under-supplied, fill with silence and increment an underrun counter. **Do not block.** Underruns are a tuning signal, not a fatal error.
3. The player thread loops:
   - If paused → park on a `Condvar` until unpaused.
   - Call `emulator.step()` (advances exactly one NES frame).
   - Pull samples via `get_audio_samples(samples_per_frame, 1)` where `samples_per_frame = sample_rate / 60.0988` (round, accumulate fractional remainder across frames so we don't drift).
   - **Push samples into the ring buffer, blocking when the buffer is full.** This back-pressure is the clock — the audio callback's drain rate dictates the emulator's pace.
   - Pull `get_piano_roll_frame()` and `arc_swap.store(Arc::new(frame))`.
4. End-of-track detection: reuse the existing logic from `Renderer::next_fadeout_timer` ([src/renderer/mod.rs:145](src/renderer/mod.rs#L145)) — loop-count threshold, NSFe duration, or "explicit Cxx end". When the active track ends:
   - If a fadeout is desired, run a short volume-divisor ramp (same mechanism as `volume_divisor` in `get_audio_samples`).
   - Send a `TrackEnded` message to the GUI; advance the playlist; load the next track.

### Track switching

- **Same file, different subsong**: `emulator.select_track(idx)` + `emulator.clear_sample_buffer()`. Drain (or keep playing) the ring buffer until the audio callback has consumed prior samples — the simplest correct version drops the ring buffer's residual on track change to avoid mixing old/new audio.
- **Different file**: full re-load via `Emulator::open(path)` + `init()` + `select_track`. There will be a perceptible gap. Acceptable for v1.

### Pixel format / frame plumbing

`get_piano_roll_frame()` returns RGBA bytes, dimensions = canvas size set by `set_piano_roll_size`. For the player, pick a sensible fixed canvas size (e.g. 960×540 or whatever fits the player window). Slint's `Image::from_rgba8` takes a `SharedPixelBuffer<Rgba8Pixel>`. Convert by copying the `Vec<u8>` into a `SharedPixelBuffer` once per displayed frame (this copy happens on the GUI thread, not the audio thread). No format conversion needed.

## Slint UI sketch (player.slint)

```
┌──────────────────────────────────────────────────────────┐
│ [ Add files… ] [ Add folder… ] [ Clear ]                 │
├──────────────┬───────────────────────────────────────────┤
│ Playlist     │                                           │
│ ─────────    │                                           │
│ ▶ Track 01   │           Piano-roll visualization        │
│   Track 02   │           (Image bound to latest_frame)   │
│   Track 03   │                                           │
│   foo.nsf #1 │                                           │
│   foo.nsf #2 │                                           │
│   …          │                                           │
├──────────────┴───────────────────────────────────────────┤
│ Now playing: "Stage 1" — Capcom — © 1987                 │
│ [⏮]  [▶/⏸]  [⏭]    00:42 / 02:15    [▁▁▃▅▇▅▃▁ vu meter]  │
└──────────────────────────────────────────────────────────┘
```

Playlist row model in Slint: `{ display_name: string, file_path: string, track_index: int, is_current: bool, duration_seconds: int }`. Double-click → send `PlayerRequest::Seek(row_index)`.

Drag-and-drop: Slint supports drop handling at the window level. Filter to `.nsf`/`.nsfe` extensions; append to playlist via `PlayerRequest::AppendFiles`.

## Reused code (no changes needed)

- `emulator::Emulator` — used as-is.
- `emulator::m3u_searcher::search` — used to populate per-track titles when adding a file.
- `emulator::Nsf` metadata extraction — used as-is for display strings.
- `renderer::options::ChannelSettings` plumbing — eventually exposed in the player too, but not blocking v1.

## What we explicitly *don't* touch

- `src/video_builder/**` — untouched.
- `src/renderer/**` — untouched. (The web answer suggested "throw away video_builder/" — we are not doing that. We're adding a parallel pipeline.)
- `src/cli.rs` — untouched.
- `src/main.rs` — untouched (player is launched from the GUI, not from main).

## Step-by-step implementation order

Each step compiles and is independently testable. Mark a step done when its acceptance check passes.

1. **Skeleton + dependency wiring.** Add `cpal` and `ringbuf` to Cargo.toml. Create empty `src/player/mod.rs`, `audio.rs`, `playlist.rs`, `player_thread.rs`. Acceptance: `cargo build` passes.

2. **Audio-only smoke test.** Hard-code the existing test NSF path, spin up cpal, build the ring-buffer + emulator-thread loop, and play one track to the end with no UI. Print underrun counts. Acceptance: NSF plays through speakers with no audible glitches on a normal-load machine.

3. **Frame publication.** Add the arc-swap latest-frame slot. In the smoke test, dump the latest frame to a PNG every second and confirm pixels look right. Acceptance: dumped PNGs match what the renderer would produce.

4. **Player window shell.** Create `player.slint` with just a placeholder Image (no playlist yet, no transport). Wire the arc-swap → Slint Image refresh on a 60 Hz Slint timer. Open it from a new "Open player…" button in the render window. Acceptance: opening the player window shows live visualization of the hard-coded NSF.

5. **Transport controls.** Add Play/Pause/Next/Prev buttons in slint, wire to `PlayerRequest` messages. Implement Pause as `Condvar::wait` in the player thread. Acceptance: pause halts both audio and visualization; resume continues cleanly.

6. **Playlist (single file).** Add the playlist Slint component, populate from the loaded NSF's subsongs using the same logic as [src/gui/mod.rs:93](src/gui/mod.rs#L93) (NSFe titles → M3U titles → "Track N" fallback). Wire double-click → `Seek(index)`. Auto-advance on track-end. Acceptance: clicking through subsongs plays each one; reaching end of track auto-advances; pressing Next/Prev skips correctly.

7. **Multi-file: file picker.** "Add files…" opens a native file dialog (existing pattern in [src/gui/mod.rs:150](src/gui/mod.rs#L150)) with multi-select; each file's subsongs are appended as playlist rows. Track switching across files triggers full emulator re-load. Acceptance: a playlist of 3 different .nsf files plays through end-to-end with auto-advance.

8. **Multi-file: folder picker + drag-drop.** "Add folder…" recursively scans for .nsf/.nsfe. Slint drop handler accepts dropped files/folders the same way. Acceptance: dragging a folder onto the player window populates the playlist.

9. **Polish.** Underrun mitigation tuning (ring buffer size, samples-per-frame fractional accumulator), now-playing display, current-track highlighting, basic error dialogs (unreadable file, etc.).

## Risk register

| risk | severity | mitigation |
|---|---|---|
| Audio glitches under load | medium | Audio-clocked design makes the emulator thread the only one that can starve. Increase ring buffer size if needed. Worst case: a single underrun is silence, not a crash. |
| Slint's `Image` doesn't accept zero-copy RGBA buffers | low | We already pay the copy cost in the current renderer (`piano_roll_window.active_canvas().buffer.clone()`). Same cost is fine here. |
| `Emulator::open` / `init` is slow enough to be perceptible between tracks | low/medium | If noticeable, preload the next track's emulator on a separate thread while the current one is still playing. v2 if needed. |
| cpal device sample rate isn't what the APU likes | low | The APU accepts arbitrary rates. We only have to make `samples_per_frame` correct for whatever rate cpal gives us. |
| Two separate Slint windows in one process | low | Slint supports multiple windows on the same event loop. The render window and player window share the same `slint::run_event_loop()` — no extra thread needed for the UI. |

## Open questions to confirm before coding

1. **Default sample rate**: keep 44.1 kHz as today, or follow whatever the audio device's default config reports? Recommendation: follow the device. (Cheaper than resampling, and the APU handles it.)
2. **What to do at end of the entire playlist**: stop, or loop back to track 1? Recommendation: stop, with a checkbox in the transport bar for "Repeat playlist."
3. **Volume control in the player window**: needed for v1? Recommendation: yes — a master volume slider that scales `volume_divisor` (already plumbed for fadeout, [src/emulator/emulator.rs:301](src/emulator/emulator.rs#L301)). One slider, almost free.

## Effort estimate

Mirrors the web answer's estimate. A rough prototype landing through step 4 is about a weekend. Steps 5–9 plus polish bring it to ~1–2 weeks of evenings, mostly in audio-clock tuning, gapless-ish track switching, and Slint UI fiddling.
