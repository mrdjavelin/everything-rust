use std::path::PathBuf;

#[cfg(windows)]
pub fn get_scan_roots() -> Vec<PathBuf> {
    ('A'..='Z')
        .map(|letter| PathBuf::from(format!("{letter}:\\")))
        .filter(|path| path.is_dir())
        .collect()
}

#[cfg(not(windows))]
pub fn get_scan_roots() -> Vec<PathBuf> {
    vec![PathBuf::from("/")]
}
