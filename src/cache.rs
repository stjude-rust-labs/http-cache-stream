//! Implementation of the HTTP cache.

use std::fmt;
use std::io;
use std::time::SystemTime;

use anyhow::Result;
use anyhow::bail;
use bytes::Bytes;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::Response;
use http::StatusCode;
use http::Uri;
use http::Version;
use http::header::CACHE_CONTROL;
use http::uri::Authority;
use http_cache_semantics::AfterResponse;
use http_cache_semantics::BeforeRequest;
use http_cache_semantics::CacheOptions;
use http_cache_semantics::CachePolicy;
use sha2::Digest;
use sha2::Sha256;
use tracing::debug;

use crate::body::Body;
use crate::storage::CacheStorage;

/// The name of the `x-cache-lookup` custom header.
///
/// Value will be `HIT` if a response existed in cache, `MISS` if not.
pub const X_CACHE_LOOKUP: &str = "x-cache-lookup";

/// The name of the `x-cache` custom header.
///
/// Value will be `HIT` if a response was served from the cache, `MISS` if not.
pub const X_CACHE: &str = "x-cache";

/// The name of the `x-cache-storage-key` custom header.
///
/// The value will be the calculated storage key for the response.
///
/// This can be used to link a response body directly from cache storage rather
/// than reading the response body.
///
/// This header is only present when the body is coming directly from the cache.
pub const X_CACHE_STORAGE_KEY: &str = "x-cache-storage-key";

/// Gets the storage key for a request.
fn storage_key(method: &Method, uri: &Uri) -> String {
    let mut hasher = Sha256::new();
    hasher.update(method.as_str());
    hasher.update(":");

    if let Some(scheme) = uri.scheme_str() {
        hasher.update(scheme);
    }

    hasher.update("://");
    if let Some(authority) = uri.authority() {
        hasher.update(authority.as_str());
    }

    hasher.update(uri.path());

    if let Some(query) = uri.query() {
        hasher.update(query);
    }

    let bytes = hasher.finalize();
    hex::encode(bytes)
}

/// Represents a basic cache lookup status.
///
/// Used in the custom header `x-cache-lookup`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum CacheLookupStatus {
    /// A response exists in the cache.
    Hit,
    /// A response does not exist in the cache.
    Miss,
}

impl fmt::Display for CacheLookupStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hit => write!(f, "HIT"),
            Self::Miss => write!(f, "MISS"),
        }
    }
}

/// Represents a cache status.
///
/// Used in the custom header `x-cache`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum CacheStatus {
    /// The response was served from the cache.
    Hit,
    /// The response was not served from the cache.
    Miss,
}

impl fmt::Display for CacheStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hit => write!(f, "HIT"),
            Self::Miss => write!(f, "MISS"),
        }
    }
}

/// An extension trait for [`Response`].
trait ResponseExt {
    /// Adds a warning header to the response.
    fn add_warning(&mut self, uri: &Uri, code: usize, message: &str);

    /// Checks if the Cache-Control header contains the must-revalidate
    /// directive.
    fn must_revalidate(&self) -> bool;

    /// Extends the request's headers with those from the given header map.
    ///
    /// Existing matching headers will be replaced.
    fn extend_headers(&mut self, headers: HeaderMap);

    /// Sets the cache status headers of the response.
    fn set_cache_status(&mut self, lookup: CacheLookupStatus, status: CacheStatus);

    /// Sets the storage key header of the response.
    fn set_storage_key(&mut self, key: &str);
}

impl<B> ResponseExt for Response<B> {
    fn add_warning(&mut self, url: &Uri, code: usize, message: &str) {
        // warning    = "warning" ":" 1#warning-value
        // warning-value = warn-code SP warn-agent SP warn-text [SP warn-date]
        // warn-code  = 3DIGIT
        // warn-agent = ( host [ ":" port ] ) | pseudonym
        //                 ; the name or pseudonym of the server adding
        //                 ; the warning header, for use in debugging
        // warn-text  = quoted-string
        // warn-date  = <"> HTTP-date <">
        // (https://tools.ietf.org/html/rfc2616#section-14.46)
        self.headers_mut().insert(
            "warning",
            HeaderValue::from_str(&format!(
                "{} {} {:?} \"{}\"",
                code,
                url.host().expect("URL should be valid"),
                message,
                httpdate::fmt_http_date(SystemTime::now())
            ))
            .expect("value should be valid"),
        );
    }

