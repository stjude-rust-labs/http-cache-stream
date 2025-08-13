//! Implementation of a HTTP body.

use std::io;
use std::path::Path;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::task::ready;

use anyhow::Context as _;
use anyhow::Result;
use blake3::Hasher;
use bytes::Bytes;
use bytes::BytesMut;
use futures::Stream;
use futures::future::BoxFuture;
use http_body::Frame;
use pin_project_lite::pin_project;
use runtime::AsyncWrite;
use tempfile::NamedTempFile;
use tempfile::TempPath;

use crate::HttpBody;
use crate::runtime;

/// The default capacity for reading from files.
const DEFAULT_CAPACITY: usize = 4096;

pin_project! {
    /// Represents the state machine of a caching upstream source.
    #[project = ProjectedCachingUpstreamSourceState]
    enum CachingUpstreamSourceState<B> {
        /// The upstream body is being read.
        ReadingUpstream {
            // The upstream response body.
            #[pin]
            upstream: B,
            // The writer for the cache file.
            #[pin]
            writer: Option<runtime::BufWriter<runtime::File>>,
            // The temporary path of the cache file.
            path: Option<TempPath>,
            // The current bytes read from the upstream body.
            current: Bytes,
            // The hasher used to hash the body.
            hasher: Hasher,
            // The callback to invoke once the cache file is completed.
            callback: Option<Box<dyn FnOnce(String, TempPath) -> BoxFuture<'static, Result<()>> + Send>>,
        },
        /// The cache file is being flushed.
        FlushingFile {
            // The writer for the cache file.
            #[pin]
            writer: Option<runtime::BufWriter<runtime::File>>,
            // The temporary path of the cache file.
            path: Option<TempPath>,
            // The digest of the response body.
            digest: String,
            // The callback to invoke once the cache file is completed.
            callback: Option<Box<dyn FnOnce(String, TempPath) -> BoxFuture<'static, Result<()>> + Send>>,
        },
        /// The callback is being invoked.
        InvokingCallback {
            #[pin]
            future: BoxFuture<'static, Result<()>>,
        },
        /// The stream has completed.
        Completed
    }
}

pin_project! {
    /// Represents a body source from an upstream body that is being cached.
    struct CachingUpstreamSource<B> {
        // The state of the stream.
        #[pin]
        state: CachingUpstreamSourceState<B>,
    }
}

impl<B> CachingUpstreamSource<B> {
    /// Creates a new body source for caching an upstream response.
    ///
    /// The callback is invoked after the body has been written to the cache.
    async fn new<F>(upstream: B, temp_dir: &Path, callback: F) -> Result<Self>
    where
        F: FnOnce(String, TempPath) -> BoxFuture<'static, Result<()>> + Send + 'static,
    {
        let path = NamedTempFile::new_in(temp_dir)
            .context("failed to create temporary body file for cache storage")?
            .into_temp_path();

        let file = runtime::File::create(&*path).await.with_context(|| {
            format!(
                "failed to create temporary body file `{path}`",
                path = path.display()
            )
        })?;

        Ok(Self {
            state: CachingUpstreamSourceState::ReadingUpstream {
                upstream,
                writer: Some(runtime::BufWriter::new(file)),
                path: Some(path),
                callback: Some(Box::new(callback)),
                current: Bytes::new(),
                hasher: Hasher::new(),
            },
        })
    }
}

