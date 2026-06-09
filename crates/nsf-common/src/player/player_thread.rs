use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use ringbuf::traits::{Observer, Producer};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::emulator::Emulator;
use crate::player::audio::{AudioFeed, AudioSink};
use crate::player::playlist::PlaylistItem;

/// Width/height of the piano-roll canvas the player renders into.
pub const PLAYER_CANVAS_W: u32 = 960;
pub const PLAYER_CANVAS_H: u32 = 540;

/// RGBA frame buffer shared with the GUI via ArcSwap (no locks on the read side).
pub type LatestFrame = ArcSwap<Vec<u8>>;

#[derive(Clone)]
pub enum PlayerRequest {
    /// Play a specific item; replaces the current track.
    PlayItem(PlaylistItem),
    Pause,
    Resume,
    /// User-driven next: advance the playlist and play the next item.
    NextTrack,
    PreviousTrack,
    /// Set master volume in 0..=255 range. 255 = full, 0 = silent.
    SetVolume(u8),
    /// Replace the playlist entirely.
    SetPlaylist(Vec<PlaylistItem>),
    /// Enable or disable anti-aliased note rendering. Persists across
    /// track loads; default is `false` (crisp pixel edges).
    SetAntiAliasing(bool),
    /// When true, the current track loops forever (TrackEnded never
    /// fires from loop detection). When false (default), the track ends
    /// after one detected loop and the GUI advances the playlist.
    SetRepeatTrack(bool),
    Terminate,
}

#[derive(Clone, Debug)]
pub enum PlayerEvent {
    /// Emitted after a new track actually starts playing.
    TrackStarted {
        index_hint: Option<usize>,
        item: PlaylistItem,
    },
    /// Emitted when the current track has ended naturally (loop/duration).
    TrackEnded,
    PlaybackPaused,
    PlaybackResumed,
    Error(String),
}


pub struct PlayerHandle {
    pub tx: mpsc::Sender<PlayerRequest>,
    pub latest_frame: Arc<LatestFrame>,
    pub underruns: Arc<AtomicU64>,
    /// NES frame index of the most recently produced frame. The GUI polls
    /// this to update the seek bar and elapsed-time display.
    pub current_frame: Arc<AtomicU64>,
    // `AudioSink` owns the cpal::Stream, which is `!Send` on Windows/macOS.
    // It must live on the thread that created it (the GUI thread). Keeping
    // it inside the handle ties the stream's lifetime to the handle's.
    _sink: AudioSink,
    handle: Option<thread::JoinHandle<()>>,
}

