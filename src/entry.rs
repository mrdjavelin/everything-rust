use std::path::PathBuf;

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

impl FsEntry {
    pub fn info(&self) {
        match self.kind {
            EntryKind::File => {
                println!(
                    "[FILE] {} (Size: {} bytes)",
                    self.path.display(),
                    self.size.unwrap_or(0)
                );
            }
            EntryKind::Directory => {
                println!(
                    "[DIR] {} (Items: {})",
                    self.path.display(),
                    self.item_count.unwrap_or(0)
                );
            }
        }
    }
}