impl<B> Stream for CachingUpstreamSource<B>
where
    B: HttpBody,
{
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let this = self.as_mut().project();
            match this.state.project() {
                ProjectedCachingUpstreamSourceState::ReadingUpstream {
                    upstream,
                    mut writer,
                    path,
                    current,
                    hasher,
                    callback,
                } => {
                    // Check to see if a read is needed
                    if current.is_empty() {
                        match ready!(upstream.poll_next_data(cx)) {
                            Some(Ok(data)) if data.is_empty() => continue,
                            Some(Ok(data)) => {
                                // Update the hasher with the data that was read
                                hasher.update(&data);
                                *current = data;
                            }
                            Some(Err(e)) => {
                                // Set state to finished and return
                                self.set(Self {
                                    state: CachingUpstreamSourceState::Completed,
                                });
                                return Poll::Ready(Some(Err(e)));
                            }
                            None => {
                                let writer = writer.take();
                                let path = path.take();
                                let digest = hex::encode(hasher.finalize().as_bytes());
                                let callback = callback.take();

                                // We're done reading from upstream, transition to the flushing
                                // state
                                self.set(Self {
                                    state: CachingUpstreamSourceState::FlushingFile {
                                        writer,
                                        path,
                                        digest,
                                        callback,
                                    },
                                });
                                continue;
                            }
                        }
                    }

                    // Write the data to the cache and return it to the caller
                    let mut data = current.clone();
                    return match ready!(writer.as_pin_mut().unwrap().poll_write(cx, &data)) {
                        Ok(n) => {
                            *current = data.split_off(n);
                            Poll::Ready(Some(Ok(data)))
                        }
                        Err(e) => {
                            self.set(Self {
                                state: CachingUpstreamSourceState::Completed,
                            });
                            Poll::Ready(Some(Err(e)))
                        }
                    };
                }
                ProjectedCachingUpstreamSourceState::FlushingFile {
                    mut writer,
                    path,
                    digest,
                    callback,
                } => {
                    // Attempt to poll the writer for flush
                    match ready!(writer.as_mut().as_pin_mut().unwrap().poll_flush(cx)) {
                        Ok(_) => {
                            drop(writer.take());
                            let path = path.take().unwrap();
                            let digest = std::mem::take(digest);
                            let callback = callback.take().unwrap();

                            // Invoke the callback and transition to the invoking callback state
                            let future = callback(digest, path);
                            self.set(Self {
                                state: CachingUpstreamSourceState::InvokingCallback { future },
                            });
                            continue;
                        }
                        Err(e) => {
                            self.set(Self {
                                state: CachingUpstreamSourceState::Completed,
                            });
                            return Poll::Ready(Some(Err(e)));
                        }
                    }
                }
                ProjectedCachingUpstreamSourceState::InvokingCallback { future } => {
                    return match ready!(future.poll(cx)) {
                        Ok(_) => {
                            self.set(Self {
                                state: CachingUpstreamSourceState::Completed,
                            });
                            Poll::Ready(None)
                        }
                        Err(e) => {
                            self.set(Self {
                                state: CachingUpstreamSourceState::Completed,
                            });
                            Poll::Ready(Some(Err(io::Error::other(e))))
                        }
                    };
                }
                ProjectedCachingUpstreamSourceState::Completed => return Poll::Ready(None),
            }
        }
    }
}

pin_project! {
    /// Represents a body source from a previously cached response body file.
    struct FileSource {
        // The cache file being read.
        #[pin]
        reader: runtime::BufReader<runtime::File>,
        // The length of the file.
        len: u64,
        // The current read buffer.
        buf: BytesMut,
        // Whether or not we've finished the stream.
        finished: bool,
    }
}

impl Stream for FileSource {
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        if *this.finished {
            return Poll::Ready(None);
        }

        if this.buf.capacity() == 0 {
            this.buf.reserve(DEFAULT_CAPACITY);
        }

        cfg_if::cfg_if! {
            if #[cfg(feature = "tokio")] {
                match ready!(tokio_util::io::poll_read_buf(this.reader, cx, this.buf)) {
                    Ok(0) => {
                        *this.finished = true;
                        Poll::Ready(None)
                    }
                    Ok(_) => {
                        let chunk = this.buf.split();
                        Poll::Ready(Some(Ok(chunk.freeze())))
                    }
                    Err(err) => {
                        *this.finished = true;
                        Poll::Ready(Some(Err(err)))
                    }
                }
            } else if #[cfg(feature = "smol")] {
                use futures::AsyncRead;
                use bytes::BufMut;

                if !this.buf.has_remaining_mut() {
                    *this.finished = true;
                    return Poll::Ready(None);
                }

                let chunk = this.buf.chunk_mut();
                // SAFETY: `from_raw_parts_mut` will return a mutable slice treating the memory
                //         as initialized despite itself being uninitialized.
                //
                //         However, we are only using the slice as a read buffer, so the
                //         uninitialized content of the slice is not actually read.
                //
                //         Finally, upon a successful read, we advance the mutable buffer by the
                //         number of bytes read so that the remaining uninitialized content of
                //         the buffer will remain for the next poll.
                let slice =
                    unsafe { std::slice::from_raw_parts_mut(chunk.as_mut_ptr(), chunk.len()) };
                match ready!(this.reader.poll_read(cx, slice)) {
                    Ok(0) => {
                        *this.finished = true;
                        Poll::Ready(None)
                    }
                    Ok(n) => {
                        unsafe {
                            this.buf.advance_mut(n);
                        }
                        Poll::Ready(Some(Ok(this.buf.split().freeze())))
                    }
                    Err(e) => {
                        *this.finished = true;
                        Poll::Ready(Some(Err(e)))
                    }
                }
            } else {
                unimplemented!()
            }
        }
    }
}