impl PlayerHandle {
    pub fn join(mut self) {
        let _ = self.tx.send(PlayerRequest::Terminate);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Spawn the player thread.
///
/// `latest_frame` is the arc-swap into which the player thread publishes
/// each newly-emulated RGBA frame. Callers keep a clone so they can read
/// the current frame at any time — typically inside `on_new_frame`'s GUI
/// closure.
///
/// `on_new_frame` is invoked on the player thread immediately after each
/// frame is published. It runs on the audio-clock-paced producer thread, so
/// it must not block. The typical implementation uses
/// `slint::invoke_from_event_loop` (via `Weak::upgrade_in_event_loop`) to
/// schedule a UI update on the main thread.
pub fn spawn<F, G>(
    latest_frame: Arc<LatestFrame>,
    event_cb: F,
    on_new_frame: G,
) -> Result<PlayerHandle>
where
    F: Fn(PlayerEvent) + Send + 'static,
    G: Fn() + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<PlayerRequest>();
    let latest_frame_thread = latest_frame.clone();

    let (sink, feed) =
        AudioSink::open().context("Failed to open audio output device")?;
    let underruns = feed.underruns.clone();
    let current_frame = Arc::new(AtomicU64::new(0));
    let current_frame_thread = current_frame.clone();

    let handle = thread::Builder::new()
        .name("nsf-player".into())
        .spawn(move || {
            run(
                rx,
                feed,
                latest_frame_thread,
                current_frame_thread,
                event_cb,
                on_new_frame,
            );
        })?;

    Ok(PlayerHandle {
        tx,
        latest_frame,
        underruns,
        current_frame,
        _sink: sink,
        handle: Some(handle),
    })
}

pub fn blank_frame() -> Vec<u8> {
    vec![0; (PLAYER_CANVAS_W * PLAYER_CANVAS_H * 4) as usize]
}

struct PlaybackState {
    emulator: Emulator,
    item: PlaylistItem,
    loaded_file: PathBuf,
}

fn run<F, G>(
    rx: mpsc::Receiver<PlayerRequest>,
    mut feed: AudioFeed,
    latest_frame: Arc<LatestFrame>,
    current_frame: Arc<AtomicU64>,
    event_cb: F,
    on_new_frame: G,
) where
    F: Fn(PlayerEvent) + Send + 'static,
    G: Fn() + Send + 'static,
{
    let mut playlist: Vec<PlaylistItem> = Vec::new();
    let mut current: Option<PlaybackState> = None;
    let mut paused: bool = false;
    let mut volume: u8 = 255;
    // Default: AA off (crisp pixel edges). Persists across track changes.
    let mut anti_aliasing: bool = false;
    // Default: track ends after one detected loop (the GUI then advances
    // the playlist). When true, the track loops indefinitely.
    let mut repeat_track: bool = false;
    let mut terminating = false;

    // Sub-frame stepping: produce one visual sub-frame per SUBFRAME_PERIOD
    // by splitting each NES frame into N evenly-sized scanline batches
    // (default 4 → ~240 Hz). The fixed-batch approach has slightly uneven
    // row distribution (0/6/6/12 per sub-frame because APU quarter-frame
    // events don't align perfectly with the batch boundaries), but in
    // practice it ends up smoother-looking than an event-driven approach.
    const SUB_FRAMES_PER_FRAME: u32 = 4;
    let scanlines_per_subframe =
        Emulator::NES_NTSC_SCANLINES_PER_FRAME / SUB_FRAMES_PER_FRAME;
    let scanlines_last_subframe = Emulator::NES_NTSC_SCANLINES_PER_FRAME
        - scanlines_per_subframe * (SUB_FRAMES_PER_FRAME - 1);
    let subframe_period = Duration::from_secs_f64(
        1.0 / (crate::emulator::NES_NTSC_FRAMERATE * SUB_FRAMES_PER_FRAME as f64),
    );
    let mut next_subframe_target: Option<Instant> = None;

    while !terminating {
        // Drain any pending requests before doing work. If we have nothing
        // to do (no current track OR paused), block on rx.recv() so we
        // don't spin-wait. The blocking recv also resolves the
        // pause-deadlock: a Resume message naturally wakes us.
        loop {
            let active = current.is_some() && !paused;
            let msg = if active {
                match rx.try_recv() {
                    Ok(m) => Some(m),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        terminating = true;
                        None
                    }
                }
            } else {
                match rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => {
                        terminating = true;
                        None
                    }
                }
            };

            let Some(msg) = msg else { break };

            match msg {
                PlayerRequest::PlayItem(item) => {
                    match load_track(&item, feed.sample_rate) {
                        Ok(mut state) => {
                            state.emulator.set_disable_aa(!anti_aliasing);
                            // Pre-fill the ring buffer so the audio callback never
                            // starts starved. ~150 ms of audio is plenty.
                            prefill(&mut state.emulator, &mut feed, volume, 150);
                            current = Some(state);
                            next_subframe_target = Some(Instant::now());
                            feed.audio_expected.store(true, Ordering::Relaxed);
                            event_cb(PlayerEvent::TrackStarted {
                                index_hint: None,
                                item,
                            });
                        }
                        Err(e) => {
                            event_cb(PlayerEvent::Error(format!("Failed to load track: {}", e)));
                            current = None;
                            next_subframe_target = None;
                            feed.audio_expected.store(false, Ordering::Relaxed);
                        }
                    }
                }
                PlayerRequest::SetPlaylist(items) => {
                    playlist = items;
                }
                PlayerRequest::NextTrack => {
                    if let Some(item) = next_in_list(&playlist, current.as_ref().map(|s| &s.item)) {
                        match load_track(&item, feed.sample_rate) {
                            Ok(mut state) => {
                                state.emulator.set_disable_aa(!anti_aliasing);
                                prefill(&mut state.emulator, &mut feed, volume, 150);
                                current = Some(state);
                                next_subframe_target = Some(Instant::now());
                                feed.audio_expected.store(true, Ordering::Relaxed);
                                event_cb(PlayerEvent::TrackStarted {
                                    index_hint: None,
                                    item,
                                });
                            }
                            Err(e) => {
                                event_cb(PlayerEvent::Error(format!("Failed to load track: {}", e)));
                            }
                        }
                    }
                }
                PlayerRequest::PreviousTrack => {
                    if let Some(item) = prev_in_list(&playlist, current.as_ref().map(|s| &s.item)) {
                        match load_track(&item, feed.sample_rate) {
                            Ok(mut state) => {
                                state.emulator.set_disable_aa(!anti_aliasing);
                                prefill(&mut state.emulator, &mut feed, volume, 150);
                                current = Some(state);
                                next_subframe_target = Some(Instant::now());
                                feed.audio_expected.store(true, Ordering::Relaxed);
                                event_cb(PlayerEvent::TrackStarted {
                                    index_hint: None,
                                    item,
                                });
                            }
                            Err(e) => {
                                event_cb(PlayerEvent::Error(format!("Failed to load track: {}", e)));
                            }
                        }
                    }
                }
                PlayerRequest::Pause => {
                    paused = true;
                    feed.audio_expected.store(false, Ordering::Relaxed);
                    event_cb(PlayerEvent::PlaybackPaused);
                }
                PlayerRequest::Resume => {
                    paused = false;
                    if current.is_some() {
                        next_subframe_target = Some(Instant::now());
                        feed.audio_expected.store(true, Ordering::Relaxed);
                    }
                    event_cb(PlayerEvent::PlaybackResumed);
                }
                PlayerRequest::SetVolume(v) => {
                    volume = v;
                }
                PlayerRequest::SetAntiAliasing(enabled) => {
                    anti_aliasing = enabled;
                    if let Some(state) = current.as_mut() {
                        state.emulator.set_disable_aa(!enabled);
                    }
                }
                PlayerRequest::SetRepeatTrack(enabled) => {
                    repeat_track = enabled;
                }
                PlayerRequest::Terminate => {
                    feed.audio_expected.store(false, Ordering::Relaxed);
                    terminating = true;
                }
            }
        }

