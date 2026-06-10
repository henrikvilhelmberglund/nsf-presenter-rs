use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use native_dialog::{FileDialog, MessageDialog, MessageType};
use slint::{
    ComponentHandle, Image, LogicalPosition, LogicalSize, Model, SharedPixelBuffer,
    SharedString, Timer, TimerMode, VecModel, Weak, WindowPosition, WindowSize,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nsf_common::player::player_thread::{
    blank_frame, spawn as spawn_player, LatestFrame, PlayerEvent, PlayerHandle, PlayerRequest,
    ViewMode, PLAYER_CANVAS_H, PLAYER_CANVAS_W,
};
use nsf_common::player::playlist::{Playlist, PlaylistItem};

use crate::config::Config;

// `slint::include_modules!()` picks up the *last* compile()'d slint file via
// SLINT_INCLUDE_GENERATED — that's visualization.slint per build.rs. The other
// generated file (player.rs) is included explicitly below, scoped in a private
// module to avoid colliding on shared names like `TextStyle`.
slint::include_modules!();
mod player_slint {
    include!(concat!(env!("OUT_DIR"), "/player.rs"));
}
use player_slint::{PlayerWindow, PlaylistRow};

pub fn run() {
    if let Err(e) = open_player_window() {
        display_error_dialog(&format!("Failed to start player: {}", e));
    }
}

fn open_player_window() -> Result<()> {
    let config = Config::load();

    let player_w = PlayerWindow::new().context("Failed to create PlayerWindow")?;
    let viz_w = VisualizationWindow::new().context("Failed to create VisualizationWindow")?;

    // Apply persisted settings to the Slint window properties BEFORE
    // any user interaction. These take effect on show() so the UI
    // reflects the saved state immediately.
    player_w.set_view_mode(config.view_mode);
    player_w.set_scale_mode(config.scale_mode);
    player_w.set_anti_aliasing(config.anti_aliasing);
    player_w.set_volume(config.volume);
    player_w.set_repeat_playlist(config.repeat);

    let state = Rc::new(RefCell::new(PlayerWindowState::new(viz_w.as_weak(), config)?));

    // Push the rehydrated playlist into the UI and mirror the persisted
    // settings to the player thread (the player thread starts with
    // hardcoded defaults; without these sends the audio / view-mode /
    // AA state wouldn't match what the UI now claims).
    {
        let mut s = state.borrow_mut();
        // Repeat is a playlist-level concern (wrap-around at end of
        // playlist), not a player-thread one — set it directly here.
        s.playlist.repeat = s.config.repeat;
        sync_playlist_to_ui(&player_w, &s.playlist);
        s.send(PlayerRequest::SetVolume(s.config.volume.clamp(0, 255) as u8));
        s.send(PlayerRequest::SetAntiAliasing(s.config.anti_aliasing));
        s.send(PlayerRequest::SetViewMode(match s.config.view_mode {
            1 => ViewMode::Perspective,
            _ => ViewMode::Classic,
        }));
    }

    wire_callbacks(&player_w, &state);
    install_status_timer(&player_w, &state);
    install_event_pump(&player_w, &state);

    // Closing the player window terminates the player + closes viz + exits.
    {
        let state = state.clone();
        let viz_weak = viz_w.as_weak();
        player_w.window().on_close_requested(move || {
            if let Some(viz) = viz_weak.upgrade() {
                let _ = viz.hide();
            }
            state.borrow_mut().shutdown();
            // Exit the event loop so main() returns.
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        });
    }

    // Closing the visualization just hides it — the player keeps playing
    // and the user can re-open it from the toolbar.
    {
        let state = state.clone();
        viz_w.window().on_close_requested(move || {
            state.borrow_mut().visualization_visible = false;
            slint::CloseRequestResponse::HideWindow
        });
    }

    player_w.set_visualization_visible(true);

    // Default placement: controls window on the left, visualization to its
    // right at the same Y. Sizes come from the .slint preferred-width/height.
    // set_size/set_position MUST be called after show() — Slint applies its
    // own initial sizing on show() and overrides earlier set_size calls.
    let player_x: f32 = 100.0;
    let player_y: f32 = 100.0;
    let player_w_size: f32 = 640.0;
    let player_h_size: f32 = 620.0;
    // Initial viz size honors the persisted scale_mode so a saved 1x
    // launches at 960×540 instead of being letterboxed inside 1920×1080.
    let saved_scale = state.borrow().config.scale_mode;
    let (viz_w_size, viz_h_size): (f32, f32) = match saved_scale {
        1 => (960.0, 540.0),
        _ => (1920.0, 1080.0),
    };
    let gap: f32 = 12.0;

    player_w.show().context("Failed to show player window")?;
    viz_w.show().context("Failed to show visualization window")?;

    player_w
        .window()
        .set_size(WindowSize::Logical(LogicalSize::new(player_w_size, player_h_size)));
    player_w
        .window()
        .set_position(WindowPosition::Logical(LogicalPosition::new(player_x, player_y)));

    viz_w
        .window()
        .set_size(WindowSize::Logical(LogicalSize::new(viz_w_size, viz_h_size)));
    viz_w.window().set_position(WindowPosition::Logical(LogicalPosition::new(
        player_x + player_w_size + gap,
        player_y,
    )));

    slint::run_event_loop().context("Slint event loop failed")?;

    // Drop everything once the event loop returns.
    drop(state);
    drop(player_w);
    drop(viz_w);
    Ok(())
}

struct PlayerWindowState {
    player: Option<PlayerHandle>,
    playlist: Playlist,
    viz_weak: Weak<VisualizationWindow>,
    visualization_visible: bool,
    event_queue: Arc<Mutex<Vec<PlayerEvent>>>,
    /// Persisted UI/playback state. Mutated by callbacks and flushed
    /// to disk via `save_config`. Holds a snapshot of every setting
    /// we round-trip (playlist contents, view mode, scale, AA, volume,
    /// repeat) — anything not in here doesn't persist across launches.
    config: Config,
}

impl PlayerWindowState {
    fn new(viz_weak: Weak<VisualizationWindow>, config: Config) -> Result<Self> {
        let event_queue: Arc<Mutex<Vec<PlayerEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let event_queue_thread = event_queue.clone();

        let latest_frame: Arc<LatestFrame> = Arc::new(ArcSwap::from_pointee(blank_frame()));
        let pending = Arc::new(AtomicBool::new(false));

        let latest_frame_cb = latest_frame.clone();
        let pending_cb = pending.clone();
        let viz_weak_cb = viz_weak.clone();
        let on_new_frame = move || {
            if pending_cb.swap(true, Ordering::Acquire) {
                return;
            }
            let latest = latest_frame_cb.clone();
            let pending_inner = pending_cb.clone();
            let weak = viz_weak_cb.clone();
            let _ = weak.upgrade_in_event_loop(move |viz| {
                let frame_arc = latest.load_full();
                let img = build_image(&frame_arc);
                viz.set_visualization_frame(img);
                viz.window().request_redraw();
                pending_inner.store(false, Ordering::Release);
            });
        };

        let player = spawn_player(
            latest_frame,
            move |evt| {
                event_queue_thread.lock().unwrap().push(evt);
            },
            on_new_frame,
        )
        .context("Failed to start player thread")?;

        let mut playlist = Playlist::new();
        if !config.playlist.is_empty() {
            playlist.set_items(config.playlist.clone());
        }

        Ok(Self {
            player: Some(player),
            playlist,
            viz_weak,
            visualization_visible: true,
            event_queue,
            config,
        })
    }

    fn shutdown(&mut self) {
        if let Some(handle) = self.player.take() {
            handle.join();
        }
    }

    fn send(&self, req: PlayerRequest) {
        if let Some(p) = &self.player {
            let _ = p.tx.send(req);
        }
    }

    /// Snapshot the current playlist into `self.config` and persist to
    /// disk. Call after any playlist mutation (append / clear).
    fn save_playlist(&mut self) {
        self.config.playlist = self.playlist.items().to_vec();
        self.flush_config();
    }

    /// Persist `self.config` to disk. Errors are logged but don't
    /// propagate — losing persistence shouldn't take down the player.
    fn flush_config(&self) {
        if let Err(e) = self.config.save() {
            eprintln!("Failed to save config.toml: {}", e);
        }
    }
}

fn wire_callbacks(window: &PlayerWindow, state: &Rc<RefCell<PlayerWindowState>>) {
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_open_files(move || {
            let files = FileDialog::new()
                .add_filter("All supported formats", &["nsf", "nsfe"])
                .add_filter("Nintendo Sound Format module", &["nsf"])
                .add_filter("Extended Nintendo Sound Format module", &["nsfe"])
                .show_open_multiple_file()
                .unwrap_or_default();

            if files.is_empty() {
                return;
            }

            let started_empty = state.borrow().playlist.is_empty();
            for f in &files {
                let res = state.borrow_mut().playlist.append_file(f);
                if let Err(e) = res {
                    eprintln!("Skipping {}: {}", f.display(), e);
                }
            }
            sync_playlist_to_ui(&weak.unwrap(), &state.borrow().playlist);
            push_playlist_to_thread(&state.borrow());
            state.borrow_mut().save_playlist();
            if started_empty && !state.borrow().playlist.is_empty() {
                let first = state.borrow_mut().playlist.select(0);
                if let Some(first) = first {
                    state.borrow().send(PlayerRequest::PlayItem(first));
                    weak.unwrap().set_playing(true);
                }
            }
        });
    }

    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_open_folder(move || {
            let folder = FileDialog::new().show_open_single_dir().unwrap_or(None);
            let Some(folder) = folder else { return };

            let started_empty = state.borrow().playlist.is_empty();
            let res = state.borrow_mut().playlist.append_folder(&folder);
            if let Err(e) = res {
                eprintln!("Folder scan error: {}", e);
            }
            sync_playlist_to_ui(&weak.unwrap(), &state.borrow().playlist);
            push_playlist_to_thread(&state.borrow());
            state.borrow_mut().save_playlist();
            if started_empty && !state.borrow().playlist.is_empty() {
                let first = state.borrow_mut().playlist.select(0);
                if let Some(first) = first {
                    state.borrow().send(PlayerRequest::PlayItem(first));
                    weak.unwrap().set_playing(true);
                }
            }
        });
    }

    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_clear_playlist(move || {
            state.borrow_mut().playlist.clear();
            sync_playlist_to_ui(&weak.unwrap(), &state.borrow().playlist);
            push_playlist_to_thread(&state.borrow());
            state.borrow_mut().save_playlist();
        });
    }

    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_play_pause(move || {
            let w = weak.unwrap();
            if w.get_playing() {
                state.borrow().send(PlayerRequest::Pause);
                w.set_playing(false);
            } else {
                state.borrow().send(PlayerRequest::Resume);
                w.set_playing(true);
            }
        });
    }

    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_next_track(move || {
            let item = state.borrow_mut().playlist.advance();
            if let Some(item) = item {
                state.borrow().send(PlayerRequest::PlayItem(item));
                weak.unwrap().set_playing(true);
                refresh_current_index(&weak.unwrap(), &state.borrow().playlist);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_prev_track(move || {
            let item = state.borrow_mut().playlist.previous();
            if let Some(item) = item {
                state.borrow().send(PlayerRequest::PlayItem(item));
                weak.unwrap().set_playing(true);
                refresh_current_index(&weak.unwrap(), &state.borrow().playlist);
            }
        });
    }

    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_play_index(move |idx| {
            let item = state.borrow_mut().playlist.select(idx as usize);
            if let Some(item) = item {
                state.borrow().send(PlayerRequest::PlayItem(item));
                weak.unwrap().set_playing(true);
                refresh_current_index(&weak.unwrap(), &state.borrow().playlist);
            }
        });
    }

    {
        let state = state.clone();
        window.on_set_volume(move |v| {
            let clamped = v.clamp(0, 255);
            let mut s = state.borrow_mut();
            s.send(PlayerRequest::SetVolume(clamped as u8));
            s.config.volume = clamped;
            s.flush_config();
        });
    }

    {
        let state = state.clone();
        window.on_toggle_repeat(move |checked| {
            let mut s = state.borrow_mut();
            // Playlist-level repeat: wrap to track 0 when the last
            // track ends. The player thread keeps auto-ending tracks
            // via loop detection regardless; this just controls what
            // `Playlist::advance` returns at the end of the list.
            s.playlist.repeat = checked;
            s.config.repeat = checked;
            s.flush_config();
        });
    }

    {
        let state = state.clone();
        window.on_set_anti_aliasing(move |on| {
            let mut s = state.borrow_mut();
            s.send(PlayerRequest::SetAntiAliasing(on));
            s.config.anti_aliasing = on;
            s.flush_config();
        });
    }

    {
        let state = state.clone();
        window.on_set_view_mode(move |mode| {
            let vm = match mode {
                1 => ViewMode::Perspective,
                _ => ViewMode::Classic,
            };
            let mut s = state.borrow_mut();
            s.send(PlayerRequest::SetViewMode(vm));
            s.config.view_mode = mode;
            s.flush_config();
        });
    }

    {
        let state = state.clone();
        window.on_set_scale_mode(move |mode| {
            let mut s = state.borrow_mut();
            if let Some(viz) = s.viz_weak.upgrade() {
                viz.set_scale_mode(mode);
                // 1x and 2x snap the viz window to exact pixel sizes so the
                // user actually sees the canvas at that resolution instead
                // of letterboxed inside a bigger window. Scaled mode leaves
                // the user's window size alone.
                match mode {
                    1 => {
                        viz.window().set_size(WindowSize::Logical(
                            LogicalSize::new(960.0, 540.0),
                        ));
                    }
                    2 => {
                        viz.window().set_size(WindowSize::Logical(
                            LogicalSize::new(1920.0, 1080.0),
                        ));
                    }
                    _ => {}
                }
            }
            s.config.scale_mode = mode;
            s.flush_config();
        });
    }

    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_toggle_visualization(move || {
            let mut s = state.borrow_mut();
            s.visualization_visible = !s.visualization_visible;
            let visible = s.visualization_visible;
            if let Some(viz) = s.viz_weak.upgrade() {
                if visible {
                    let _ = viz.show();
                } else {
                    let _ = viz.hide();
                }
            }
            drop(s);
            weak.unwrap().set_visualization_visible(visible);
        });
    }
}

