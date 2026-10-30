use std::{io, path::Path, process::Command};

pub fn open_path(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("explorer.exe");
        command.arg(path);
        command
    };

    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };

    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };

    #[cfg(any(windows, unix))]
    {
        command.spawn().map(|_| ())
    }

    #[cfg(not(any(windows, unix)))]
    {
        let _ = path;
        Err(io::Error::other(
            "Opening files is not supported on this platform",
        ))
    }
}
