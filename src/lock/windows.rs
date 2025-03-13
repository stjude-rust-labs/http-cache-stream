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
