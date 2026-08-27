//! Choosing files to upload by looking at them.
//!
//! A path typed from memory is the part of uploading that goes wrong, so this walks the
//! filesystem instead — and, because the terminal can already draw photographs, shows the
//! one under the cursor while you decide.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::commands::upload::is_media;

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Whether the name looks like something imogen stores. A file that does not is still
    /// selectable — naming it is the caller's judgement, not ours — but it is dimmed.
    pub is_media: bool,
}

pub struct Picker {
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub cursor: usize,
    pub chosen: BTreeSet<PathBuf>,
    pub show_hidden: bool,
    /// Set when a folder cannot be read, so the pane can say so rather than look empty.
    pub error: Option<String>,
}

impl Picker {
    pub fn open(start: &Path) -> Self {
        let mut picker = Self {
            cwd: start.to_path_buf(),
            entries: Vec::new(),
            cursor: 0,
            chosen: BTreeSet::new(),
            show_hidden: false,
            error: None,
        };
        picker.refresh();
        picker
    }

    /// Re-reads the current folder. Directories first, then everything else, each sorted
    /// the way a person reads rather than the way bytes sort.
    pub fn refresh(&mut self) {
        self.entries.clear();
        self.error = None;

        let read = match std::fs::read_dir(&self.cwd) {
            Ok(read) => read,
            Err(error) => {
                self.error = Some(format!("{}: {error}", self.cwd.display()));
                return;
            }
        };

        for entry in read.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if !self.show_hidden && name.starts_with('.') {
                continue;
            }
            let metadata = entry.metadata().ok();
            let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            self.entries.push(Entry {
                is_media: !is_dir && is_media(&path),
                size: metadata.as_ref().map(|m| m.len()).unwrap_or(0),
                path,
                name,
                is_dir,
            });
        }

        self.entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    pub fn go_to(&mut self, path: &Path) {
        self.cwd = path.to_path_buf();
        self.cursor = 0;
        self.refresh();
    }

    /// Steps into the highlighted folder. Returns false when the cursor is on a file,
    /// which the caller treats as a selection instead.
    pub fn descend(&mut self) -> bool {
        let Some(entry) = self.current() else {
            return false;
        };
        if !entry.is_dir {
            return false;
        }
        let target = entry.path.clone();
        self.go_to(&target);
        true
    }

    /// Steps out, leaving the cursor on the folder just left so going up and down again
    /// does not lose your place.
    pub fn ascend(&mut self) {
        let Some(parent) = self.cwd.parent().map(Path::to_path_buf) else {
            return;
        };
        let leaving = self.cwd.clone();
        self.go_to(&parent);
        if let Some(index) = self.entries.iter().position(|e| e.path == leaving) {
            self.cursor = index;
        }
    }

    pub fn toggle(&mut self) {
        let Some(entry) = self.current() else {
            return;
        };
        let path = entry.path.clone();
        if !self.chosen.remove(&path) {
            self.chosen.insert(path);
        }
    }

    /// Everything in this folder that looks like media. A quick way to take a whole
    /// folder without also taking the folder's subfolders.
    pub fn choose_all_media(&mut self) {
        for entry in &self.entries {
            if entry.is_media {
                self.chosen.insert(entry.path.clone());
            }
        }
    }

    pub fn is_chosen(&self, entry: &Entry) -> bool {
        self.chosen.contains(&entry.path)
    }

    /// What to upload: whatever has been ticked, or the thing under the cursor when
    /// nothing has.
    pub fn to_upload(&self) -> Vec<PathBuf> {
        if !self.chosen.is_empty() {
            return self.chosen.iter().cloned().collect();
        }
        self.current()
            .map(|e| vec![e.path.clone()])
            .into_iter()
            .flatten()
            .collect()
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.refresh();
    }
}

/// Files the `image` crate can decode for a preview. A HEIC or a RAW is perfectly
/// uploadable and simply cannot be shown here, which the pane says rather than hiding.
pub fn is_previewable(path: &Path) -> bool {
    const PREVIEWABLE: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "tif", "tiff", "bmp"];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| PREVIEWABLE.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Decoding a very large file to draw it a few centimetres across is not worth the memory.
pub const PREVIEW_SIZE_LIMIT: u64 = 96 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("subfolder")).unwrap();
        std::fs::write(dir.path().join("b.jpg"), b"x").unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        std::fs::write(dir.path().join(".hidden.jpg"), b"x").unwrap();
        dir
    }

    #[test]
    fn folders_come_first_and_hidden_files_are_left_out() {
        let dir = fixture();
        let picker = Picker::open(dir.path());
        let names: Vec<&str> = picker.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["subfolder", "a.txt", "b.jpg"]);
    }

    #[test]
    fn hidden_files_can_be_asked_for() {
        let dir = fixture();
        let mut picker = Picker::open(dir.path());
        picker.toggle_hidden();
        assert!(picker.entries.iter().any(|e| e.name == ".hidden.jpg"));
    }

    #[test]
    fn a_file_that_does_not_look_like_media_is_still_choosable() {
        let dir = fixture();
        let mut picker = Picker::open(dir.path());
        let text = picker
            .entries
            .iter()
            .position(|e| e.name == "a.txt")
            .unwrap();
        picker.cursor = text;
        assert!(!picker.current().unwrap().is_media);
        picker.toggle();
        assert_eq!(picker.to_upload().len(), 1);
    }

    #[test]
    fn with_nothing_ticked_the_cursor_is_the_selection() {
        let dir = fixture();
        let mut picker = Picker::open(dir.path());
        picker.cursor = picker
            .entries
            .iter()
            .position(|e| e.name == "b.jpg")
            .unwrap();
        assert_eq!(picker.to_upload(), vec![dir.path().join("b.jpg")]);
    }

    #[test]
    fn stepping_out_leaves_the_cursor_on_the_folder_just_left() {
        let dir = fixture();
        let mut picker = Picker::open(&dir.path().join("subfolder"));
        picker.ascend();
        assert_eq!(picker.cwd, dir.path());
        assert_eq!(picker.current().unwrap().name, "subfolder");
    }

    #[test]
    fn choosing_all_media_leaves_the_other_files_alone() {
        let dir = fixture();
        let mut picker = Picker::open(dir.path());
        picker.choose_all_media();
        assert_eq!(picker.chosen.len(), 1);
        assert!(picker.chosen.iter().all(|p| p.ends_with("b.jpg")));
    }

    #[test]
    fn only_what_can_be_decoded_is_offered_a_preview() {
        assert!(is_previewable(Path::new("a/b.JPG")));
        assert!(!is_previewable(Path::new("a/b.heic")));
        assert!(!is_previewable(Path::new("a/b")));
    }
}
