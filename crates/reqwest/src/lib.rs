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