        if terminating {
            break;
        }

        let Some(state) = current.as_mut() else {
            continue;
        };

        // Pausing is handled at the rx-recv() level above (blocking recv
        // when paused), so by the time we reach the sub-frame work we
        // know we're not paused.

        // Produce one full NES frame as N evenly-paced sub-frames.
        for sub_index in 0..SUB_FRAMES_PER_FRAME {
            // Wall-clock pace each sub-frame.
            if let Some(target) = next_subframe_target {
                pace_to(target);
                let now = Instant::now();
                if now > target + subframe_period * 8 {
                    // Fell badly behind (e.g. unpaused after a long pause)
                    // — reset to avoid sprinting through accumulated frames.
                    next_subframe_target = Some(now + subframe_period);
                } else {
                    next_subframe_target = Some(target + subframe_period);
                }
            }

            let scanlines = if sub_index == SUB_FRAMES_PER_FRAME - 1 {
                scanlines_last_subframe
            } else {
                scanlines_per_subframe
            };
            state.emulator.run_scanlines(scanlines);

            let frame = state.emulator.get_piano_roll_frame();
            latest_frame.store(Arc::new(frame));
            on_new_frame();
        }

        // Per-NES-frame bookkeeping (Update event + loop detection).
        state.emulator.end_frame();
        current_frame.store(state.emulator.last_frame() as u64, Ordering::Relaxed);

        // Drain every sample the APU emitted this frame and push them all into
        // the ring buffer. push_samples blocks when the buffer is full — that
        // back-pressure is what paces emulation to real-time.
        let samples = state.emulator.drain_audio_samples(1);
        if !samples.is_empty() {
            push_samples(&mut feed, &samples, volume);
        }

        // Crude end-of-track: stop on song end (Cxx) for FT-based drivers, or when an
        // NSFe duration was provided and we've passed it.
        if track_ended(&state.emulator, &state.item, repeat_track) {
            event_cb(PlayerEvent::TrackEnded);
            current = None;
            feed.audio_expected.store(false, Ordering::Relaxed);
        }
    }
}

fn push_samples(feed: &mut AudioFeed, samples: &[i16], volume: u8) {
    let scale = volume as i32;
    let channels = feed.channels as usize;

    // Expand mono → device channels into a contiguous buffer, then batch-push.
    let mut out: Vec<i16> = Vec::with_capacity(samples.len() * channels);
    for &s in samples {
        let scaled = ((s as i32 * scale) / 255) as i16;
        for _ in 0..channels {
            out.push(scaled);
        }
    }

    // Block-push: keep retrying until everything's in. With wall-clock
    // pacing in the main loop, this rarely blocks; when it does it just
    // smooths over short sample-rate mismatches between the device and
    // what we configured.
    let mut written = 0;
    while written < out.len() {
        let n = feed.producer.push_slice(&out[written..]);
        written += n;
        if written < out.len() {
            thread::sleep(Duration::from_micros(500));
        }
    }
}

