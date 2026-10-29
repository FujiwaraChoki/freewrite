//! Local entry storage.
//!
//! Entries live in `Documents/Freewrite` as `[uuid]-[yyyy-MM-dd-HH-mm-ss].md`,
//! the same layout the macOS app uses, so a folder can be shared between them.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use chrono::{Local, NaiveDateTime};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const DATE_FORMAT: &str = "%Y-%m-%d-%H-%M-%S";

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: Uuid,
    pub created: NaiveDateTime,
    pub filename: String,
    pub preview: String,
}

impl Entry {
    pub fn new() -> Self {
        let id = Uuid::new_v4();
        let created = Local::now().naive_local();
        Entry {
            id,
            created,
            filename: filename_for(id, created),
            preview: String::new(),
        }
    }

    /// "Mar 14", like the macOS sidebar.
    pub fn display_date(&self) -> String {
        self.created.format("%b %-d").to_string()
    }
}

pub fn filename_for(id: Uuid, created: NaiveDateTime) -> String {
    format!(
        "[{}]-[{}].md",
        id.to_string().to_uppercase(),
        created.format(DATE_FORMAT)
    )
}

/// Parse `[uuid]-[yyyy-MM-dd-HH-mm-ss].md`.
pub fn parse_filename(filename: &str) -> Option<(Uuid, NaiveDateTime)> {
    let stem = filename.strip_suffix(".md")?;
    let rest = stem.strip_prefix('[')?;
    let (uuid, rest) = rest.split_once("]-[")?;
    let date = rest.strip_suffix(']')?;
    let id = Uuid::parse_str(uuid).ok()?;
    let created = NaiveDateTime::parse_from_str(date, DATE_FORMAT).ok()?;
    Some((id, created))
}

/// First 30 characters on one line, like the macOS sidebar.
pub fn preview_for(content: &str) -> String {
    let preview = content.replace('\n', " ");
    let preview = preview.trim();
    if preview.chars().count() > 30 {
        format!("{}...", preview.chars().take(30).collect::<String>())
    } else {
        preview.to_string()
    }
}

pub fn content_hash(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The entries folder. Every write goes through here, behind one lock, so the
/// editor and the sync thread never interleave on the same file.
pub struct Store {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl Store {
    pub fn open_default() -> io::Result<Self> {
        // Minimal Linux setups often lack xdg-user-dirs; use ~/Documents like everyone else.
        let base = dirs::document_dir()
            .or_else(|| dirs::home_dir().map(|home| home.join("Documents")))
            .unwrap_or_else(|| PathBuf::from("."));
        Self::open(base.join("Freewrite"))
    }

    pub fn open(dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Store {
            dir,
            lock: Mutex::new(()),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, filename: &str) -> PathBuf {
        self.dir.join(filename)
    }

    /// All entries, newest first, with their contents.
    pub fn list(&self) -> Vec<(Entry, String)> {
        let Ok(read_dir) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut entries: Vec<(Entry, String)> = read_dir
            .filter_map(|item| item.ok())
            .filter_map(|item| {
                let filename = item.file_name().into_string().ok()?;
                let (id, created) = parse_filename(&filename)?;
                let content = fs::read_to_string(item.path()).ok()?;
                let entry = Entry {
                    id,
                    created,
                    filename,
                    preview: preview_for(&content),
                };
                Some((entry, content))
            })
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.0.created));
        entries
    }

    /// Entries keyed by id, for the sync engine. Fails if the folder can't be read.
    pub fn by_id(&self) -> io::Result<HashMap<Uuid, (Entry, String)>> {
        fs::read_dir(&self.dir)?;
        Ok(self
            .list()
            .into_iter()
            .map(|(entry, content)| (entry.id, (entry, content)))
            .collect())
    }

    pub fn read(&self, filename: &str) -> io::Result<String> {
        fs::read_to_string(self.path(filename))
    }

    pub fn modified(&self, filename: &str) -> Option<SystemTime> {
        fs::metadata(self.path(filename))
            .and_then(|m| m.modified())
            .ok()
    }

    #[cfg(test)]
    pub fn write(&self, filename: &str, content: &str) -> io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.write_locked(filename, content)
    }

    /// Replace a file only if its current content still hashes to `expected`
    /// (`None` meaning the file must not exist). Returns whether it wrote.
    pub fn write_if_unchanged(
        &self,
        filename: &str,
        expected: Option<&str>,
        content: &str,
    ) -> io::Result<bool> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.matches(filename, expected) {
            return Ok(false);
        }
        self.write_locked(filename, content)?;
        Ok(true)
    }

    pub fn delete(&self, filename: &str) -> io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        match fs::remove_file(self.path(filename)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Delete a file only if its content still hashes to `expected`.
    pub fn delete_if_unchanged(&self, filename: &str, expected: &str) -> io::Result<bool> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.matches(filename, Some(expected)) {
            return Ok(false);
        }
        fs::remove_file(self.path(filename))?;
        Ok(true)
    }

    fn matches(&self, filename: &str, expected: Option<&str>) -> bool {
        let current = fs::read_to_string(self.path(filename)).ok();
        match (current, expected) {
            (None, None) => true,
            (Some(current), Some(expected)) => content_hash(&current) == expected,
            _ => false,
        }
    }

    fn write_locked(&self, filename: &str, content: &str) -> io::Result<()> {
        // Write to a temporary file and rename, so a crash never leaves half an entry.
        let tmp = self.dir.join(format!(".{filename}.tmp"));
        fs::write(&tmp, content)?;
        fs::rename(&tmp, self.path(filename))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Store {
        let dir = std::env::temp_dir().join(format!("freewrite-test-{}", Uuid::new_v4()));
        Store::open(dir).unwrap()
    }

    #[test]
    fn filenames_round_trip_with_macos_format() {
        let name = "[0C9F4B4E-1F0A-4C7E-9D7B-7A1B2C3D4E5F]-[2025-03-14-09-30-00].md";
        let (id, created) = parse_filename(name).unwrap();
        assert_eq!(filename_for(id, created), name);
        assert!(parse_filename("entry.md").is_none());
        assert!(parse_filename("[nope]-[2025-03-14-09-30-00].md").is_none());
    }

    #[test]
    fn preview_is_truncated_to_thirty_characters() {
        assert_eq!(preview_for("\n\nhello\nworld"), "hello world");
        let long = "a".repeat(40);
        assert_eq!(preview_for(&long), format!("{}...", "a".repeat(30)));
    }

    #[test]
    fn conditional_writes_respect_the_expected_content() {
        let store = temp_store();
        let name = Entry::new().filename;
        assert!(store.write_if_unchanged(&name, None, "one").unwrap());
        assert!(!store.write_if_unchanged(&name, None, "two").unwrap());
        assert!(
            !store
                .write_if_unchanged(&name, Some(&content_hash("zzz")), "two")
                .unwrap()
        );
        assert!(
            store
                .write_if_unchanged(&name, Some(&content_hash("one")), "two")
                .unwrap()
        );
        assert_eq!(store.read(&name).unwrap(), "two");
        assert!(
            !store
                .delete_if_unchanged(&name, &content_hash("one"))
                .unwrap()
        );
        assert!(
            store
                .delete_if_unchanged(&name, &content_hash("two"))
                .unwrap()
        );
        assert!(store.list().is_empty());
        fs::remove_dir_all(store.dir()).unwrap();
    }
}
