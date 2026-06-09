use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use native_dialog::FileDialog;
use slint::{ComponentHandle, Image, Model, SharedPixelBuffer, SharedString, Timer, TimerMode, VecModel, Weak};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::player::player_thread::{
    blank_frame, spawn as spawn_player, LatestFrame, PlayerEvent, PlayerHandle, PlayerRequest,
    PLAYER_CANVAS_H, PLAYER_CANVAS_W,
};
use crate::player::playlist::{Playlist, PlaylistItem};

use super::{PlayerWindow, PlaylistRow, VisualizationWindow};

/// Open the player. Creates the controls window and the visualization window
/// together. Returns immediately; both windows run on the shared Slint event
/// loop.
pub fn open_player_window() -> Result<()> {
    let player_w = PlayerWindow::new().context("Failed to create PlayerWindow")?;
    let viz_w = VisualizationWindow::new().context("Failed to create VisualizationWindow")?;
    let state = Rc::new(RefCell::new(PlayerWindowState::new(
        viz_w.as_weak(),
    )?));

    wire_callbacks(&player_w, &viz_w, &state);
    install_frame_timer(&player_w, &viz_w, &state);
    install_event_pump(&player_w, &state);

    // Closing the player window terminates the player and closes the viz.
    {
        let state = state.clone();
        let viz_weak = viz_w.as_weak();
        player_w.window().on_close_requested(move || {
            if let Some(viz) = viz_weak.upgrade() {
                let _ = viz.hide();
            }
            state.borrow_mut().shutdown();
            slint::CloseRequestResponse::HideWindow
        });
    }

    // Closing the visualization window just hides it — the player keeps
    // playing and the user can re-open via the toolbar.
    {
        let state = state.clone();
        viz_w.window().on_close_requested(move || {
            state.borrow_mut().visualization_visible = false;
            slint::CloseRequestResponse::HideWindow
        });
    }

    // Reflect initial visualization state on the player toolbar.
    player_w.set_visualization_visible(true);

    player_w.show().context("Failed to show player window")?;
    viz_w.show().context("Failed to show visualization window")?;

    // Keep both windows + state alive on the Slint event loop.
    let _keep_alive = Box::leak(Box::new((player_w, viz_w, state)));
    Ok(())
}

struct PlayerWindowState {
    player: Option<PlayerHandle>,
    playlist: Playlist,
    /// Weak handle to the visualization window so we can show/hide it from
    /// callbacks. Upgrades fail after the window is dropped on shutdown.
    viz_weak: Weak<VisualizationWindow>,
    visualization_visible: bool,
    /// Buffer used to receive events from the player thread. The player thread
    /// pushes into the mutex; the Slint timer drains it and updates the UI.
    event_queue: Arc<Mutex<Vec<PlayerEvent>>>,
}

impl PlayerWindowState {
    fn new(viz_weak: Weak<VisualizationWindow>) -> Result<Self> {
        let event_queue: Arc<Mutex<Vec<PlayerEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let event_queue_thread = event_queue.clone();

        // Push-based frame updates. The player thread calls `on_new_frame`
        // after every produced NES frame, which schedules a UI update on
        // the Slint event loop. A coalescing flag prevents the event loop
        // from getting back-logged if the GUI ever falls behind — only one
        // update is in flight at a time, and the closure always reads the
        // most recent frame from arc-swap.
        let latest_frame: Arc<LatestFrame> = Arc::new(ArcSwap::from_pointee(blank_frame()));
        let pending = Arc::new(AtomicBool::new(false));

        let latest_frame_cb = latest_frame.clone();
        let pending_cb = pending.clone();
        let viz_weak_cb = viz_weak.clone();
        let on_new_frame = move || {
            if pending_cb.swap(true, Ordering::Acquire) {
                return; // a render is already scheduled
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

        Ok(Self {
            player: Some(player),
            playlist: Playlist::new(),
            viz_weak,
            visualization_visible: true,
            event_queue,
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
}

fn wire_callbacks(
    window: &PlayerWindow,
    _viz: &VisualizationWindow,
    state: &Rc<RefCell<PlayerWindowState>>,
) {
    // Add files
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
            if started_empty && !state.borrow().playlist.is_empty() {
                let first = state.borrow_mut().playlist.select(0);
                if let Some(first) = first {
                    state.borrow().send(PlayerRequest::PlayItem(first));
                    weak.unwrap().set_playing(true);
                }
            }
        });
    }

    // Add folder
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
            if started_empty && !state.borrow().playlist.is_empty() {
                let first = state.borrow_mut().playlist.select(0);
                if let Some(first) = first {
                    state.borrow().send(PlayerRequest::PlayItem(first));
                    weak.unwrap().set_playing(true);
                }
            }
        });
    }

    // Clear
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_clear_playlist(move || {
            state.borrow_mut().playlist.clear();
            sync_playlist_to_ui(&weak.unwrap(), &state.borrow().playlist);
            push_playlist_to_thread(&state.borrow());
        });
    }

    // Play / Pause toggle
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

    // Next / Prev
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

    // Direct playlist click → play that index
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

    // Volume
    {
        let state = state.clone();
        window.on_set_volume(move |v| {
            let v = v.clamp(0, 255) as u8;
            state.borrow().send(PlayerRequest::SetVolume(v));
        });
    }

    // Repeat toggle
    {
        let state = state.clone();
        window.on_toggle_repeat(move |checked| {
            state.borrow_mut().playlist.repeat = checked;
        });
    }

    // Show / hide visualization window
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

fn install_frame_timer(
    player_w: &PlayerWindow,
    _viz_w: &VisualizationWindow,
    state: &Rc<RefCell<PlayerWindowState>>,
) {
    let player_weak = player_w.as_weak();
    let state = state.clone();
    let last_underruns: Cell<u64> = Cell::new(u64::MAX);

    // Frame updates are push-driven via on_new_frame in PlayerWindowState
    // — this timer only refreshes the underrun count in the status bar.
    let timer = Box::leak(Box::new(Timer::default()));
    timer.start(
        TimerMode::Repeated,
        Duration::from_millis(250),
        move || {
            let state = state.borrow();
            let Some(player) = state.player.as_ref() else { return };

            let underruns = player.underruns.load(Ordering::Relaxed);
            if underruns != last_underruns.get() {
                last_underruns.set(underruns);
                if let Some(w) = player_weak.upgrade() {
                    w.set_status_text(SharedString::from(format!(
                        "underruns: {}",
                        underruns
                    )));
                }
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

/// Drain events from the player thread on the Slint thread and update UI state.
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
    window.set_current_track_index(
        playlist.current_index().map(|i| i as i32).unwrap_or(-1),
    );
}

fn push_playlist_to_thread(state: &PlayerWindowState) {
    let items: Vec<PlaylistItem> = state.playlist.items().to_vec();
    state.send(PlayerRequest::SetPlaylist(items));
}