/// Sleep + spin hybrid that returns very close to `target`. With Windows
/// multimedia timer resolution set to 1 ms (see main::win_timer) the sleep
/// portion is accurate to ~1 ms; the spin polishes off the last bit.
fn pace_to(target: Instant) {
    loop {
        let now = Instant::now();
        if now >= target {
            return;
        }
        let remaining = target - now;
        if remaining > Duration::from_micros(1500) {
            thread::sleep(remaining - Duration::from_micros(1000));
        } else {
            // Last sub-ms — spin to avoid sleep undershoot.
            while Instant::now() < target {
                std::hint::spin_loop();
            }
            return;
        }
    }
}

/// Spin the emulator without going to sleep until the ring buffer holds at
/// least `target_ms` of audio. Skips sample data on the very first call so the
/// audio device never gets handed silence on track-start.
fn prefill(emulator: &mut Emulator, feed: &mut AudioFeed, volume: u8, target_ms: u32) {
    let target_samples =
        (feed.sample_rate as u64 * target_ms as u64 / 1000) as usize * feed.channels as usize;
    let cap = feed.producer.capacity().get();
    let target = target_samples.min(cap.saturating_sub(64));

    let mut guard = 0;
    while feed.producer.occupied_len() < target && guard < 600 {
        emulator.step();
        let samples = emulator.drain_audio_samples(1);
        if !samples.is_empty() {
            push_samples(feed, &samples, volume);
        }
        guard += 1;
    }
}

fn track_ended(emulator: &Emulator, item: &PlaylistItem, repeat_track: bool) -> bool {
    // Always-on end conditions: an explicit driver end marker (Cxx in
    // FamiTracker) and the NSFe/M3U duration. These represent "the song
    // really ended" and override the repeat flag.
    if let Some(pos) = emulator.get_song_position() {
        if pos.end {
            return true;
        }
    }
    if let Some(secs) = item.duration_seconds {
        let frames = (secs as f64 * crate::emulator::NES_NTSC_FRAMERATE) as u64;
        if emulator.last_frame() as u64 > frames {
            return true;
        }
    }

    // Loop-based end: after the FT loop detector has seen the song repeat
    // once, advance to the next playlist track — unless the user wants
    // the current track to loop indefinitely.
    if !repeat_track {
        if let Some(count) = emulator.loop_count() {
            if count >= 1 {
                return true;
            }
        }
    }

    false
}

fn load_track(item: &PlaylistItem, sample_rate: u32) -> Result<PlaybackState> {
    let mut emulator = Emulator::new();
    emulator.init(None);
    emulator
        .open(item.file_path.to_str().context("Invalid file path")?)
        .with_context(|| format!("Failed to open {}", item.file_path.display()))?;
    emulator.select_track(item.track_index);
    emulator.config_audio(sample_rate as u64, 0x10000, false, true, false);
    emulator.set_piano_roll_size(PLAYER_CANVAS_W, PLAYER_CANVAS_H);
    // AA is configured by the caller after loading (see the SetAntiAliasing
    // request handling in run()), so the user's choice persists across
    // track loads.

    // Step a few frames and discard their audio to skip the NSF init transient
    // (writing APU registers during init produces a non-zero first sample,
    // which is audible as a startup click).
    for _ in 0..6 {
        emulator.step();
    }
    emulator.clear_sample_buffer();
    // Also drain whatever's still queued in the APU itself.
    let _ = emulator.drain_audio_samples(1);

    Ok(PlaybackState {
        emulator,
        item: item.clone(),
        loaded_file: item.file_path.clone(),
    })
}

fn next_in_list(playlist: &[PlaylistItem], current: Option<&PlaylistItem>) -> Option<PlaylistItem> {
    if playlist.is_empty() {
        return None;
    }
    let idx = current
        .and_then(|cur| find_index(playlist, cur))
        .map(|i| i + 1)
        .unwrap_or(0);
    playlist.get(idx).cloned()
}

fn prev_in_list(playlist: &[PlaylistItem], current: Option<&PlaylistItem>) -> Option<PlaylistItem> {
    if playlist.is_empty() {
        return None;
    }
    let idx = current
        .and_then(|cur| find_index(playlist, cur))
        .map(|i| i.saturating_sub(1))
        .unwrap_or(0);
    playlist.get(idx).cloned()
}

fn find_index(playlist: &[PlaylistItem], item: &PlaylistItem) -> Option<usize> {
    playlist
        .iter()
        .position(|x| x.file_path == item.file_path && x.track_index == item.track_index)
}
