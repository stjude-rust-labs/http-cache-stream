//! Implementation of the async runtime integration.

#[cfg(not(any(feature = "tokio", feature = "smol")))]
compile_error!("either feature `tokio` or `smol` must be enabled");

#[cfg(all(feature = "tokio", feature = "smol"))]
compile_error!("features `tokio` and `smol` are mutually exclusive");

cfg_if::cfg_if! {
    if #[cfg(feature = "tokio")] {
        pub use tokio::fs::File;
        pub use tokio::task::spawn_blocking;

        /// Helper for difference in `spawn_blocking` signatures.
        pub fn unwrap_task_output<T>(result: Result<T, tokio::task::JoinError>) -> Option<T> {
            result.ok()
        }
    } else if #[cfg(feature = "smol")] {
        pub use smol::fs::File;
        pub use smol::unblock as spawn_blocking;

        /// Helper for difference in `spawn_blocking` signatures.
        pub fn unwrap_task_output<T>(result: T) -> Option<T> {
            Some(result)
        }
    }
}
