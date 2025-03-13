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

/// Represents a response from storage.
pub struct StoredResponse<B: HttpBody> {
    /// The cached response.
    pub response: Response<Body<B>>,
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
    fn get<B: HttpBody>(
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

    /// Puts a response with supplied body into the storage for the given
    /// response key.
    ///
    /// Returns the body from the cache along with its digest upon success.
    fn put_with_body<B: HttpBody>(
        &self,
        key: &str,
        parts: &Parts,
        policy: &CachePolicy,
        body: B,
    ) -> impl Future<Output = Result<(Body<B>, String)>> + Send;

    /// Deletes a previously cached response for the given response key.
    ///
    /// Deleting an unknown key is not considered an error.
    fn delete(&self, key: &str) -> impl Future<Output = Result<()>> + Send;

    /// Gets the path to a response body for the given content digest.
    ///
    /// This method does not verify the content's existence.
    fn body_path(&self, digest: &str) -> PathBuf;
}
