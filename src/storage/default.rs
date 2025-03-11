//! Implementation of the default cache storage.
//!
//! ## Layout
//!
//! This storage implementation uses the following directory structure:
//!
//! ```text
//! <root>/
//! ├─ <storage-version>/
//! │  ├─ <key>/
//! │  │  ├─ .lock
//! │  │  ├─ response
//! │  │  ├─ body
//! │  ├─ <key>/
//! │  │  ├─ .lock
//! │  │  ├─ response
//! │  │  ├─ body
//! │  ├─ ...
//! ```
//!
//! Where `<root>` is the root storage directory and `<key>` is supplied by the
//! cache.
//!
//! ## Key Directory Contents
//!
//! Each key directory contains the following files:
//!
//! * `.lock` - a lock file that coordinates access to the directory's contents.
//! * `response` - the file containing a cached bincode-encoded response.
//! * `body` - the file containing the cached response body.
//!
//! The storage has no TTL on the entries and does not perform any automatic
//! evictions or garbage collection.
//!
//! ## Locking
//!
//! Advisory file locks are obtained on the `.lock` file as the cache entries
//! are read and updated.
//!
//! This is used to coordinate access to the storage via this library; it does
//! not protect against external modifications to the storage.
//!
//! The lock file is also used to validate an entry; a marker is placed in the
//! lock file at the end of a `put` operation and if the marker is not present,
//! storage will treat the entry as not present.
//!
//! ## Integrity
//!
//! This storage implementation does not provide strong guarantees on the
//! integrity of the stored response bodies.
//!
//! If the storage is externally modified, the modification will go undetected
//! and the modified response bodies will be served.

use std::fs;
use std::io;
use std::io::Read;
use std::io::SeekFrom;
use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use http::HeaderMap;
use http::Response;
use http::StatusCode;
use http::Version;
use http::response::Parts;
use http_cache_semantics::CachePolicy;
use serde::Deserialize;
use serde::Serialize;
use tracing::debug;

use crate::HttpBody;
use crate::body::Body;
use crate::lock::LockedFile;
use crate::lock::OpenOptionsExt;
use crate::runtime;
use crate::runtime::SeekExt;
use crate::storage::CacheStorage;

/// The current directory layout version.
const STORAGE_VERSION: &str = "v1";
/// The name of the lock file.
const LOCK_FILE_NAME: &str = ".lock";
/// The name of the response file.
const RESPONSE_FILE_NAME: &str = "response";
/// The name of the response body file.
const BODY_FILE_NAME: &str = "body";
/// A marker present in a lock file to indicate a good entry.
const VALID_MARKER: &str = "ok";

/// Represents a reference to a cached response.
///
/// This type is serialized to the response file.
#[derive(Serialize)]
struct CachedResponseRef<'a> {
    /// The response's status.
    #[serde(with = "http_serde::status_code")]
    status: StatusCode,

    /// The response's version.
    #[serde(with = "http_serde::version")]
    version: Version,

    /// The response's headers.
    #[serde(with = "http_serde::header_map")]
    headers: &'a HeaderMap,

    /// The last used cached policy.
    policy: &'a CachePolicy,
}

/// Represents a cached response.
///
/// This type is deserialized from the response file.
#[derive(Deserialize)]
struct CachedResponse {
    /// The response's status.
    #[serde(with = "http_serde::status_code")]
    status: StatusCode,

    /// The response's version.
    #[serde(with = "http_serde::version")]
    version: Version,

    /// The response's headers.
    #[serde(with = "http_serde::header_map")]
    headers: HeaderMap,

    /// The last used cached policy.
    policy: CachePolicy,
}

/// The default cache storage implementation.
pub struct DefaultCacheStorage(PathBuf);

impl DefaultCacheStorage {
    /// Constructs a new default cache storage with the given
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self(root_dir.into())
    }
}

impl DefaultCacheStorage {
    /// Calculates the path to an entry file.
    fn entry_path(&self, key: &str, file: &str) -> PathBuf {
        let mut path = self.0.to_path_buf();
        path.push(STORAGE_VERSION);
        path.push(key);
        path.push(file);
        path
    }

    /// Locks a cache entry for shared access.
    ///
    /// Returns `Ok(None)` if the file does not exist.
    async fn lock_shared(&self, key: &str) -> Result<Option<LockedFile>> {
        let path = self.entry_path(key, LOCK_FILE_NAME);
        fs::OpenOptions::new()
            .read(true)
            .open_shared(&path)
            .await
            .map(Some)
            .or_else(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    Ok(None)
                } else {
                    Err(e)
                }
            })
            .with_context(|| {
                format!(
                    "failed to open file `{path}` with a shared lock",
                    path = path.display()
                )
            })
    }

    /// Locks a cache entry for exclusive access.
    ///
    /// If the lock file does not exist, it is created.
    ///
    /// The lock file is intentionally truncated upon lock acquisition.
    async fn lock_exclusive(&self, key: &str) -> Result<LockedFile> {
        let mut options = fs::OpenOptions::new();

        // This is opened read-write so we can downgrade exclusive locks to shared
        // Note: we don't use the `truncate` option to truncate the file as we need the
        // truncation to happen *after* the lock is acquired
        options.create(true).read(true).write(true);

        #[cfg(unix)]
        {
            // On Unix, make the mode 600
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let path = self.entry_path(key, LOCK_FILE_NAME);
        let dir = path.parent().expect("should have parent directory");
        fs::create_dir_all(dir)
            .with_context(|| format!("failed to create directory `{dir}`", dir = dir.display()))?;

        let file = options.open_exclusive(&path).await.with_context(|| {
            format!(
                "failed to create file `{path}` with exclusive lock",
                path = path.display()
            )
        })?;

        file.set_len(0).with_context(|| {
            format!(
                "failed to truncate lock file `{path}`",
                path = path.display()
            )
        })?;
        Ok(file)
    }
}