    fn must_revalidate(&self) -> bool {
        self.headers()
            .get(CACHE_CONTROL.as_str())
            .is_some_and(|val| {
                val.to_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains("must-revalidate")
            })
    }

    fn extend_headers(&mut self, headers: HeaderMap) {
        self.headers_mut().extend(headers);
    }

    fn set_cache_status(&mut self, lookup: CacheLookupStatus, status: CacheStatus) {
        self.headers_mut().insert(
            X_CACHE_LOOKUP,
            lookup.to_string().parse().expect("value should parse"),
        );
        self.headers_mut().insert(
            X_CACHE,
            status.to_string().parse().expect("value should parse"),
        );
    }

    fn set_storage_key(&mut self, key: &str) {
        self.headers_mut().insert(
            X_CACHE_STORAGE_KEY,
            key.parse().expect("value should parse"),
        );
    }
}

/// Represents the supported HTTP body trait from middleware integrations.
pub trait HttpBody: http_body::Body<Data = Bytes, Error = io::Error> + Send {}

/// An abstraction of an HTTP request.
///
/// This trait is used in HTTP middleware integrations to abstract the request
/// type and sending the request upstream.
pub trait Request<B: HttpBody>: Send {
    /// Gets the request's version.
    fn version(&self) -> Version;

    /// Gets the request's method.
    fn method(&self) -> &Method;

    /// Gets the request's URI.
    fn uri(&self) -> &Uri;

    /// Gets the request's headers.
    fn headers(&self) -> &HeaderMap;

    /// Sends the request to upstream and gets the response.
    ///
    /// If `headers` is `Some`, the supplied headers should override any
    /// matching headers in the original request.
    fn send(self, headers: Option<HeaderMap>) -> impl Future<Output = Result<Response<B>>> + Send;
}

/// Provides an implementation of `RequestLike` for `http-cache-semantics`.
struct RequestLike {
    /// The request method.
    method: Method,
    /// The request URI.
    uri: Uri,
    /// The request headers.
    headers: HeaderMap,
}

impl RequestLike {
    /// Constructs a new `RequestLike` for the given request.
    fn new<R: Request<B>, B: HttpBody>(request: &R) -> Self {
        // Unfortunate we have to clone the header map here
        Self {
            method: request.method().clone(),
            uri: request.uri().clone(),
            headers: request.headers().clone(),
        }
    }
}

impl http_cache_semantics::RequestLike for RequestLike {
    fn uri(&self) -> Uri {
        // Note: URI is cheaply cloned
        self.uri.clone()
    }

    fn is_same_uri(&self, other: &Uri) -> bool {
        self.uri.eq(other)
    }

    fn method(&self) -> &Method {
        &self.method
    }

    fn headers(&self) -> &HeaderMap {
        &self.headers
    }
}

/// Implement a HTTP cache.
pub struct Cache<S> {
    /// The cache storage.
    storage: S,
    /// The cache options to use.
    options: CacheOptions,
}

