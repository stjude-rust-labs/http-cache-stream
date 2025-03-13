//! Implementation of the default cache storage.

use std::fs;
use std::io;
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

use super::StoredResponse;
use crate::HttpBody;
use crate::body::Body;
use crate::lock::LockedFile;
use crate::lock::OpenOptionsExt;
use crate::runtime;
use crate::storage::CacheStorage;

/// The current directory layout version.
const STORAGE_VERSION: &str = "v1";
/// The name of the `responses` directory.
const RESPONSE_DIRECTORY_NAME: &str = "responses";
/// The name of the `content` directory.
const CONTENT_DIRECTORY_NAME: &str = "content";
/// The name of the `tmp` directory.
const TEMP_DIRECTORY_NAME: &str = "tmp";

/// Represents a reference to a cached response.
///
/// This type is serialized to the response file.
///
/// This definition must be kept in sync with `CachedResponse`.
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

    /// The content digest of the response.
    digest: &'a str,

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

    /// The content digest of the response.
    digest: String,

    /// The last used cached policy.
    policy: CachePolicy,
}

/// The default cache storage implementation.
///
/// ## Layout
///
/// This storage implementation uses the following directory structure:
///
/// ```text
/// <root>/
/// ├─ <storage-version>/
/// │  ├─ responses/
/// │  │  ├─ <key>
/// │  │  ├─ <key>
/// │  │  ├─ ...
/// │  ├─ content/
/// │  │  ├─ <digest>
/// │  │  ├─ <digest>
/// │  │  ├─ ...
/// │  ├─ tmp/
/// ```
///
/// Where `<root>` is the root storage directory, `<storage-version>` is a
/// constant that changes when the directory layout changes (currently `v1`),
/// `<key>` is supplied by the cache, and `<digest>` is the calculated digest of
/// a response body.
///
/// ## The `responses` directory
///
/// The `responses` directory contains a file for each cached response.
///
/// The file is a bincode-serialized `CachedResponse` that contains information
/// about the response, including the response body content digest.
///
/// ### Response file locking
///
/// Advisory file locks are obtained on a response file as the cache entries
/// are read and updated.
///
/// This is used to coordinate access to the storage via this library; it does
/// not protect against external modifications to the storage.
///
/// ## The `content` directory
///
/// The `content` directory contains a file for each cached response body.
///
/// The file name is the digest of the response body contents.
///
/// Currently the [`blake3`][blake3] hash algorithm is used for calculating
/// response body digests.
///
/// ## The `tmp` directory
///
/// The `tmp` directory is used for temporarily storing response bodies as they
/// are saved to the cache.
///
/// The content digest of the response is calculated as the response is written
/// into temporary storage.
///
/// Once the response body has been fully read, the temporary file is atomically
/// renamed to its content directory location; if the content already exists,
/// the temporary file is deleted.
///
/// ## Integrity
///
/// This storage implementation does not provide strong guarantees on the
/// integrity of the stored response bodies.
///
/// If the storage is externally modified, the modification will go undetected
/// and the modified response bodies will be served.
///
/// ## Fault tolerance
///
/// If an error occurs while updating a cache entry with
/// [`DefaultCacheStorage::put`], a future [`DefaultCacheStorage::get`] call
/// will treat the entry as not present.
///
/// [blake3]: https://github.com/BLAKE3-team/BLAKE3
pub struct DefaultCacheStorage(PathBuf);

impl DefaultCacheStorage {
    /// Constructs a new default cache storage with the given
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self(root_dir.into())
    }
}

impl DefaultCacheStorage {
    /// Calculates the path to a response file.
    fn response_path(&self, key: &str) -> PathBuf {
        let mut path = self.0.to_path_buf();
        path.push(STORAGE_VERSION);
        path.push(RESPONSE_DIRECTORY_NAME);
        path.push(key);
        path
    }

    /// Calculates the path to a content file.
    fn content_path(&self, digest: &str) -> PathBuf {
        let mut path = self.0.to_path_buf();
        path.push(STORAGE_VERSION);
        path.push(CONTENT_DIRECTORY_NAME);
        path.push(digest);
        path
    }

    /// Calculates the path to the temp directory.
    fn temp_dir_path(&self) -> PathBuf {
        let mut path = self.0.to_path_buf();
        path.push(STORAGE_VERSION);
        path.push(TEMP_DIRECTORY_NAME);
        path
    }

    /// Reads a response from storage for the given key.
    ///
    /// This method will block if the response file is exclusively locked.
    async fn read_response(&self, key: &str) -> Result<Option<CachedResponse>> {
        // Acquire a shared lock on the response file
        let mut response = match self.lock_response_shared(key).await? {
            Some(file) => file,
            None => return Ok(None),
        };

        // Decode the cached response
        Ok(
            bincode::serde::decode_from_std_read::<CachedResponse, _, _>(
                &mut *response,
                bincode::config::standard(),
            )
            .inspect_err(|e| {
                debug!(
                    "failed to deserialize response file `{path}`: {e} (cache entry will be \
                     ignored)",
                    path = self.response_path(key).display()
                );
            })
            .ok(),
        )
    }

    /// Writes a response to storage for the given key.
    ///
    /// This method will block if the response file is locked.
    async fn write_response(&self, key: &str, response: CachedResponseRef<'_>) -> Result<()> {
        // Acquire a shared lock on the response file
        let mut file: LockedFile = self.lock_response_exclusive(key).await?;

        // Encode the response
        bincode::serde::encode_into_std_write(response, &mut *file, bincode::config::standard())
            .with_context(|| format!("failed to serialize response data for cache key `{key}`"))
            .map(|_| ())
    }

