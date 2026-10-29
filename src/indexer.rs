use std::{fs, path::Path};

use crate::entry::{EntryKind, FsEntry};

pub fn read_dir(path: &Path, files: &mut Vec<FsEntry>) {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("Cannot read {}: {err}", path.display());
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                eprintln!("{err}");
                continue;
            }
        };

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                eprintln!("{err}");
                continue;
            }
        };

        let is_directory = file_type.is_dir();

        if !is_directory && !file_type.is_file() {
            continue;
        }

        if let Some(item) = process_entry(entry, is_directory) {
            if is_directory {
                read_dir(&item.path, files);
            }
            files.push(item);
        }
    }
}

fn process_entry(entry: fs::DirEntry, is_directory: bool) -> Option<FsEntry> {
    let name = match entry.file_name().into_string() {
        Ok(name) => name,
        Err(err) => {
            eprintln!("Invalid UTF-8 filename: {}", err.to_string_lossy());
            return None;
        }
    };

    let path = entry.path();

    if is_directory {
        let item_count = fs::read_dir(&path).map(|entries| entries.count()).ok();

        Some(FsEntry {
            name,
            path,
            kind: EntryKind::Directory,
            size: None,
            item_count,
        })
    } else {
        let size = match entry.metadata() {
            Ok(metadata) => metadata.len(),
            Err(err) => {
                eprintln!("{err}");
                return None;
            }
        };

        Some(FsEntry {
            name,
            path,
            kind: EntryKind::File,
            size: Some(size),
            item_count: None,
        })
    }
}