fn install_status_timer(player_w: &PlayerWindow, state: &Rc<RefCell<PlayerWindowState>>) {
    let player_weak = player_w.as_weak();
    let state = state.clone();
    let last_underruns: Cell<u64> = Cell::new(u64::MAX);

    let timer = Box::leak(Box::new(Timer::default()));
    timer.start(
        TimerMode::Repeated,
        Duration::from_millis(250),
        move || {
            let state = state.borrow();
            let Some(player) = state.player.as_ref() else { return };
            let Some(w) = player_weak.upgrade() else { return };

            let underruns = player.underruns.load(Ordering::Relaxed);
            if underruns != last_underruns.get() {
                last_underruns.set(underruns);
                w.set_status_text(SharedString::from(format!("underruns: {}", underruns)));
            }
        },
    );
}

fn build_image(frame: &Arc<Vec<u8>>) -> Image {
    let mut buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(PLAYER_CANVAS_W, PLAYER_CANVAS_H);
    let bytes = buf.make_mut_bytes();
    let src: &[u8] = frame.as_ref();
    let n = bytes.len().min(src.len());
    bytes[..n].copy_from_slice(&src[..n]);
    Image::from_rgba8(buf)
}

fn install_event_pump(window: &PlayerWindow, state: &Rc<RefCell<PlayerWindowState>>) {
    let weak = window.as_weak();
    let state = state.clone();

    let timer = Box::leak(Box::new(Timer::default()));
    timer.start(
        TimerMode::Repeated,
        Duration::from_millis(50),
        move || {
            let Some(window) = weak.upgrade() else { return };
            let events: Vec<PlayerEvent> = {
                let s = state.borrow();
                let mut q = s.event_queue.lock().unwrap();
                std::mem::take(&mut *q)
            };
            for evt in events {
                match evt {
                    PlayerEvent::TrackStarted { item, .. } => {
                        let name = item.display_name.clone();
                        window.set_now_playing_text(SharedString::from(name));
                        refresh_current_index(&window, &state.borrow().playlist);
                        window.set_playing(true);
                    }
                    PlayerEvent::TrackEnded => {
                        let next = state.borrow_mut().playlist.advance();
                        if let Some(item) = next {
                            state.borrow().send(PlayerRequest::PlayItem(item));
                        } else {
                            window.set_playing(false);
                            window.set_now_playing_text(SharedString::from("Nothing playing"));
                        }
                    }
                    PlayerEvent::PlaybackPaused => window.set_playing(false),
                    PlayerEvent::PlaybackResumed => window.set_playing(true),
                    PlayerEvent::Error(e) => eprintln!("Player error: {}", e),
                }
            }
        },
    );
}

fn sync_playlist_to_ui(window: &PlayerWindow, playlist: &Playlist) {
    let rows: Vec<PlaylistRow> = playlist
        .items()
        .iter()
        .map(|item| PlaylistRow {
            display_name: SharedString::from(item.display_name.clone()),
            file_name: SharedString::from(
                item.file_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            track_index: item.track_index as i32,
        })
        .collect();
    window.set_playlist(slint::ModelRc::new(VecModel::from(rows)));
    refresh_current_index(window, playlist);
}

fn refresh_current_index(window: &PlayerWindow, playlist: &Playlist) {
    window.set_current_track_index(playlist.current_index().map(|i| i as i32).unwrap_or(-1));
}

fn push_playlist_to_thread(state: &PlayerWindowState) {
    let items: Vec<PlaylistItem> = state.playlist.items().to_vec();
    state.send(PlayerRequest::SetPlaylist(items));
}

fn display_error_dialog(text: &str) {
    let _ = MessageDialog::new()
        .set_title("NSFPlayer")
        .set_text(text)
        .set_type(MessageType::Error)
        .show_alert();
}
