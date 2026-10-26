//! One Opal at a time. A second start tells the first to close (SIGUSR1) and
//! exits, so the screenshot key toggles the overlay.

use std::{fs::OpenOptions, io::{Read, Write}, os::fd::AsRawFd, path::PathBuf};

pub struct Guard {
    _file: std::fs::File,
}

fn path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("opal.lock")
}

/// `Some(guard)` when we are the only instance. `None` when another one is
/// running; it has been asked to close.
pub fn acquire() -> Option<Guard> {
    let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path()).ok()?;
    let fd = file.as_raw_fd();
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        let _ = file.set_len(0);
        let _ = write!(file, "{}", std::process::id());
        return Some(Guard { _file: file });
    }
    let mut pid = String::new();
    let _ = file.read_to_string(&mut pid);
    if let Ok(pid) = pid.trim().parse::<i32>() {
        if pid > 1 {
            unsafe { libc::kill(pid, libc::SIGUSR1) };
        }
    }
    None
}