pin_project! {
    /// Represents a response body source.
    ///
    /// The body may come from the following sources:
    ///
    /// * Upstream without caching the response body.
    /// * Upstream with caching the response body.
    /// * A previously cached response body file.
    #[project = ProjectedBodySource]
    enum BodySource<B> {
        /// The body is coming from upstream without being cached.
        Upstream {
            // The underlying source for the body.
            #[pin]
            source: B
        },
        /// The body is coming from upstream with being cached.
        CachingUpstream {
            // The underlying source for the body.
            #[pin]
            source: CachingUpstreamSource<B>,
        },
        /// The body is coming from a previously cached response body.
        File {
            // The underlying source for the body.
            #[pin]
            source: FileSource
        },
    }
}

pin_project! {
    /// Represents a response body.
    pub struct Body<B> {
        // The body source.
        #[pin]
        source: BodySource<B>
    }
}

impl<B> Body<B>
where
    B: HttpBody,
{
    /// Constructs a new body from an upstream response body that is not being
    /// cached.
    pub(crate) fn from_upstream(upstream: B) -> Self {
        Self {
            source: BodySource::Upstream { source: upstream },
        }
    }

    /// Constructs a new body from an upstream response body that is being
    /// cached.
    pub(crate) async fn from_caching_upstream<F>(
        upstream: B,
        temp_dir: &Path,
        callback: F,
    ) -> Result<Self>
    where
        F: FnOnce(String, TempPath) -> BoxFuture<'static, Result<()>> + Send + 'static,
    {
        Ok(Self {
            source: BodySource::CachingUpstream {
                source: CachingUpstreamSource::new(upstream, temp_dir, callback).await?,
            },
        })
    }

    /// Constructs a new body from a local file.
    pub(crate) async fn from_file(file: runtime::File) -> Result<Self> {
        let metadata = file.metadata().await?;

        Ok(Self {
            source: BodySource::File {
                source: FileSource {
                    reader: runtime::BufReader::new(file),
                    len: metadata.len(),
                    buf: BytesMut::new(),
                    finished: false,
                },
            },
        })
    }
}

impl<B> http_body::Body for Body<B>
where
    B: HttpBody,
{
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, io::Error>>> {
        match self.project().source.project() {
            ProjectedBodySource::Upstream { source } => source.poll_frame(cx),
            ProjectedBodySource::CachingUpstream { source } => {
                source.poll_next(cx).map_ok(Frame::data)
            }
            ProjectedBodySource::File { source } => source.poll_next(cx).map_ok(Frame::data),
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.source {
            BodySource::Upstream { source } => source.is_end_stream(),
            BodySource::CachingUpstream { source } => {
                matches!(&source.state, CachingUpstreamSourceState::Completed)
            }
            BodySource::File { source } => source.finished,
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match &self.source {
            BodySource::Upstream { source } => source.size_hint(),
            BodySource::CachingUpstream { source } => match &source.state {
                CachingUpstreamSourceState::ReadingUpstream { upstream, .. } => {
                    upstream.size_hint()
                }
                _ => http_body::SizeHint::default(),
            },
            BodySource::File { source } => http_body::SizeHint::with_exact(source.len),
        }
    }
}

impl<B> HttpBody for Body<B> where B: HttpBody + Send {}

/// An implementation of `Stream` for body.
///
/// This implementation only retrieves the data frames of the body.
///
/// Trailer frames are not read.
impl<B> Stream for Body<B>
where
    B: HttpBody,
{
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project().source.project() {
            ProjectedBodySource::Upstream { source } => source.poll_next_data(cx),
            ProjectedBodySource::CachingUpstream { source } => source.poll_next(cx),
            ProjectedBodySource::File { source } => source.poll_next(cx),
        }
    }
}