impl<S> Cache<S>
where
    S: CacheStorage,
{
    /// Construct a new cache with the given storage.
    ///
    /// Defaults to a private cache.
    pub fn new(storage: S) -> Self {
        Self {
            storage,
            // Default to a private cache
            options: CacheOptions {
                shared: false,
                ..Default::default()
            },
        }
    }

    /// Construct a new cache with the given storage and options.
    pub fn new_with_options(storage: S, options: CacheOptions) -> Self {
        Self { storage, options }
    }

    /// Gets the storage used by the cache.
    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// Sends a HTTP request through the cache.
    ///
    /// If a previous response is cached and not stale, the request is not sent
    /// upstream and the cached response is returned.
    ///
    /// If a previous response is cached and is stale, the response is
    /// revalidated, the cache is updated, and the cached response returned.
    ///
    /// If a previous response is not in the cache, the request is sent upstream
    /// and the response is cached, if it is cacheable.
    pub async fn send<B: HttpBody>(&self, request: impl Request<B>) -> Result<Response<Body<B>>> {
        let method = request.method();
        let uri = request.uri();

        let key = storage_key(method, uri);
        if matches!(*method, Method::GET | Method::HEAD) {
            match self.storage.get(&key).await {
                Ok(Some((response, policy))) => {
                    debug!(
                        method = method.as_str(),
                        scheme = uri.scheme_str(),
                        authority = uri.authority().map(Authority::as_str),
                        path = uri.path(),
                        key,
                        "cache hit"
                    );
                    return self
                        .conditional_send_upstream(key, request, response, policy)
                        .await;
                }
                Ok(None) => {
                    debug!(
                        method = method.as_str(),
                        scheme = uri.scheme_str(),
                        authority = uri.authority().map(Authority::as_str),
                        path = uri.path(),
                        key,
                        "cache miss"
                    );
                }
                Err(e) => {
                    debug!(
                        method = method.as_str(),
                        scheme = uri.scheme_str(),
                        authority = uri.authority().map(Authority::as_str),
                        path = uri.path(),
                        key,
                        error = format!("{e:?}"),
                        "failed to get response from storage; treating as not cached"
                    );

                    // Treat as a miss
                }
            }
        }

        self.send_upstream(key, request, CacheLookupStatus::Miss)
            .await
    }

    /// Sends the original request upstream.
    ///
    /// Caches the response if the response is cacheable.
    async fn send_upstream<B: HttpBody>(
        &self,
        key: String,
        request: impl Request<B>,
        lookup_status: CacheLookupStatus,
    ) -> Result<Response<Body<B>>> {
        let request_like: RequestLike = RequestLike::new(&request);

        let mut response = request.send(None).await?;
        let policy =
            CachePolicy::new_options(&request_like, &response, SystemTime::now(), self.options);

        response.set_cache_status(lookup_status, CacheStatus::Miss);

        if matches!(request_like.method, Method::GET | Method::HEAD)
            && response.status() == StatusCode::OK
            && policy.is_storable()
        {
            let (parts, body) = response.into_parts();
            return match self.storage.put(&key, &parts, &policy, Some(body)).await {
                Ok(body) => {
                    debug!(
                        method = request_like.method.as_str(),
                        scheme = request_like.uri.scheme_str(),
                        authority = request_like.uri.authority().map(Authority::as_str),
                        path = request_like.uri.path(),
                        key,
                        "cache storage updated successfully"
                    );

                    let mut response = Response::from_parts(parts, body);
                    response.set_storage_key(&key);
                    Ok(response)
                }
                Err(e) => {
                    debug!(
                        method = request_like.method.as_str(),
                        scheme = request_like.uri.scheme_str(),
                        authority = request_like.uri.authority().map(Authority::as_str),
                        path = request_like.uri.path(),
                        key,
                        error = format!("{e:?}"),
                        "failed to put response into storage"
                    );
                    Err(e)
                }
            };
        }

        debug!(
            method = request_like.method.as_str(),
            scheme = request_like.uri.scheme_str(),
            authority = request_like.uri.authority().map(Authority::as_str),
            path = request_like.uri.path(),
            key,
            "response is not cacheable"
        );

        if !request_like.method.is_safe() {
            // If the request is not safe, assume the resource has been modified and delete
            // any cached responses we may have for HEAD/GET
            for method in [Method::HEAD, Method::GET] {
                let key = storage_key(&method, &request_like.uri);
                if let Err(e) = self.storage.delete(&key).await {
                    debug!(
                        method = method.as_str(),
                        scheme = request_like.uri.scheme_str(),
                        authority = request_like.uri.authority().map(Authority::as_str),
                        path = request_like.uri.path(),
                        key,
                        error = format!("{e:?}"),
                        "failed to put response into storage"
                    );
                }
            }
        }

        Ok(response.map(|body| Body::from_upstream(body)))
    }

    /// Performs a conditional send to upstream.
    ///
    /// If a cached request is still fresh, it is returned.
    ///
    /// If a cached request is stale, an attempt is made to revalidate it.
    async fn conditional_send_upstream<B: HttpBody>(
        &self,
        key: String,
        request: impl Request<B>,
        mut cached_response: Response<Body<B>>,
        cached_policy: CachePolicy,
    ) -> Result<Response<Body<B>>> {
        let request_like = RequestLike::new(&request);

        let headers = match cached_policy.before_request(&request_like, SystemTime::now()) {
            BeforeRequest::Fresh(parts) => {
                // The cached response is still fresh, return it
                debug!(
                    method = request_like.method.as_str(),
                    scheme = request_like.uri.scheme_str(),
                    authority = request_like.uri.authority().map(Authority::as_str),
                    path = request_like.uri.path(),
                    key,
                    "response is still fresh: responding with body from storage"
                );

                cached_response.extend_headers(parts.headers);
                cached_response.set_cache_status(CacheLookupStatus::Hit, CacheStatus::Hit);
                cached_response.set_storage_key(&key);
                return Ok(cached_response);
            }
            BeforeRequest::Stale {
                request: http::request::Parts { headers, .. },
                matches,
            } => {
                // Cached response is stale and needs to be revalidated
                if matches { Some(headers) } else { None }
            }
        };

        debug!(
            method = request_like.method.as_str(),
            scheme = request_like.uri.scheme_str(),
            authority = request_like.uri.authority().map(Authority::as_str),
            path = request_like.uri.path(),
            key,
            "response is stale: sending request upstream for validation"
        );

        // Revalidate the request
        match request.send(headers).await {
            Ok(response) if response.status() == StatusCode::OK => {
                debug!(
                    method = request_like.method.as_str(),
                    scheme = request_like.uri.scheme_str(),
                    authority = request_like.uri.authority().map(Authority::as_str),
                    path = request_like.uri.path(),
                    key,
                    "server responded with a new response"
                );

                // The server responded with the body, the cached body is no longer valid
                let policy = CachePolicy::new_options(
                    &request_like,
                    &response,
                    SystemTime::now(),
                    self.options,
                );

                // We must drop the old cached response before attempting to update storage;
                // this will release any locks on the body from the cache
                drop(cached_response);

                let (parts, body) = response.into_parts();
                match self.storage.put(&key, &parts, &policy, Some(body)).await {
                    Ok(body) => {
                        debug!(
                            method = request_like.method.as_str(),
                            scheme = request_like.uri.scheme_str(),
                            authority = request_like.uri.authority().map(Authority::as_str),
                            path = request_like.uri.path(),
                            key,
                            "cache storage updated successfully"
                        );

                        // Response was stored, return the response with the storage key
                        let mut response = Response::from_parts(parts, body);
                        response.set_cache_status(CacheLookupStatus::Hit, CacheStatus::Miss);
                        response.set_storage_key(&key);
                        Ok(response)
                    }
                    Err(e) => {
                        debug!(
                            method = request_like.method.as_str(),
                            scheme = request_like.uri.scheme_str(),
                            authority = request_like.uri.authority().map(Authority::as_str),
                            path = request_like.uri.path(),
                            key,
                            error = format!("{e:?}"),
                            "failed to put response into cache storage"
                        );
                        Err(e)
                    }
                }
            }
            Ok(response) if response.status() == StatusCode::NOT_MODIFIED => {
                debug!(
                    method = request_like.method.as_str(),
                    scheme = request_like.uri.scheme_str(),
                    authority = request_like.uri.authority().map(Authority::as_str),
                    path = request_like.uri.path(),
                    key,
                    "server responded with a not modified status"
                );

                // The server informed us that our response hasn't been modified
                // Note that the response body for this code is always empty
                match cached_policy.after_response(&request_like, &response, SystemTime::now()) {
                    AfterResponse::Modified(..) => {
                        debug!(
                            method = request_like.method.as_str(),
                            scheme = request_like.uri.scheme_str(),
                            authority = request_like.uri.authority().map(Authority::as_str),
                            path = request_like.uri.path(),
                            key,
                            "cached response was considered modified despite revalidation"
                        );

                        // This shouldn't happen as the server say it was unmodified, but
                        // `http-cache-semantics` said it was
                        bail!("cached response was considered modified despite revalidation");
                    }
                    AfterResponse::NotModified(policy, parts) => {
                        cached_response.extend_headers(parts.headers);

                        let (parts, _) = cached_response.into_parts();
                        match self.storage.put::<B>(&key, &parts, &policy, None).await {
                            Ok(body) => {
                                debug!(
                                    method = request_like.method.as_str(),
                                    scheme = request_like.uri.scheme_str(),
                                    authority = request_like.uri.authority().map(Authority::as_str),
                                    path = request_like.uri.path(),
                                    key,
                                    "cache storage updated successfully"
                                );

                                // Response was stored and the body comes from storage
                                let mut cached_response = Response::from_parts(parts, body);
                                cached_response
                                    .set_cache_status(CacheLookupStatus::Hit, CacheStatus::Hit);
                                cached_response.set_storage_key(&key);
                                Ok(cached_response)
                            }
                            Err(e) => {
                                debug!(
                                    method = request_like.method.as_str(),
                                    scheme = request_like.uri.scheme_str(),
                                    authority = request_like.uri.authority().map(Authority::as_str),
                                    path = request_like.uri.path(),
                                    key,
                                    error = format!("{e:?}"),
                                    "failed to put response into cache storage"
                                );
                                Err(e)
                            }
                        }
                    }
                }
            }
            Ok(response)
                if response.status().is_server_error() && !cached_response.must_revalidate() =>
            {
                Self::prepare_stale_response(
                    &request_like.method,
                    &request_like.uri,
                    &key,
                    &mut cached_response,
                );
                Ok(cached_response)
            }
            Ok(mut response) => {
                debug!(
                    method = request_like.method.as_str(),
                    scheme = request_like.uri.scheme_str(),
                    authority = request_like.uri.authority().map(Authority::as_str),
                    path = request_like.uri.path(),
                    key,
                    "failed to revalidate response: returning response from server uncached"
                );

                // Otherwise, don't serve the cached response at all
                response.set_cache_status(CacheLookupStatus::Hit, CacheStatus::Miss);
                Ok(response.map(|b| Body::from_upstream(b)))
            }
            Err(e) => {
                if cached_response.must_revalidate() {
                    Err(e)
                } else {
                    Self::prepare_stale_response(
                        &request_like.method,
                        &request_like.uri,
                        &key,
                        &mut cached_response,
                    );
                    Ok(cached_response)
                }
            }
        }
    }

    /// Prepares a stale response for sending back to the client.
    fn prepare_stale_response<B>(
        method: &Method,
        uri: &Uri,
        key: &str,
        response: &mut Response<Body<B>>,
    ) {
        debug!(
            method = method.as_str(),
            scheme = uri.scheme_str(),
            authority = uri.authority().map(Authority::as_str),
            path = uri.path(),
            key,
            "failed to revalidate response: serving potentially stale body from storage with a \
             warning"
        );

        // If the server failed to give us a response, add the required warning to the
        // cached response:
        //   111 Revalidation failed
        //   MUST be included if a cache returns a stale response
        //   because an attempt to revalidate the response failed,
        //   due to an inability to reach the server.
        // (https://tools.ietf.org/html/rfc2616#section-14.46)
        response.add_warning(uri, 111, "Revalidation failed");
        response.set_cache_status(CacheLookupStatus::Hit, CacheStatus::Hit);
        response.set_storage_key(key);
    }
}
