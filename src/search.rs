use crate::entry::FsEntry;

pub fn search<'a>(letters: &str, files: &'a [FsEntry]) -> Vec<&'a FsEntry> {
    let letters_lower = letters.to_lowercase();

    files
        .iter()
        .filter(|file| file.name.to_lowercase().contains(&letters_lower))
        .collect()
}
