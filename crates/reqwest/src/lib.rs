//! An implementation of a [`reqwest`][reqwest] middleware that uses
//! [`http-cache-stream`][http-cache-stream].
//!
//! ```no_run
//! use http_cache_stream_reqwest::Cache;
//! use http_cache_stream_reqwest::storage::DefaultCacheStorage;
//! use reqwest::Client;
//! use reqwest_middleware::ClientBuilder;
//! use reqwest_middleware::Result;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let client = ClientBuilder::new(Client::new())
//!         .with(Cache::new(DefaultCacheStorage::new("./cache")))
//!         .build();
//!     client.get("https://example.com").send().await?;
//!     Ok(())
//! }
//! ```
//!
//! [reqwest]: https://github.com/seanmonstar/reqwest
//! [http-cache-stream]: https://github.com/stjude-rust-labs/http-cache-stream

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
#![warn(rust_2021_compatibility)]
#![warn(clippy::missing_docs_in_private_items)]
#![warn(rustdoc::broken_intra_doc_links)]

use std::io;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use anyhow::Context as _;
use anyhow::Result;
use bytes::Bytes;
use futures::FutureExt;
use futures::future::BoxFuture;
pub use http_cache_stream::X_CACHE;
pub use http_cache_stream::X_CACHE_DIGEST;
pub use http_cache_stream::X_CACHE_LOOKUP;
use http_cache_stream::http::Extensions;
use http_cache_stream::http::Uri;
use http_cache_stream::http_body::Frame;
pub use http_cache_stream::semantics;
pub use http_cache_stream::semantics::CacheOptions;
pub use http_cache_stream::storage;
pub use http_cache_stream::storage::CacheStorage;
use reqwest::Body;
use reqwest::Request;
use reqwest::Response;
use reqwest::ResponseBuilderExt;
use reqwest::header::HeaderMap;
use reqwest_middleware::Next;

pin_project_lite::pin_project! {
    /// Adapter for [`Body`] to implement `HttpBody`.
    struct MiddlewareBody {
        #[pin]
        body: Body
    }
}

impl http_cache_stream::http_body::Body for MiddlewareBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        // The two body implementations differ on error type, so map it here
        self.project().body.poll_frame(cx).map_err(io::Error::other)
    }
}

impl http_cache_stream::HttpBody for MiddlewareBody {}

/// Represents a request flowing through the cache middleware.
struct MiddlewareRequest<'a, 'b> {
    /// The request URI.
    uri: Uri,
    /// The request sent to the middleware.
    request: Request,
    /// The next middleware to run.
    next: Next<'a>,
    /// The request extensions.
    extensions: &'b mut Extensions,
}

impl http_cache_stream::Request<MiddlewareBody> for MiddlewareRequest<'_, '_> {
    fn version(&self) -> http_cache_stream::http::Version {
        self.request.version()
    }

    fn method(&self) -> &http_cache_stream::http::Method {
        self.request.method()
    }

    fn uri(&self) -> &http_cache_stream::http::Uri {
        &self.uri
    }

    fn headers(&self) -> &http_cache_stream::http::HeaderMap {
        self.request.headers()
    }

    async fn send(
        mut self,
        headers: Option<http_cache_stream::http::HeaderMap>,
    ) -> anyhow::Result<http_cache_stream::http::Response<MiddlewareBody>> {
        // Override the specified headers
        if let Some(headers) = headers {
            self.request.headers_mut().extend(headers);
        }

        // Send the response to the next middleware
        let mut response = self.next.run(self.request, self.extensions).await?;

        // Build a response
        let mut builder =
            http_cache_stream::http::Response::builder()
                .version(response.version())
                .status(response.status())
                .url(response.url().as_str().parse().with_context(|| {
                    format!("invalid response URL `{url}`", url = response.url())
                })?);

        let headers = std::mem::take(response.headers_mut());
        builder
            .headers_mut()
            .expect("should have headers")
            .extend(headers);
        builder
            .body(MiddlewareBody {
                body: Body::wrap_stream(response.bytes_stream()),
            })
            .context("failed to create response")
    }
}

/// Implements a caching middleware for [`reqwest`].
pub struct Cache<S>(http_cache_stream::Cache<S>);

