//! Cross-process safety for the small files every process on one data directory shares
//! (the identity, the peer store, the launch tokens, the console pins): an advisory lock
//! on a sidecar file, and temp names no two writers share.
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Holds the exclusive advisory lock on `<path>.lock` until dropped. The lock is on a
/// sidecar, not on the data file: that file is replaced by rename, which would leave a
/// second process locking the old inode.
#[derive(Debug)]
pub struct FileLock {
    _file: File,
}

fn sidecar(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// Blocks until this process holds the lock for `path` (creating the directory and the
/// sidecar as needed). Take it around a whole read-modify-write, not just the write.
pub fn lock(path: &Path) -> io::Result<FileLock> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(sidecar(path))?;
    file.lock()?;
    Ok(FileLock { _file: file })
}

/// A temp path beside `path` that only this call will ever use: pid plus a counter, so
/// two threads or two processes writing the same file never share (or truncate) one.
pub fn unique_tmp(path: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
}

/// Removes its path on drop unless disarmed: a failed write leaves no temp file.
pub struct TmpGuard(Option<PathBuf>);

impl TmpGuard {
    pub fn new(p: PathBuf) -> Self {
        Self(Some(p))
    }
    pub fn path(&self) -> &Path {
        self.0.as_deref().expect("armed")
    }
    pub fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for TmpGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}
