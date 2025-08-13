//! An implementation of an HTTP response cache that uses
//! `http-cache-semantics`.
//!
//! Inspired by the `http-cache` crate.
//!
//! This crate's implementation differs significantly from `http-cache` in the
//! following ways:
//!
//! * Response bodies are not fully read into memory.
//! * Response bodies may be linked directly from cache storage.
//! * Uses its own lightweight storage implementation (instead of `cacache`), by
//!   default.
//! * This library is not nearly as customizable as `http-cache`.
//!
//! By default, this crate uses `tokio` as its async runtime.
//!
//! Enable the `smol` feature for using the `smol` runtime instead.

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
#![warn(rust_2021_compatibility)]
#![warn(clippy::missing_docs_in_private_items)]
#![warn(rustdoc::broken_intra_doc_links)]

mod body;
mod cache;
pub(crate) mod runtime;
pub mod storage;

pub use body::*;
pub use cache::*;
// Re-export the http crate.
pub use http;
// Re-export the http-body crate.
pub use http_body;
// Re-export the semantics crate
pub use http_cache_semantics as semantics;