impl<S: CacheStorage> Cache<S> {
    /// Constructs a new caching middleware with the given storage.
    pub fn new(storage: S) -> Self {
        Self(http_cache_stream::Cache::new(storage))
    }

    /// Construct a new caching middleware with the given storage and options.
    pub fn new_with_options(storage: S, options: CacheOptions) -> Self {
        Self(http_cache_stream::Cache::new_with_options(storage, options))
    }

    /// Sets the revalidation hook to use.
    ///
    /// The hook is provided the original request and a mutable header map
    /// containing headers explicitly set for the revalidation request.
    ///
    /// For example, a hook may alter the revalidation headers to update an
    /// `Authorization` header based on the headers used for revalidation.
    ///
    /// If the hook returns an error, the error is propagated out as the result
    /// of the original request.
    pub fn with_revalidation_hook(
        mut self,
        hook: impl Fn(&dyn semantics::RequestLike, &mut HeaderMap) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.0 = self.0.with_revalidation_hook(hook);
        self
    }

    /// Gets the underlying storage of the cache.
    pub fn storage(&self) -> &S {
        self.0.storage()
    }
}

impl<S: CacheStorage> reqwest_middleware::Middleware for Cache<S> {
    fn handle<'a, 'b, 'c, 'd>(
        &'a self,
        req: Request,
        extensions: &'b mut Extensions,
        next: Next<'c>,
    ) -> BoxFuture<'d, reqwest_middleware::Result<Response>>
    where
        'a: 'd,
        'b: 'd,
        'c: 'd,
        Self: 'd,
    {
        async {
            let request = MiddlewareRequest {
                uri: req.url().as_str().parse().map_err(|e| {
                    anyhow::anyhow!("URL `{url}` is not valid: {e}", url = req.url())
                })?,
                request: req,
                next,
                extensions,
            };

            let response = self
                .0
                .send(request)
                .await
                .map(|r| r.map(Body::wrap_stream).into())?;
            Ok(response)
        }
        .boxed()
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    use http_cache_stream::http;
    use http_cache_stream::storage::DefaultCacheStorage;
    use reqwest::Response;
    use reqwest::StatusCode;
    use reqwest::header;
    use reqwest_middleware::ClientWithMiddleware;
    use reqwest_middleware::Middleware;
    use tempfile::tempdir;

    use super::*;

    struct MockMiddlewareState {
        responses: Vec<Option<Response>>,
        current: usize,
    }

    struct MockMiddleware(Mutex<MockMiddlewareState>);

    impl MockMiddleware {
        fn new<R>(responses: impl IntoIterator<Item = R>) -> Self
        where
            R: Into<Response>,
        {
            Self(Mutex::new(MockMiddlewareState {
                responses: responses.into_iter().map(|r| Some(r.into())).collect(),
                current: 0,
            }))
        }
    }

    impl Middleware for MockMiddleware {
        fn handle<'a, 'b, 'c, 'd>(
            &'a self,
            _: Request,
            _: &'b mut Extensions,
            _: Next<'c>,
        ) -> BoxFuture<'d, reqwest_middleware::Result<Response>>
        where
            'a: 'd,
            'b: 'd,
            'c: 'd,
            Self: 'd,
        {
            async {
                let mut state = self.0.lock().unwrap();

                let current = state.current;
                state.current += 1;

                Ok(state
                    .responses
                    .get_mut(current)
                    .expect("unexpected client request: not enough responses defined")
                    .take()
                    .unwrap())
            }
            .boxed()
        }
    }

    #[tokio::test]
    async fn no_store() {
        const BODY: &str = "hello world!";
        const DIGEST: &str = "3aa61c409fd7717c9d9c639202af2fae470c0ef669be7ba2caea5779cb534e9d";

        let dir = tempdir().unwrap();
        let cache = Arc::new(Cache::new(DefaultCacheStorage::new(dir.path())));
        let mock = Arc::new(MockMiddleware::new([
            http::Response::builder()
                .header(header::CACHE_CONTROL, "no-store")
                .body(BODY)
                .unwrap(),
            http::Response::builder()
                .header(header::CACHE_CONTROL, "no-store")
                .body(BODY)
                .unwrap(),
        ]));
        let client = ClientWithMiddleware::new(
            Default::default(),
            vec![cache.clone() as Arc<dyn Middleware>, mock.clone()],
        );

        // Response should not be served from the cache or stored
        let response = client.get("http://test.local/").send().await.unwrap();
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        assert_eq!(response.headers().get(X_CACHE_LOOKUP).unwrap(), "MISS");
        assert_eq!(response.headers().get(X_CACHE).unwrap(), "MISS");
        assert!(response.headers().get(X_CACHE_DIGEST).is_none());
        assert_eq!(response.text().await.unwrap(), BODY);

        // Ensure no content directory
        assert!(!cache.storage().body_path(DIGEST).exists());

        // Response should *still* not be served from the cache or stored
        let response = client.get("http://test.local/").send().await.unwrap();
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        assert_eq!(response.headers().get(X_CACHE_LOOKUP).unwrap(), "MISS");
        assert_eq!(response.headers().get(X_CACHE).unwrap(), "MISS");
        assert!(response.headers().get(X_CACHE_DIGEST).is_none());
        assert_eq!(response.text().await.unwrap(), BODY);

        // Ensure no content directory
        assert!(!cache.storage().body_path(DIGEST).exists());
    }

    #[tokio::test]
    async fn max_age() {
        const BODY: &str = "hello world!";
        const DIGEST: &str = "3aa61c409fd7717c9d9c639202af2fae470c0ef669be7ba2caea5779cb534e9d";

        let dir = tempdir().unwrap();
        let cache = Arc::new(
            Cache::new(DefaultCacheStorage::new(dir.path()))
                .with_revalidation_hook(|_, _| panic!("a revalidation should not take place")),
        );
        let mock = Arc::new(MockMiddleware::new([http::Response::builder()
            .header(header::CACHE_CONTROL, "max-age=1000")
            .body(BODY)
            .unwrap()]));
        let client = ClientWithMiddleware::new(
            Default::default(),
            vec![cache.clone() as Arc<dyn Middleware>, mock.clone()],
        );

        // First response should not be served from the cache
        let response = client.get("http://test.local/").send().await.unwrap();
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "max-age=1000"
        );
        assert_eq!(response.headers().get(X_CACHE_LOOKUP).unwrap(), "MISS");
        assert_eq!(response.headers().get(X_CACHE).unwrap(), "MISS");
        assert!(response.headers().get(X_CACHE_DIGEST).is_none());
        assert_eq!(response.text().await.unwrap(), BODY);

        // Ensure a content directory exists
        assert!(cache.storage().body_path(DIGEST).exists());

        // Second response should be served from the cache without revalidation
        // If a revalidation is made, the mock middleware will panic since there was
        // only one response defined
        let response = client.get("http://test.local/").send().await.unwrap();
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "max-age=1000"
        );
        assert_eq!(response.headers().get(X_CACHE_LOOKUP).unwrap(), "HIT");
        assert_eq!(response.headers().get(X_CACHE).unwrap(), "HIT");
        assert_eq!(
            response
                .headers()
                .get(X_CACHE_DIGEST)
                .map(|v| v.to_str().unwrap())
                .unwrap(),
            DIGEST
        );
        assert_eq!(response.text().await.unwrap(), BODY);
    }

    #[tokio::test]
    async fn cache_hit_unmodified() {
        const BODY: &str = "hello world!";
        const DIGEST: &str = "3aa61c409fd7717c9d9c639202af2fae470c0ef669be7ba2caea5779cb534e9d";

        let dir = tempdir().unwrap();
        let revalidated = Arc::new(AtomicBool::new(false));
        let revalidated_clone = revalidated.clone();
        let cache = Arc::new(
            Cache::new(DefaultCacheStorage::new(dir.path())).with_revalidation_hook(move |_, _| {
                revalidated_clone.store(true, Ordering::SeqCst);
                Ok(())
            }),
        );
        let mock = Arc::new(MockMiddleware::new([
            http::Response::builder().body(BODY).unwrap(),
            http::Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .body("")
                .unwrap(),
        ]));
        let client = ClientWithMiddleware::new(
            Default::default(),
            vec![cache.clone() as Arc<dyn Middleware>, mock.clone()],
        );

        // First response should be a miss
        let response = client.get("http://test.local/").send().await.unwrap();
        assert_eq!(response.headers().get(X_CACHE_LOOKUP).unwrap(), "MISS");
        assert_eq!(response.headers().get(X_CACHE).unwrap(), "MISS");
        assert!(response.headers().get(X_CACHE_DIGEST).is_none());
        assert_eq!(response.text().await.unwrap(), BODY);

        // Ensure there is a content directory
        assert!(cache.storage().body_path(DIGEST).exists());

        // Assert no revalidation took place
        assert!(!revalidated.load(Ordering::SeqCst));

        // Second response should be served from the cache
        let response = client.get("http://test.local/").send().await.unwrap();
        assert_eq!(response.headers().get(X_CACHE_LOOKUP).unwrap(), "HIT");
        assert_eq!(response.headers().get(X_CACHE).unwrap(), "HIT");
        assert_eq!(
            response
                .headers()
                .get(X_CACHE_DIGEST)
                .map(|v| v.to_str().unwrap())
                .unwrap(),
            DIGEST
        );
        assert_eq!(response.text().await.unwrap(), BODY);

        // Assert a revalidation took place
        assert!(revalidated.swap(false, Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cache_hit_modified() {
        const BODY: &str = "hello world!";
        const MODIFIED_BODY: &str = "hello world!!!";
        const DIGEST: &str = "3aa61c409fd7717c9d9c639202af2fae470c0ef669be7ba2caea5779cb534e9d";
        const MODIFIED_DIGEST: &str =
            "22b8d362b2e8064356915b1451f630d1d920b427d3b2f9b3432fbf4c03d94184";

        let dir = tempdir().unwrap();
        let revalidated = Arc::new(AtomicBool::new(false));
        let revalidated_clone = revalidated.clone();
        let cache = Arc::new(
            Cache::new(DefaultCacheStorage::new(dir.path())).with_revalidation_hook(move |_, _| {
                revalidated_clone.store(true, Ordering::SeqCst);
                Ok(())
            }),
        );
        let mock = Arc::new(MockMiddleware::new([
            http::Response::builder().body(BODY).unwrap(),
            http::Response::builder().body(MODIFIED_BODY).unwrap(),
            http::Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .body("")
                .unwrap(),
        ]));
        let client = ClientWithMiddleware::new(
            Default::default(),
            vec![cache.clone() as Arc<dyn Middleware>, mock.clone()],
        );

        // First response should be a miss
        let response = client.get("http://test.local/").send().await.unwrap();
        assert_eq!(response.headers().get(X_CACHE_LOOKUP).unwrap(), "MISS");
        assert_eq!(response.headers().get(X_CACHE).unwrap(), "MISS");
        assert!(response.headers().get(X_CACHE_DIGEST).is_none());
        assert_eq!(response.text().await.unwrap(), BODY);

        // Ensure there is a content directory
        assert!(cache.storage().body_path(DIGEST).exists());

        // Assert no revalidation took place
        assert!(!revalidated.load(Ordering::SeqCst));

        // Second response should not be served from the cache (was modified)
        let response = client.get("http://test.local/").send().await.unwrap();
        assert_eq!(response.headers().get(X_CACHE_LOOKUP).unwrap(), "HIT");
        assert_eq!(response.headers().get(X_CACHE).unwrap(), "MISS");
        assert!(response.headers().get(X_CACHE_DIGEST).is_none());
        assert_eq!(response.text().await.unwrap(), MODIFIED_BODY);

        // Ensure there is a content directory
        assert!(cache.storage().body_path(MODIFIED_DIGEST).exists());

        // Assert a revalidation took place
        assert!(revalidated.swap(false, Ordering::SeqCst));

        // Second response should be served from the cache (not modified)
        let response = client.get("http://test.local/").send().await.unwrap();
        assert_eq!(response.headers().get(X_CACHE_LOOKUP).unwrap(), "HIT");
        assert_eq!(response.headers().get(X_CACHE).unwrap(), "HIT");
        assert_eq!(
            response
                .headers()
                .get(X_CACHE_DIGEST)
                .map(|v| v.to_str().unwrap())
                .unwrap(),
            MODIFIED_DIGEST
        );
        assert_eq!(response.text().await.unwrap(), MODIFIED_BODY);

        // Assert a revalidation took place
        assert!(revalidated.swap(false, Ordering::SeqCst));
    }
}
