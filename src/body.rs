//! Implementation of a HTTP body.

use std::io;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use anyhow::Result;
use bytes::Bytes;
use bytes::BytesMut;
use futures::Stream;
use http_body::Frame;
use pin_project_lite::pin_project;

use crate::HttpBody;
use crate::lock::LockedFile;
use crate::runtime;

/// The default capacity for reading from files.
const DEFAULT_CAPACITY: usize = 4096;

pin_project! {
    /// Represents the source of a response body.
    #[project = ProjectedSource]
    enum Source<B> {
        /// The body is coming from an upstream response.
        Upstream {
            #[pin]
            body: B,
        },
        /// The body is coming from the cache.
        Cached {
            lock: LockedFile,
            #[pin]
            file: runtime::File,
            len: u64,
            buf: BytesMut,
            finished: bool,
        },
    }
}

pin_project! {
    /// Represents a response body from the HTTP cache.
    ///
    /// The response body may be from upstream or from a cache file.
    pub struct Body<B> {
        #[pin]
        source: Source<B>,
    }
}

impl<B: HttpBody> Body<B> {
    /// Constructs a new body from the cache.
    pub(crate) async fn from_cache(lock: LockedFile, file: runtime::File) -> Result<Self> {
        let metadata = file.metadata().await?;

        Ok(Self {
            source: Source::Cached {
                lock,
                file,
                len: metadata.len(),
                buf: BytesMut::new(),
                finished: false,
            },
        })
    }

    /// Constructs a new body from an upstream response.
    pub(crate) fn from_upstream(body: B) -> Self {
        Self {
            source: Source::Upstream { body },
        }
    }

    /// Writes the body to the given file.
    ///
    /// Trailers in the body are not stored.
    pub(crate) async fn write_to(self, file: &mut runtime::File) -> io::Result<()> {
        cfg_if::cfg_if! {
            if #[cfg(feature = "tokio")] {
                let this = self;
                tokio::pin!(this);
                let mut reader = tokio_util::io::StreamReader::new(this);
                tokio::io::copy(&mut reader, file).await?;
                Ok(())
            } else if #[cfg(feature = "async-std")] {
                use futures::stream::TryStreamExt;
                let this = self;
                futures::pin_mut!(this);
                let mut reader = this.into_async_read();
                async_std::io::copy(&mut reader, file).await?;
                Ok(())
            } else {
                unimplemented!()
            }
        }
    }
}

impl<B: HttpBody> http_body::Body for Body<B> {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, io::Error>>> {
        match self.as_mut().project().source.project() {
            ProjectedSource::Upstream { body } => body.poll_frame(cx),
            ProjectedSource::Cached {
                lock: _,
                file,
                len: _,
                buf,
                finished,
            } => {
                if *finished {
                    return Poll::Ready(None);
                }

                if buf.capacity() == 0 {
                    buf.reserve(DEFAULT_CAPACITY);
                }

                cfg_if::cfg_if! {
                    if #[cfg(feature = "tokio")] {
                        match tokio_util::io::poll_read_buf(file, cx, buf) {
                            Poll::Pending => Poll::Pending,
                            Poll::Ready(Err(err)) => {
                                *finished = true;
                                Poll::Ready(Some(Err(err)))
                            }
                            Poll::Ready(Ok(0)) => {
                                *finished = true;
                                Poll::Ready(None)
                            }
                            Poll::Ready(Ok(_)) => {
                                let chunk = buf.split();
                                Poll::Ready(Some(Ok(Frame::data(chunk.freeze()))))
                            }
                        }
                    } else if #[cfg(feature = "async-std")] {
                        use futures::AsyncRead;
                        use bytes::BufMut;

                        if !buf.has_remaining_mut() {
                            *finished = true;
                            return Poll::Ready(None);
                        }

                        let chunk = buf.chunk_mut();
                        let slice =
                            unsafe { std::slice::from_raw_parts_mut(chunk.as_mut_ptr(), chunk.len()) };
                        match file.poll_read(cx, slice) {
                            Poll::Ready(Ok(n)) if n == 0 => {
                                *finished = true;
                                Poll::Ready(None)
                            }
                            Poll::Ready(Ok(n)) => {
                                unsafe {
                                    buf.advance_mut(n);
                                }
                                Poll::Ready(Some(Ok(Frame::data(buf.split().freeze()))))
                            }
                            Poll::Ready(Err(e)) => {
                                *finished = true;
                                Poll::Ready(Some(Err(e)))
                            }
                            Poll::Pending => Poll::Pending,
                        }
                    } else {
                        unimplemented!()
                    }
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.source {
            Source::Upstream { body } => body.is_end_stream(),
            Source::Cached { finished, .. } => *finished,
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match &self.source {
            Source::Upstream { body } => body.size_hint(),
            Source::Cached { len, .. } => http_body::SizeHint::with_exact(*len),
        }
    }
}

/// An implementation of `Stream` for body.
///
/// This implementation only retrieves the data frames of the body.
///
/// Trailer frames are not read.
impl<B: HttpBody> Stream for Body<B> {
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        use http_body::Body;

        match self.poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => match frame.into_data().ok() {
                Some(data) => Poll::Ready(Some(Ok(data))),
                None => Poll::Ready(None),
            },
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
