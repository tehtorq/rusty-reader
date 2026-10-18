//! Remembering preferences and where you left off in each book.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::book::{self, Metadata};
use crate::platform;
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
    /// Identity across file names; see [`Metadata::key`].
    #[serde(default)]
    pub key: Option<String>,
}

/// What became of a book offered to the shelf.
#[derive(Debug, PartialEq)]
pub enum Admitted {
    /// Newly shelved, at this path.
    Added(PathBuf),
    /// Already on the shelf, perhaps under another file name: this is the path to open.
    Already(PathBuf),
    /// Found by a folder scan, but removed from the shelf before.
    Skipped,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub settings: Settings,
    pub recents: Vec<RecentBook>,
    /// Books already found in the books folder, so one removed from the shelf stays removed.
    pub seen: Vec<PathBuf>,
}

impl Store {
    fn file() -> Option<PathBuf> {
        Some(crate::platform::config_dir()?.join("state.ron"))
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

    /// Offer a book to the shelf. Books merely found in the books folder (`imported == false`)
    /// stay off it if they were removed before; books the person chose come back regardless.
    pub fn admit(&mut self, path: &Path, imported: bool) -> Admitted {
        let meta = book::metadata(path);
        self.admit_known(path, meta, imported)
    }

    fn admit_known(&mut self, path: &Path, meta: Option<Metadata>, imported: bool) -> Admitted {
        if !self.seen.iter().any(|p| p == path) {
            self.seen.push(path.to_owned());
        } else if !imported && self.recent(path).is_none() {
            return Admitted::Skipped;
        }
        if self.recent(path).is_some() {
            return Admitted::Already(path.to_owned());
        }

        // The same book under another name: keep the existing entry, and with it the place.
        let key = meta.as_ref().map(|m| m.key.clone());
        if let Some(entry) = self.recents.iter_mut().find(|r| r.key.is_some() && r.key == key) {
            if entry.path.exists() {
                // A second copy of our own is just clutter.
                if platform::is_managed(path) {
                    let _ = std::fs::remove_file(path);
                }
            } else {
                entry.path = path.to_owned();
            }
            return Admitted::Already(entry.path.clone());
        }

        let (title, author) = match meta {
            Some(m) => (m.title, m.author),
            None => (path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), None),
        };
        self.recents.push(RecentBook {
            path: path.to_owned(),
            title,
            author,
            chapter: 0,
            offset: 0,
            progress: 0.0,
            opened: 0,
            key,
        });
        Admitted::Added(path.to_owned())
    }

    /// Take a book off the shelf, deleting the app's own copy of it (never one of yours).
    pub fn forget(&mut self, index: usize) {
        if index >= self.recents.len() {
            return;
        }
        let book = self.recents.remove(index);
        if platform::is_managed(&book.path) {
            let _ = std::fs::remove_file(&book.path);
            self.seen.retain(|p| p != &book.path);
        }
    }

    /// Note a place reached in a book, straight into the saved state: for the narrator, which
    /// reads on while the app's own copy of the state may not be looking.
    pub fn remember_place(path: &Path, chapter: usize, offset: usize, progress: f32) {
        let mut store = Store::load();
        if let Some(entry) = store.recent_mut(path) {
            entry.chapter = chapter;
            entry.offset = offset;
            entry.progress = progress;
            store.save();
        }
    }

    /// Shelves saved before books had keys learn them, so duplicates can be recognised.
    pub fn fill_keys(&mut self) -> bool {
        let mut changed = false;
        for entry in self.recents.iter_mut().filter(|r| r.key.is_none()) {
            if let Some(meta) = book::metadata(&entry.path) {
                entry.key = Some(meta.key);
                changed = true;
            }
        }
        changed
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
                key: None,
            },
        };
        if entry.key.is_none() {
            entry.key = book::metadata(path).map(|m| m.key);
        }
        entry.title = title.to_owned();
        entry.author = author.map(str::to_owned);
        entry.opened = now();
        self.recents.insert(0, entry);
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(title: &str, id: &str) -> Option<Metadata> {
        Some(Metadata { title: title.into(), author: None, key: format!("{title}|{id}") })
    }

    #[test]
    fn removed_books_stay_removed_unless_chosen_again() {
        let mut store = Store::default();
        let a = PathBuf::from("/books/a.epub");

        assert_eq!(store.admit_known(&a, meta("A", "1"), false), Admitted::Added(a.clone()));
        assert_eq!(store.admit_known(&a, meta("A", "1"), true), Admitted::Already(a.clone()));

        store.forget(0);
        assert_eq!(store.admit_known(&a, meta("A", "1"), false), Admitted::Skipped);
        assert_eq!(store.admit_known(&a, meta("A", "1"), true), Admitted::Added(a.clone()));
        assert_eq!(store.recents.len(), 1);
    }

    #[test]
    fn a_renamed_copy_keeps_its_place() {
        let mut store = Store::default();
        let old = PathBuf::from("/gone/Eye of the World.epub");
        let new = PathBuf::from("/books/Eye of the World (1).epub");
        store.admit_known(&old, meta("The Eye of the World", "isbn"), true);
        store.recents[0].offset = 4242;

        // The old file no longer exists, so the entry moves to the new one, place intact.
        assert_eq!(
            store.admit_known(&new, meta("The Eye of the World", "isbn"), true),
            Admitted::Already(new.clone())
        );
        assert_eq!(store.recents.len(), 1);
        assert_eq!(store.recents[0].path, new);
        assert_eq!(store.recents[0].offset, 4242);
    }

    #[test]
    fn a_shared_placeholder_identifier_does_not_merge_books() {
        let mut store = Store::default();
        let a = PathBuf::from("/books/a.epub");
        let b = PathBuf::from("/books/b.epub");
        store.admit_known(&a, meta("Emma", "123"), true);
        assert_eq!(store.admit_known(&b, meta("Persuasion", "123"), true), Admitted::Added(b.clone()));
        assert_eq!(store.recents.len(), 2);
    }
}
