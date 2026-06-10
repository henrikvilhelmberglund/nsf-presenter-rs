use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::fs;
use crate::emulator::{m3u_searcher, Nsf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaylistItem {
    pub file_path: PathBuf,
    /// 1-indexed NSF subsong (matches `Emulator::select_track`).
    pub track_index: u8,
    pub display_name: String,
    pub duration_seconds: Option<u32>,
}

#[derive(Default)]
pub struct Playlist {
    items: Vec<PlaylistItem>,
    current: Option<usize>,
    pub repeat: bool,
}

impl Playlist {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn items(&self) -> &[PlaylistItem] {
        &self.items
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    pub fn current(&self) -> Option<&PlaylistItem> {
        self.current.and_then(|i| self.items.get(i))
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.current = None;
    }

    /// Replace the playlist's items wholesale — used by config-restore
    /// to rehydrate a saved playlist on startup without re-reading the
    /// NSF files (we already saved every PlaylistItem's metadata).
    pub fn set_items(&mut self, items: Vec<PlaylistItem>) {
        self.items = items;
        self.current = None;
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Make the item at `index` current. Returns the item if the index is valid.
    pub fn select(&mut self, index: usize) -> Option<PlaylistItem> {
        if index >= self.items.len() {
            return None;
        }
        self.current = Some(index);
        Some(self.items[index].clone())
    }

    /// Advance to the next track. Wraps to 0 if `repeat` is set, else returns None at end.
    pub fn advance(&mut self) -> Option<PlaylistItem> {
        let next = match self.current {
            Some(i) if i + 1 < self.items.len() => i + 1,
            Some(_) => {
                if self.repeat && !self.items.is_empty() {
                    0
                } else {
                    self.current = None;
                    return None;
                }
            }
            None if !self.items.is_empty() => 0,
            None => return None,
        };
        self.current = Some(next);
        Some(self.items[next].clone())
    }

    pub fn previous(&mut self) -> Option<PlaylistItem> {
        let prev = match self.current {
            Some(0) => {
                if self.repeat && !self.items.is_empty() {
                    self.items.len() - 1
                } else {
                    return None;
                }
            }
            Some(i) => i - 1,
            None if !self.items.is_empty() => 0,
            None => return None,
        };
        self.current = Some(prev);
        Some(self.items[prev].clone())
    }

    /// Read the NSF, expand each subsong into a `PlaylistItem`, and append them.
    pub fn append_file<P: AsRef<Path>>(&mut self, path: P) -> Result<usize> {
        let items = items_for_file(path.as_ref())?;
        let added = items.len();
        self.items.extend(items);
        Ok(added)
    }

    pub fn replace_with_file<P: AsRef<Path>>(&mut self, path: P) -> Result<usize> {
        self.clear();
        self.append_file(path)
    }

    /// Recursively walk `dir`, appending every .nsf/.nsfe file found (sorted).
    pub fn append_folder<P: AsRef<Path>>(&mut self, dir: P) -> Result<usize> {
        let mut files = Vec::new();
        collect_nsf_files(dir.as_ref(), &mut files)?;
        files.sort();

        let mut added = 0;
        for path in files {
            match self.append_file(&path) {
                Ok(n) => added += n,
                Err(e) => eprintln!("Skipping {}: {}", path.display(), e),
            }
        }
        Ok(added)
    }
}

fn items_for_file(path: &Path) -> Result<Vec<PlaylistItem>> {
    let cart_data = fs::read(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;

    let nsf = Nsf::from(&cart_data);
    if !nsf.magic_valid() {
        anyhow::bail!("Not a valid NSF/NSFe file: {}", path.display());
    }
    let nsfe_metadata = nsf.nsfe_metadata();
    let m3u_metadata = m3u_searcher::search(path).unwrap_or_default();

    let song_count = nsf.songs().max(1);
    let mut out = Vec::with_capacity(song_count as usize);

    let filename = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown")
        .to_string();

    for i in 0..song_count {
        let track_index = i + 1;
        let display_name = nsfe_metadata
            .as_ref()
            .and_then(|m| m.track_title(track_index as usize))
            .or_else(|| m3u_metadata.get(&i).map(|(t, _)| t.clone()))
            .unwrap_or_else(|| format!("{} — Track {}", filename, track_index));

        let duration_seconds = nsfe_metadata
            .as_ref()
            .and_then(|m| m.track_duration(track_index as usize))
            .map(|frames| (frames as f64 / crate::emulator::NES_NTSC_FRAMERATE) as u32)
            .or_else(|| {
                m3u_metadata
                    .get(&i)
                    .and_then(|(_, d)| d.map(|d| d.as_secs() as u32))
            });

        out.push(PlaylistItem {
            file_path: path.to_path_buf(),
            track_index,
            display_name,
            duration_seconds,
        });
    }

    Ok(out)
}

fn collect_nsf_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)
        .with_context(|| format!("Failed to read directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let _ = collect_nsf_files(&path, out);
        } else if file_type.is_file() {
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                let ext = ext.to_ascii_lowercase();
                if ext == "nsf" || ext == "nsfe" {
                    out.push(path);
                }
            }
        }
    }
    Ok(())
}