impl CacheStorage for DefaultCacheStorage {
    async fn get<B: HttpBody>(
        &self,
        key: &str,
    ) -> Result<Option<(Response<Body<B>>, CachePolicy)>> {
        // Acquire a shared lock to the entry
        let mut lock = match self.lock_shared(key).await? {
            Some(file) => file,
            None => return Ok(None),
        };

        // Check to see if the entry is valid
        // A valid entry would have a lock file of exactly 1 byte in size
        let mut marker = String::new();
        lock.read_to_string(&mut marker)
            .with_context(|| format!("failed to read lock file for entry `{key}`"))?;
        if marker != VALID_MARKER {
            debug!("lock file for entry `{key}` is not valid: treating as not present");
            return Ok(None);
        }

        // Open the response
        let response_path = self.entry_path(key, RESPONSE_FILE_NAME);
        let mut response_file = match fs::File::open(&response_path)
            .map(Some)
            .or_else(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    Ok(None)
                } else {
                    Err(e)
                }
            })
            .with_context(|| {
                format!(
                    "failed to open response file `{path}`",
                    path = response_path.display()
                )
            })? {
            Some(file) => file,
            None => return Ok(None),
        };

        // Open the response body
        let body_path: PathBuf = self.entry_path(key, BODY_FILE_NAME);
        let body_file = match runtime::File::open(&body_path)
            .await
            .map(Some)
            .or_else(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    Ok(None)
                } else {
                    Err(e)
                }
            })
            .with_context(|| {
                format!(
                    "failed to open response body `{path}`",
                    path = body_path.display()
                )
            })? {
            Some(file) => file,
            None => return Ok(None),
        };

        // Decode the cached response
        let cached = match bincode::serde::decode_from_std_read::<CachedResponse, _, _>(
            &mut response_file,
            bincode::config::standard(),
        )
        .inspect_err(|e| {
            debug!(
                "failed to deserialize response file `{path}`: {e} (cache entry will be ignored)",
                path = response_path.display()
            );
        })
        .ok()
        {
            Some(response) => response,
            None => return Ok(None),
        };

        // Build a response from the cached parts
        let mut builder = Response::builder()
            .version(cached.version)
            .status(cached.status);
        let headers = builder.headers_mut().expect("should be valid");
        headers.extend(cached.headers);

        Ok(Some((
            builder
                .body(Body::from_cache(lock, body_file).await.with_context(|| {
                    format!(
                        "failed to create response body for `{path}`",
                        path = body_path.display()
                    )
                })?)
                .expect("should be valid"),
            cached.policy,
        )))
    }

    async fn put<B: HttpBody>(
        &self,
        key: &str,
        parts: &Parts,
        policy: &CachePolicy,
        body: Option<B>,
    ) -> Result<Body<B>> {
        // Acquire an exclusive lock to the entry
        // This truncates the file, so if we fail to write the marker at the end of this
        // method, future `get` operations will treat the entry as missing
        let mut lock = self.lock_exclusive(key).await?;

        // Create the response file
        let response_path = self.entry_path(key, RESPONSE_FILE_NAME);
        let mut response_file = fs::File::create(&response_path).with_context(|| {
            format!(
                "failed to create response file `{path}`",
                path = response_path.display()
            )
        })?;

        // Encode the response
        bincode::serde::encode_into_std_write(
            CachedResponseRef {
                status: parts.status,
                version: parts.version,
                headers: &parts.headers,
                policy,
            },
            &mut response_file,
            bincode::config::standard(),
        )?;

        let body_path: PathBuf = self.entry_path(key, BODY_FILE_NAME);
        let body_file = match body {
            Some(body) => {
                // Create the body file
                let mut body_file = runtime::OpenOptions::new()
                    .create(true)
                    .read(true)
                    .write(true)
                    .truncate(true)
                    .open(&body_path)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to create body file `{path}`",
                            path = body_path.display()
                        )
                    })?;

                // Write the HTTP body to the file
                Body::from_upstream(body)
                    .write_to(&mut body_file)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to write to body file `{path}`",
                            path = body_path.display()
                        )
                    })?;

                // Seek back to the start
                body_file.seek(SeekFrom::Start(0)).await.with_context(|| {
                    format!(
                        "failed to seek body file `{path}`",
                        path = body_path.display()
                    )
                })?;

                body_file
            }
            None => {
                // Open the response body as one wasn't given to us
                runtime::File::open(&body_path).await.with_context(|| {
                    format!(
                        "failed to open response body `{path}`",
                        path = body_path.display()
                    )
                })?
            }
        };

        // Mark the entry as valid
        lock.write(VALID_MARKER.as_bytes())
            .with_context(|| format!("failed to write to lock file for entry `{key}`"))?;

        // Downgrade the lock to shared
        lock.downgrade()
            .with_context(|| format!("failed to downgrade lock for entry `{key}`"))?;

        Body::from_cache(lock, body_file).await.with_context(|| {
            format!(
                "failed to create response body for `{path}`",
                path = body_path.display()
            )
        })
    }

    async fn delete(&self, key: &str) -> Result<()> {
        // To delete an entry, we simply acquire an exclusive lock and do not write a
        // valid marker
        // While the files still remain on disk, future `get` operations will treat the
        // entry as missing
        self.lock_exclusive(key).await?;
        Ok(())
    }

    fn body_path(&self, key: &str) -> PathBuf {
        self.entry_path(key, BODY_FILE_NAME)
    }
}
