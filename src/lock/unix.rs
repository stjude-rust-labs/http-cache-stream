//! Implementation of file locking for unix-based operating systems.

use std::io::Error;
use std::io::Result;
use std::os::fd::RawFd;
use std::os::unix::io::AsRawFd;

/// Locks a file with a shared lock.
///
/// This is a blocking operation.
pub fn lock_shared(file: &impl AsRawFd) -> Result<()> {
    flock(file.as_raw_fd(), libc::LOCK_SH)
}

/// Locks a file with an exclusive lock.
///
/// This is a blocking operation.
pub fn lock_exclusive(file: &impl AsRawFd) -> Result<()> {
    flock(file.as_raw_fd(), libc::LOCK_EX)
}

/// Locks a file with a shared lock.
///
/// If the lock cannot be acquired, an error is returned immediately without
/// blocking.
pub fn try_lock_shared(file: &impl AsRawFd) -> Result<()> {
    flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB)
}

/// Locks a file with an exclusive lock.
///
/// If the lock cannot be acquired, an error is returned immediately without
/// blocking.
pub fn try_lock_exclusive(file: &impl AsRawFd) -> Result<()> {
    flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
}

/// Unlocks the given file.
pub fn unlock(file: &impl AsRawFd) -> Result<()> {
    flock(file.as_raw_fd(), libc::LOCK_UN)
}

/// Determines if the error is a result of lock contention.
pub fn error_contended(err: &Error) -> bool {
    err.raw_os_error() == Some(libc::EWOULDBLOCK)
}

/// Determines if the error is the result of file locking being unsupported.
pub fn error_unsupported(err: &Error) -> bool {
    match err.raw_os_error() {
        // Unfortunately, depending on the target, these may or may not be the same.
        // For targets in which they are the same, the duplicate pattern causes a warning.
        #[allow(unreachable_patterns)]
        Some(libc::ENOTSUP | libc::EOPNOTSUPP) => true,
        Some(libc::ENOSYS) => true,
        _ => false,
    }
}

/// Wrapper around the `flock` system call.
fn flock(file: RawFd, flag: libc::c_int) -> Result<()> {
    let ret = unsafe { libc::flock(file, flag) };
    if ret < 0 {
        Err(Error::last_os_error())
    } else {
        Ok(())
    }
}
