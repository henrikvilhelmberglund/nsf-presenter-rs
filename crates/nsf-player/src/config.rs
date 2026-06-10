//! Portable per-launch persistence for nsf-player.
//!
//! Reads/writes `config.toml` next to the executable. Holds the
//! playlist + UI/playback settings so the player comes back up in the
//! same state the user left it in. Save failures are non-fatal (logged
//! to stderr) — we'd rather lose persistence than crash on a read-only
//! mount or transient I/O hiccup.

use anyhow::{Context, Result};
use nsf_common::player::playlist::PlaylistItem;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub playlist: Vec<PlaylistItem>,
    pub view_mode: i32,
    pub scale_mode: i32,
    pub anti_aliasing: bool,
    pub volume: i32,
    pub repeat: bool,
}

impl Default for Config {
    fn default() -> Self {
        // Mirror the defaults baked into the .slint file so a missing /
        // brand-new config.toml gives the same first-run experience the
        // hardcoded defaults always did.
        Self {
            playlist: Vec::new(),
            view_mode: 0,
            scale_mode: 2,
            anti_aliasing: false,
            volume: 255,
            repeat: false,
        }
    }
}

impl Config {
    /// Read `config.toml` from the executable's directory. Returns a
    /// default config if the file is missing or unreadable (a brand-new
    /// install or a corrupted file shouldn't block startup).
    pub fn load() -> Self {
        let Some(path) = config_path() else { return Self::default() };
        let Ok(text) = fs::read_to_string(&path) else { return Self::default() };
        match toml::from_str::<Config>(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("config.toml parse error ({}); using defaults", e);
                Self::default()
            }
        }
    }

    /// Serialize to `config.toml` next to the executable. Errors are
    /// returned but the caller should log-and-continue — losing
    /// persistence is annoying, not fatal.
    pub fn save(&self) -> Result<()> {
        let path = config_path().context("can't locate exe directory")?;
        let text = toml::to_string_pretty(self).context("serialize config")?;
        fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

fn config_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join(CONFIG_FILE_NAME)))
}
