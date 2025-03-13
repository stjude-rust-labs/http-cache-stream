//! Implementation of the async runtime integration.

#[cfg(not(any(feature = "tokio", feature = "async-std")))]
compile_error!("either feature `tokio` or `async-std` must be enabled");

#[cfg(all(feature = "tokio", feature = "async-std"))]
compile_error!("features `tokio` and `async-std` are mutually exclusive");

cfg_if::cfg_if! {
    if #[cfg(feature = "tokio")] {
        pub use tokio::fs::File;
        pub use tokio::task::spawn_blocking;

        /// Helper for difference in `spawn_blocking` signatures.
        pub fn unwrap_task_output<T>(result: Result<T, tokio::task::JoinError>) -> Option<T> {
            result.ok()
        }
    } else if #[cfg(feature = "async-std")] {
        pub use async_std::fs::File;
        pub use async_std::task::spawn_blocking;

        /// Helper for difference in `spawn_blocking` signatures.
        pub fn unwrap_task_output<T>(result: T) -> Option<T> {
            Some(result)
        }
    }
}
