use std::{fs, path::Path};

use crate::entry::{EntryKind, FsEntry};

const BATCH_SIZE: usize = 512;

pub struct ScanSummary {
    pub discovered: usize,
    pub skipped: usize,
    pub complete: bool,
}

pub fn scan_root<E>(
    root: &Path,
    mut save_batch: impl FnMut(&[FsEntry]) -> Result<(), E>,
) -> Result<ScanSummary, E> {
    let mut summary = ScanSummary {
        discovered: 0,
        skipped: 0,
        complete: true,
    };
    let mut pending = Vec::with_capacity(BATCH_SIZE);
    let mut directories = vec![root.to_path_buf()];

    while let Some(directory) = directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                summary.skipped += 1;
                summary.complete = false;
                continue;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    summary.skipped += 1;
                    summary.complete = false;
                    continue;
                }
            };

            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => {
                    summary.skipped += 1;
                    summary.complete = false;
                    continue;
                }
            };

            let is_directory = file_type.is_dir();
            if !is_directory && !file_type.is_file() {
                continue;
            }

            let name = match entry.file_name().into_string() {
                Ok(name) => name,
                Err(_) => {
                    summary.skipped += 1;
                    summary.complete = false;
                    continue;
                }
            };
            let path = entry.path();
            let (kind, size) = if is_directory {
                directories.push(path.clone());
                (EntryKind::Directory, None)
            } else {
                match entry.metadata() {
                    Ok(metadata) => (EntryKind::File, Some(metadata.len())),
                    Err(_) => {
                        summary.skipped += 1;
                        summary.complete = false;
                        continue;
                    }
                }
            };

            pending.push(FsEntry {
                name,
                path,
                kind,
                size,
                item_count: None,
            });
            summary.discovered += 1;

            if pending.len() == BATCH_SIZE {
                save_batch(&pending)?;
                pending.clear();
            }
        }
    }

    if !pending.is_empty() {
        save_batch(&pending)?;
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn scan_emits_bounded_batches_without_collecting_the_tree() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("everything-rust-scan-{suffix}"));
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("top.txt"), b"top").unwrap();
        fs::write(nested.join("child.txt"), b"child").unwrap();

        let mut batch_count = 0;
        let mut discovered = Vec::new();
        let result = scan_root(&root, |batch| {
            assert!(batch.len() <= BATCH_SIZE);
            batch_count += 1;
            discovered.extend(batch.iter().map(|entry| entry.name.clone()));
            Ok::<_, ()>(())
        })
        .unwrap();

        fs::remove_dir_all(&root).unwrap();
        assert!(result.complete);
        assert_eq!(result.discovered, 3);
        assert_eq!(batch_count, 1);
        assert!(discovered.iter().any(|name| name == "nested"));
        assert!(discovered.iter().any(|name| name == "top.txt"));
        assert!(discovered.iter().any(|name| name == "child.txt"));
    }

    #[test]
    fn inaccessible_root_is_reported_as_incomplete() {
        let summary =
            scan_root::<()>(Path::new("missing-directory-for-indexer-test"), |_| Ok(())).unwrap();
        assert!(!summary.complete);
        assert_eq!(summary.skipped, 1);
    }
}
