pub mod audio;
pub mod playlist;
pub mod player_thread;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::player::player_thread::{blank_frame, spawn, PlayerEvent, PlayerRequest, PLAYER_CANVAS_H, PLAYER_CANVAS_W};
use crate::player::playlist::Playlist;

/// Headless smoke test: load `nsf_path`, build a playlist of all its subsongs,
/// and play them through. Prints status until the playlist ends or Ctrl-C is hit.
pub fn smoke_test(nsf_path: &str) {
    let path = PathBuf::from(nsf_path);
    let mut playlist = Playlist::new();
    match playlist.append_file(&path) {
        Ok(n) => println!("Loaded {} track(s) from {}", n, path.display()),
        Err(e) => {
            eprintln!("Failed to load: {}", e);
            return;
        }
    }
    if playlist.is_empty() {
        eprintln!("Playlist is empty.");
        return;
    }

    let track_ended = Arc::new(Mutex::new(false));
    let track_ended_cb = track_ended.clone();

    let latest_frame = Arc::new(ArcSwap::from_pointee(blank_frame()));
    let handle = match spawn(
        latest_frame,
        move |evt| match evt {
            PlayerEvent::TrackStarted { item, .. } => {
                println!(
                    "▶ {} (track {} of {})",
                    item.display_name, item.track_index, item.file_path.display()
                );
            }
            PlayerEvent::TrackEnded => {
                *track_ended_cb.lock().unwrap() = true;
            }
            PlayerEvent::PlaybackPaused => println!("paused"),
            PlayerEvent::PlaybackResumed => println!("resumed"),
            PlayerEvent::Error(e) => eprintln!("Player error: {}", e),
        },
        || {},
    ) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Failed to start player: {}", e);
            return;
        }
    };

    // Send the playlist and kick off the first track.
    let items: Vec<_> = playlist.items().to_vec();
    let first = items[0].clone();
    handle.tx.send(PlayerRequest::SetPlaylist(items.clone())).unwrap();
    handle.tx.send(PlayerRequest::PlayItem(first)).unwrap();

    let started = Instant::now();
    let mut last_underruns: u64 = 0;
    let mut next_track_index = 1usize;
    let mut next_dump_at = Duration::from_secs(2);
    let mut dump_counter = 0u32;

    loop {
        std::thread::sleep(Duration::from_millis(500));

        let elapsed = started.elapsed();
        let underruns = handle.underruns.load(Ordering::Relaxed);
        if underruns != last_underruns {
            println!("[t+{:>4}s] underruns: {}", elapsed.as_secs(), underruns);
            last_underruns = underruns;
        }

        // Dump a PNG of the latest frame every couple seconds so we can
        // visually confirm the frame-publication path is working before we
        // wire it to Slint.
        if elapsed >= next_dump_at {
            let frame_arc = handle.latest_frame.load_full();
            let path = format!("player_test_frame_{:02}.png", dump_counter);
            match image::save_buffer(
                &path,
                &frame_arc,
                PLAYER_CANVAS_W,
                PLAYER_CANVAS_H,
                image::ColorType::RGBA(8),
            ) {
                Ok(()) => println!("  wrote {}", path),
                Err(e) => eprintln!("  failed to write {}: {}", path, e),
            }
            dump_counter += 1;
            next_dump_at += Duration::from_secs(2);
        }

        // When a track ends, advance.
        if *track_ended.lock().unwrap() {
            *track_ended.lock().unwrap() = false;

            if next_track_index >= items.len() {
                println!("Playlist finished.");
                break;
            }
            let next = items[next_track_index].clone();
            next_track_index += 1;
            handle.tx.send(PlayerRequest::PlayItem(next)).unwrap();
        }
    }

    handle.join();
}
