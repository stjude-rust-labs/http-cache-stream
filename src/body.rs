//! Implementation of a HTTP body.

use std::io;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use anyhow::Result;
use blake3::Hash;
use blake3::Hasher;
use bytes::Bytes;
use bytes::BytesMut;
use futures::Stream;
use http_body::Frame;
use pin_project_lite::pin_project;

use crate::HttpBody;
use crate::runtime;

/// The default capacity for reading from files.
const DEFAULT_CAPACITY: usize = 4096;

pin_project! {
    /// A wrapper around a byte stream that performs a Blake3 hash on the stream data.
    struct HashStream<S> {
        #[pin]
        stream: S,
        hasher: Hasher,
        finished: bool,
    }
}

impl<S> HashStream<S> {
    /// Constructs a new hash stream.
    fn new(stream: S) -> Self
    where
        S: Stream<Item = io::Result<Bytes>>,
    {
        Self {
            stream,
            hasher: Hasher::new(),
            finished: false,
        }
    }

    /// Computes the hash of the byte stream.
    fn hash(self) -> Hash {
        self.hasher.finalize()
    }
}

impl<S> Stream for HashStream<S>
where
    S: Stream<Item = io::Result<Bytes>>,
{
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.finished {
            return Poll::Ready(None);
        }

        let this = self.project();
        match this.stream.poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                this.hasher.update(&bytes);
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(e))) => {
                *this.finished = true;
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                *this.finished = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

pin_project! {
    /// Represents the source of a response body.
    #[project = ProjectedSource]
    enum Source<B> {
        /// The body is coming from an upstream response.
        Upstream {
            #[pin]
            body: B,
        },
        /// The body is coming from a local file.
        File {
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
    /// The response body may be from upstream or from a local file.
    pub struct Body<B> {
        #[pin]
        source: Source<B>,
    }
}

impl<B: HttpBody> Body<B> {
    /// Constructs a new body from a local file.
    pub(crate) async fn from_file(file: runtime::File) -> Result<Self> {
        let metadata = file.metadata().await?;

        Ok(Self {
            source: Source::File {
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
    ///
    /// Returns the calculated Blake3 hash of the body.
    pub(crate) async fn write_to(self, file: &mut runtime::File) -> io::Result<String> {
        cfg_if::cfg_if! {
            if #[cfg(feature = "tokio")] {
                let this = self;
                tokio::pin!(this);

                let mut stream = HashStream::new(this);
                let mut reader = tokio_util::io::StreamReader::new(&mut stream);
                tokio::io::copy(&mut reader, file).await?;
                Ok(hex::encode(stream.hash().as_bytes()))
            } else if #[cfg(feature = "async-std")] {
                use futures::stream::TryStreamExt;
                let this = self;
                futures::pin_mut!(this);

                let mut stream = HashStream::new(this);
                let mut reader = (&mut stream).into_async_read();
                async_std::io::copy(&mut reader, file).await?;
                Ok(hex::encode(stream.hash().as_bytes()))
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
            ProjectedSource::File {
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
                            Poll::Ready(Ok(0)) => {
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
            Source::File { finished, .. } => *finished,
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match &self.source {
            Source::Upstream { body } => body.size_hint(),
            Source::File { len, .. } => http_body::SizeHint::with_exact(*len),
        }
    }
}

impl<B: HttpBody> HttpBody for Body<B> {}

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
