mod gui;

use build_time::build_time_utc;

#[cfg(windows)]
mod win_timer {
    // Bump Windows multimedia timer resolution to 1 ms so thread::sleep is
    // accurate enough for sub-frame video pacing. Default resolution is
    // ~15.6 ms.
    #[link(name = "winmm")]
    extern "system" {
        fn timeBeginPeriod(uPeriod: u32) -> u32;
    }
    pub fn enable_high_resolution() {
        unsafe {
            timeBeginPeriod(1);
        }
    }
}

#[cfg(not(windows))]
mod win_timer {
    pub fn enable_high_resolution() {}
}

fn main() {
    println!(
        "NSFPlayer started! (built {})",
        build_time_utc!("%Y-%m-%dT%H:%M:%S")
    );
    win_timer::enable_high_resolution();
    gui::run();
}
