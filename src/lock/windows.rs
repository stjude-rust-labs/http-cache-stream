//! Implementation of file locking for Windows.

use std::io::Error;
use std::io::Result;
use std::mem;
use std::os::windows::io::AsRawHandle;
use std::os::windows::io::RawHandle;

use windows_sys::Win32::Foundation::ERROR_INVALID_FUNCTION;
use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::LOCKFILE_EXCLUSIVE_LOCK;
use windows_sys::Win32::Storage::FileSystem::LOCKFILE_FAIL_IMMEDIATELY;
use windows_sys::Win32::Storage::FileSystem::LockFileEx;
use windows_sys::Win32::Storage::FileSystem::UnlockFile;

/// Locks a file with a shared lock.
///
/// This is a blocking operation.
pub fn lock_shared(file: &impl AsRawHandle) -> Result<()> {
    flock(file.as_raw_handle(), 0)
}

/// Locks a file with an exclusive lock.
///
/// This is a blocking operation.
pub fn lock_exclusive(file: &impl AsRawHandle) -> Result<()> {
    flock(file.as_raw_handle(), LOCKFILE_EXCLUSIVE_LOCK)
}

/// Locks a file with a shared lock.
///
/// If the lock cannot be acquired, an error is returned immediately without
/// blocking.
pub fn try_lock_shared(file: &impl AsRawHandle) -> Result<()> {
    flock(file.as_raw_handle(), LOCKFILE_FAIL_IMMEDIATELY)
}

/// Locks a file with an exclusive lock.
///
/// If the lock cannot be acquired, an error is returned immediately without
/// blocking.
pub fn try_lock_exclusive(file: &impl AsRawHandle) -> Result<()> {
    flock(
        file.as_raw_handle(),
        LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
    )
}

/// Unlocks the given file.
pub fn unlock(file: &impl AsRawHandle) -> Result<()> {
    unsafe {
        let ret = UnlockFile(file.as_raw_handle() as HANDLE, 0, 0, !0, !0);
        if ret == 0 {
            Err(Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// Determines if the error is a result of lock contention.
pub fn error_contended(err: &Error) -> bool {
    err.raw_os_error()
        .map_or(false, |x| x == ERROR_LOCK_VIOLATION as i32)
}

/// Determines if the error is the result of file locking being unsupported.
pub fn error_unsupported(err: &Error) -> bool {
    err.raw_os_error()
        .map_or(false, |x| x == ERROR_INVALID_FUNCTION as i32)
}

/// Downgrades a lock
pub fn downgrade(file: &impl AsRawFd) -> Result<()> {
    // From https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-lockfileex
    //
    // Exclusive locks cannot overlap an existing locked region of a file. Shared
    // locks can overlap a locked region provided locks held on that region are
    // shared locks. A shared lock can overlap an exclusive lock if both locks were
    // created using the same file handle. When a shared lock overlaps an exclusive
    // lock, the only possible access is a read by the owner of the locks. If the
    // same range is locked with an exclusive and a shared lock, two unlock
    // operations are necessary to unlock the region; the first unlock operation
    // unlocks the exclusive lock, the second unlock operation unlocks the shared
    // lock.

    // Lock the file again with a shared lock
    lock_shared(file)?;

    // Unlock the exclusive lock
    unlock(file)
}

/// Wrapper around the `LockFileEx` system call.
fn flock(handle: RawHandle, flags: u32) -> Result<()> {
    unsafe {
        let mut overlapped = mem::zeroed();
        let ret = LockFileEx(handle as HANDLE, flags, 0, !0, !0, &mut overlapped);
        if ret == 0 {
            Err(Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
