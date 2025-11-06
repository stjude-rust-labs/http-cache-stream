//! Implementation of cache storage.

use std::path::PathBuf;

use anyhow::Result;
use http::Response;
use http::response::Parts;
use http_body::Body;
use http_cache_semantics::CachePolicy;

use crate::body::CacheBody;

mod default;

pub use default::*;

/// Represents a response from storage.
pub struct StoredResponse<B: Body> {
    /// The cached response.
    pub response: Response<CacheBody<B>>,
    /// The current cache policy.
    pub policy: CachePolicy,
    /// The response content digest.
    pub digest: String,
}

/// A trait implemented on cache storage.
///
/// Cache keys are strings of hexadecimal characters.
pub trait CacheStorage: Send + Sync + 'static {
    /// Gets a previously stored response for the given response key.
    ///
    /// Returns `Ok(None)` if a response does not exist in the storage for the
    /// given response key.
    fn get<B: Body + Send>(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<StoredResponse<B>>>> + Send;

    /// Puts a response with an existing content digest into the storage for the
    /// given response key.
    ///
    /// The provided content digest must come from a previous call to
    /// [`CacheStorage::get`].
    fn put(
        &self,
        key: &str,
        parts: &Parts,
        policy: &CachePolicy,
        digest: &str,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Stores a new response body in the cache.
    ///
    /// Returns a response with a body streaming to the cache.
    fn store<B: Body + Send>(
        &self,
        key: String,
        parts: Parts,
        body: B,
        policy: CachePolicy,
    ) -> impl Future<Output = Result<Response<CacheBody<B>>>> + Send;

    /// Deletes a previously cached response for the given response key.
    ///
    /// Deleting an unknown key is not considered an error.
    fn delete(&self, key: &str) -> impl Future<Output = Result<()>> + Send;

    /// Gets the path to a response body for the given content digest.
    ///
    /// This method does not verify the content's existence.
    fn body_path(&self, digest: &str) -> PathBuf;
}