    /// Locks a response file for shared access.
    ///
    /// Returns `Ok(None)` if the file does not exist.
    async fn lock_response_shared(&self, key: &str) -> Result<Option<LockedFile>> {
        let path = self.response_path(key);
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
                    "failed to open response file `{path}` with a shared lock",
                    path = path.display()
                )
            })
    }

    /// Locks a response file for exclusive access.
    ///
    /// If the file does not exist, it is created.
    ///
    /// The file is intentionally truncated upon lock acquisition.
    async fn lock_response_exclusive(&self, key: &str) -> Result<LockedFile> {
        let mut options = fs::OpenOptions::new();

        // Note: we don't use the `truncate` option to truncate the file as we need the
        // truncation to happen *after* the lock is acquired
        options.create(true).write(true);

        #[cfg(unix)]
        {
            // On Unix, make the mode 600
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let path = self.response_path(key);
        let dir = path.parent().expect("should have parent directory");
        fs::create_dir_all(dir)
            .with_context(|| format!("failed to create directory `{dir}`", dir = dir.display()))?;

        let file = options.open_exclusive(&path).await.with_context(|| {
            format!(
                "failed to create response file `{path}` with exclusive lock",
                path = path.display()
            )
        })?;

        file.set_len(0).with_context(|| {
            format!(
                "failed to truncate response file `{path}`",
                path = path.display()
            )
        })?;
        Ok(file)
    }
}

impl CacheStorage for DefaultCacheStorage {
    async fn get<B: HttpBody>(&self, key: &str) -> Result<Option<StoredResponse<B>>> {
        let cached = match self.read_response(key).await? {
            Some(response) => response,
            None => return Ok(None),
        };

        // Open the response body
        let path = self.body_path(&cached.digest);
        let body = match runtime::File::open(&path)
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
                    path = path.display()
                )
            })? {
            Some(file) => file,
            None => return Ok(None),
        };

        // Build a response from the cached parts
        let mut builder = Response::builder()
            .version(cached.version)
            .status(cached.status);
        let headers = builder.headers_mut().expect("should be valid");
        headers.extend(cached.headers);

        Ok(Some(StoredResponse {
            response: builder
                .body(Body::from_file(body).await.with_context(|| {
                    format!(
                        "failed to create response body for `{path}`",
                        path = path.display()
                    )
                })?)
                .expect("should be valid"),
            policy: cached.policy,
            digest: cached.digest,
        }))
    }

    async fn put(
        &self,
        key: &str,
        parts: &Parts,
        policy: &CachePolicy,
        digest: &str,
    ) -> Result<()> {
        self.write_response(
            key,
            CachedResponseRef {
                status: parts.status,
                version: parts.version,
                headers: &parts.headers,
                digest,
                policy,
            },
        )
        .await
    }

    async fn put_with_body<B: HttpBody>(
        &self,
        key: &str,
        parts: &Parts,
        policy: &CachePolicy,
        body: B,
    ) -> Result<(Body<B>, String)> {
        // Create a temporary file for the download of the body
        let temp_dir = self.temp_dir_path();
        fs::create_dir_all(&temp_dir).with_context(|| {
            format!(
                "failed to create temporary directory `{path}`",
                path = temp_dir.display()
            )
        })?;
        let temp_path = tempfile::NamedTempFile::new_in(temp_dir)
            .context("failed to create temporary body file for cache storage")?
            .into_temp_path();

        // Write the HTTP body to the file
        let mut body_file = runtime::File::create(&*temp_path).await.with_context(|| {
            format!(
                "failed to create temporary body file `{path}`",
                path = temp_path.display()
            )
        })?;
        let digest = Body::from_upstream(body)
            .write_to(&mut body_file)
            .await
            .with_context(|| {
                format!(
                    "failed to write to temporary body file `{path}`",
                    path = temp_path.display()
                )
            })?;

        // Drop the body file as we're about to rename it
        drop(body_file);

        let content_path = self.content_path(&digest);
        fs::create_dir_all(content_path.parent().expect("should have parent"))
            .context("failed to create content directory")?;

        // Atomically persist the temp file into the `content` location
        temp_path.persist(&content_path).with_context(|| {
            format!(
                "failed to persist downloaded body to content path `{path}`",
                path = content_path.display()
            )
        })?;

        // Update the response
        self.write_response(
            key,
            CachedResponseRef {
                status: parts.status,
                version: parts.version,
                headers: &parts.headers,
                digest: &digest,
                policy,
            },
        )
        .await?;

        Ok((
            Body::from_file(runtime::File::open(&content_path).await.with_context(|| {
                format!(
                    "failed to open body content file `{path}`",
                    path = content_path.display()
                )
            })?)
            .await?,
            digest,
        ))
    }

    async fn delete(&self, key: &str) -> Result<()> {
        // Acquire an exclusive lock on the response file
        // By acquiring the lock, we truncate the file; any attempt to deserialize an
        // empty response file will fail and be treated as not-present
        self.lock_response_exclusive(key).await?;
        Ok(())
    }

    fn body_path(&self, digest: &str) -> PathBuf {
        self.content_path(digest)
    }
}
