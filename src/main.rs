mod video_builder;
mod emulator;
mod renderer;
mod player;
mod cli;
mod gui;

use std::env;
use build_time::build_time_utc;

#[cfg(windows)]
mod win_timer {
    // Bump Windows multimedia timer resolution to 1 ms so thread::sleep
    // is accurate enough for 60 Hz frame pacing in the player. Default
    // resolution is ~15.6 ms, which makes audio-clocked playback feel
    // janky even when audio is glitch-free.
    #[link(name = "winmm")]
    extern "system" {
        fn timeBeginPeriod(uPeriod: u32) -> u32;
    }
    pub fn enable_high_resolution() {
        unsafe { timeBeginPeriod(1); }
    }
}

#[cfg(not(windows))]
mod win_timer {
    pub fn enable_high_resolution() {}
}

fn main() {
    println!("NSFPresenter started! (built {})", build_time_utc!("%Y-%m-%dT%H:%M:%S"));
    win_timer::enable_high_resolution();
    video_builder::init().unwrap();

    let args: Vec<String> = env::args().collect();
    if args.len() >= 3 && args[1] == "--player-test" {
        player::smoke_test(&args[2]);
        return;
    }

    match args.len() {
        1 => gui::run(),
        _ => cli::run()
    };
}
