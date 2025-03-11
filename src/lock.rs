//! Implementation of advisory file locking.
//!
//! An advisory file lock is used to coordinate access to a file across threads
//! and processes.
//!
//! The locking is "best-effort" and will transparently succeed on file systems
//! that do not support advisory locks.
use std::fmt;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::ops::Deref;
use std::ops::DerefMut;
use std::path::Path;

#[cfg(unix)]
pub(crate) mod unix;

use tracing::debug;
#[cfg(unix)]
pub(crate) use unix as sys;

#[cfg(windows)]
pub(crate) mod windows;

#[cfg(windows)]
pub(crate) use windows as sys;

use crate::runtime;

/// Represents a locked file.
#[derive(Debug)]
pub struct LockedFile(File);

impl LockedFile {
    /// Downgrades an exclusive lock to a shared lock.
    pub fn downgrade(&self) -> io::Result<()> {
        sys::downgrade(&self.0)
    }
}

impl Deref for LockedFile {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for LockedFile {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for LockedFile {
    fn drop(&mut self) {
        let _ = sys::unlock(&self.0);
    }
}

/// An extension trait for [`OpenOptions`].
///
/// When the `file_lock` Rust feature stabilizes, this implementation will be
/// removed.
pub trait OpenOptionsExt {
    /// Attempts to open a file with a shared lock.
    ///
    /// Returns the locked file upon success.
    ///
    /// If the lock cannot be immediately acquired, `Ok(None)` is returned.
    fn try_open_shared(&self, path: &Path) -> io::Result<Option<LockedFile>>;

    /// Attempts to open a file with a shared lock.
    ///
    /// Returns the locked file upon success.
    fn open_shared(&self, path: &Path) -> impl Future<Output = io::Result<LockedFile>> + Send;

    /// Attempts to open a file with an exclusive lock.
    ///
    /// Returns the locked file upon success.
    ///
    /// If the lock cannot be immediately acquired, `Ok(None)` is returned.
    fn try_open_exclusive(&self, path: &Path) -> io::Result<Option<LockedFile>>;

    /// Attempts to open a file with an exclusive lock.
    ///
    /// Returns the locked file upon success.
    fn open_exclusive(&self, path: &Path) -> impl Future<Output = io::Result<LockedFile>> + Send;
}

impl OpenOptionsExt for OpenOptions {
    fn try_open_shared(&self, path: &Path) -> io::Result<Option<LockedFile>> {
        let file = self.open(path)?;

        if try_lock(&file, Access::Shared)? {
            Ok(Some(LockedFile(file)))
        } else {
            Ok(None)
        }
    }

    async fn open_shared(&self, path: &Path) -> io::Result<LockedFile> {
        lock(self.open(path)?, path, Access::Shared).await
    }

    fn try_open_exclusive(&self, path: &Path) -> io::Result<Option<LockedFile>> {
        let file = self.open(path)?;

        if try_lock(&file, Access::Exclusive)? {
            Ok(Some(LockedFile(file)))
        } else {
            Ok(None)
        }
    }

    async fn open_exclusive(&self, path: &Path) -> io::Result<LockedFile> {
        lock(self.open(path)?, path, Access::Exclusive).await
    }
}

/// Represents how a resource is accessed.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Access {
    /// Access to the resource is shared.
    Shared,
    /// Access to the resource is exclusive.
    Exclusive,
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shared => write!(f, "shared"),
            Self::Exclusive => write!(f, "exclusive"),
        }
    }
}

/// Attempts to take a lock on a file.
///
/// Returns `Ok(true)` if the file was locked or `Ok(false)` if the lock was
/// contended.
fn try_lock(file: &File, access: Access) -> io::Result<bool> {
    // We always try the lock on the first attempt so that we later wait for the
    // lock using a blocking task
    let res = match access {
        Access::Shared => sys::try_lock_shared(file),
        Access::Exclusive => sys::try_lock_exclusive(file),
    };

    match res {
        Ok(_) => Ok(true),
        Err(e) if sys::error_unsupported(&e) => Ok(true),
        Err(e) if sys::error_contended(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Takes a lock on a file.
async fn lock(file: File, path: &Path, access: Access) -> io::Result<LockedFile> {
    // First try to obtain the lock so that we don't have to do a blocking spawn
    if try_lock(&file, access)? {
        return Ok(LockedFile(file));
    }

    // Otherwise, we'll need to block
    debug!(
        "waiting to acquire {access} lock on file `{path}`",
        path = path.display()
    );
    match runtime::unwrap_task_output(
        runtime::spawn_blocking(move || {
            let res = match access {
                Access::Shared => sys::lock_shared(&file),
                Access::Exclusive => sys::lock_exclusive(&file),
            };

            match res {
                Ok(_) => Ok(LockedFile(file)),
                Err(e) if sys::error_unsupported(&e) => Ok(LockedFile(file)),
                Err(e) => Err(e),
            }
        })
        .await,
    ) {
        Some(res) => res,
        None => Err(io::Error::new(
            io::ErrorKind::Other,
            "failed to wait for file lock",
        )),
    }
}
