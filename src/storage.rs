//! Implementation of cache storage.

use std::path::PathBuf;

use anyhow::Result;
use http::Response;
use http::response::Parts;
use http_cache_semantics::CachePolicy;

use crate::HttpBody;
use crate::body::Body;

mod default;

pub use default::*;

/// A trait implemented on cache storage.
///
/// Cache keys are strings of hexadecimal characters.
pub trait CacheStorage: Send + Sync + 'static {
    /// Gets a response and cache policy for the given key.
    ///
    /// Returns `Ok(None)` if a response does not exist in the storage for the
    /// given key.
    fn get<B: HttpBody>(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<(Response<Body<B>>, CachePolicy)>>> + Send;

    /// Puts a response, cache policy, and optionally a body into the storage
    /// for the given key.
    ///
    /// Returns the body from the cache on success.
    fn put<B: HttpBody>(
        &self,
        key: &str,
        parts: &Parts,
        policy: &CachePolicy,
        body: Option<B>,
    ) -> impl Future<Output = Result<Body<B>>> + Send;

    /// Deletes a previously cached response.
    ///
    /// Does not return error for unrecognized cache keys.
    fn delete(&self, key: &str) -> impl Future<Output = Result<()>> + Send;

    /// Gets the path to a body file for the given key.
    ///
    /// This method does not verify the body file's existence.
    fn body_path(&self, key: &str) -> PathBuf;
}
