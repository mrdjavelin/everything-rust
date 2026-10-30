use std::path::PathBuf;

#[derive(Copy, Clone, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
}

pub struct FsEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub item_count: Option<usize>,
}
