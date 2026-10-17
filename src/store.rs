//! Remembering preferences and where you left off in each book.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::theme::Settings;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentBook {
    pub path: PathBuf,
    pub title: String,
    pub author: Option<String>,
    pub chapter: usize,
    pub offset: usize,
    pub progress: f32,
    pub opened: u64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub settings: Settings,
    pub recents: Vec<RecentBook>,
}

impl Store {
    fn file() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("rusty-reader").join("state.ron"))
    }

    pub fn load() -> Store {
        Self::file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|s| ron::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(file) = Self::file() else { return };
        let Ok(text) = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()) else { return };
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = file.with_extension("ron.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(tmp, file);
        }
    }

    pub fn recent(&self, path: &Path) -> Option<&RecentBook> {
        self.recents.iter().find(|r| r.path == path)
    }

    pub fn recent_mut(&mut self, path: &Path) -> Option<&mut RecentBook> {
        self.recents.iter_mut().find(|r| r.path == path)
    }

    /// Record that a book was opened, moving it to the top of the shelf.
    pub fn touch(&mut self, path: &Path, title: &str, author: Option<&str>) {
        let mut entry = match self.recents.iter().position(|r| r.path == path) {
            Some(i) => self.recents.remove(i),
            None => RecentBook {
                path: path.to_owned(),
                title: String::new(),
                author: None,
                chapter: 0,
                offset: 0,
                progress: 0.0,
                opened: 0,
            },
        };
        entry.title = title.to_owned();
        entry.author = author.map(str::to_owned);
        entry.opened = now();
        self.recents.insert(0, entry);
        self.recents.truncate(40);
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
