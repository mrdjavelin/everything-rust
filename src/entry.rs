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

impl FsEntry {
    pub fn info(&self) {
        match self.kind {
            EntryKind::File => match self.size {
                Some(size) => println!("[FILE] {} (Size: {size} bytes)", self.path.display()),
                None => println!("[FILE] {} (Size: unknown)", self.path.display()),
            },
            EntryKind::Directory => match self.item_count {
                Some(count) => println!("[DIR] {} (Items: {count})", self.path.display()),
                None => println!("[DIR] {} (Items: unknown)", self.path.display()),
            },
        }
    }
}
