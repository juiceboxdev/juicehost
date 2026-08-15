//! Disk handling and S3-compatible storage.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use futures::{Stream, StreamExt};
use object_store::ObjectStoreExt;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use utoipa::ToSchema;

use crate::error::StorageError;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, StorageError>> + Send>>;

/// Metadata about a stored file, used for ETag generation and responses.
#[derive(Debug, Clone)]
pub struct FileMetadata {
    /// Content length in bytes.
    pub size: u64,
    /// ETag value (backend-specific format).
    pub etag: String,
    /// File extension (e.g. "png", "bin").
    pub extension: String,
}

/// Response from a storage get operation.
pub struct FileData {
    /// The file contents.
    pub data: Bytes,
    /// File metadata including ETag.
    pub meta: FileMetadata,
}

/// Storage metrics for health/info endpoints.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StorageMetrics {
    /// Total disk capacity in bytes (0 for S3).
    pub total_bytes: u64,
    /// Bytes used on disk (0 for S3).
    pub used_bytes: u64,
    /// Available free bytes (u64::MAX for S3).
    pub free_bytes: u64,
    /// Minimum free bytes required before rejecting writes.
    pub min_free_bytes: u64,
    /// True when free space is below the minimum threshold.
    pub out_of_space: bool,
}

/// Trait for pluggable file storage backends.
#[async_trait::async_trait]
pub trait StorageBackend: Send + Sync + 'static {
    /// Store a file. Returns an error if the file already exists.
    async fn put(&self, id: &str, filename: &str, data: Bytes) -> Result<(), StorageError>;

    /// Stream a new file into storage without exposing backend-specific paths.
    async fn put_stream(
        &self,
        id: &str,
        filename: &str,
        data: ByteStream,
    ) -> Result<u64, StorageError>;

    /// Retrieve a file's contents and metadata.
    async fn get(&self, id: &str) -> Result<FileData, StorageError>;

    /// Get only metadata (size, ETag, extension) without reading file contents.
    /// Used for ETag-first lookups to avoid buffering the full file for 304 responses.
    async fn stat(&self, id: &str) -> Result<FileMetadata, StorageError>;

    /// Stream a byte range. `start` and `end` are inclusive.
    async fn get_range_stream(
        &self,
        id: &str,
        start: u64,
        end: u64,
    ) -> Result<ByteStream, StorageError>;

    /// Delete a file. Returns Ok(true) if deleted, Ok(false) if not found.
    async fn delete(&self, id: &str) -> Result<bool, StorageError>;

    /// Rename a file (change its ID). Returns error if new ID already exists.
    async fn rename(&self, old_id: &str, new_id: &str) -> Result<(), StorageError>;

    /// Get storage capacity metrics.
    fn storage_metrics(&self, min_free_bytes: u64) -> StorageMetrics;

    /// Concatenate multiple files into a new file, then delete the originals.
    async fn concat(
        &self,
        target_id: &str,
        filename: &str,
        part_ids: &[&str],
    ) -> Result<(), StorageError>;

    /// Return a streaming reader for a stored file.
    async fn get_stream(&self, id: &str) -> Result<ByteStream, StorageError>;
}

/// Local disk backend.
pub struct LocalBackend {
    /// Root directory where files are stored.
    files_dir: PathBuf,
    /// Maps file ID -> extension for MIME type detection.
    extensions: DashMap<String, String>,
    /// Metadata cache (ETag, size) to avoid repeated stat() syscalls on hot files.
    meta_cache: DashMap<String, FileMetadata>,
    /// Minimum free disk space required before rejecting writes.
    min_free_space_bytes: u64,
}

impl LocalBackend {
    pub fn new(files_dir: PathBuf, min_free_space_bytes: u64) -> Result<Self, String> {
        let files_dir = std::fs::canonicalize(&files_dir)
            .map_err(|e| format!("canonicalize files directory failed: {e}"))?;
        if !std::fs::metadata(&files_dir)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            return Err("files directory is not a directory".into());
        }
        Ok(Self {
            files_dir,
            extensions: DashMap::new(),
            meta_cache: DashMap::new(),
            min_free_space_bytes,
        })
    }

    /// Scan the files directory and populate the extension cache.
    pub async fn init_cache(&self) -> Result<(), String> {
        let mut entries = tokio::fs::read_dir(&self.files_dir)
            .await
            .map_err(|e| format!("read files directory failed: {e}"))?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') && (name.ends_with(".reserve") || name.ends_with(".tmp")) {
                let file_type = entry.file_type().await.map_err(|e| e.to_string())?;
                if file_type.is_file() {
                    tokio::fs::remove_file(entry.path())
                        .await
                        .map_err(|e| format!("remove stale storage file {name}: {e}"))?;
                }
                continue;
            }
            let file_type = entry.file_type().await.map_err(|e| e.to_string())?;
            if file_type.is_symlink() || !file_type.is_file() {
                return Err(format!("storage entry is not a regular file: {name}"));
            }
            if let Some(dot) = name.rfind('.') {
                let id = name[..dot].to_string();
                let ext = name[dot + 1..].to_string();
                if !valid_component(&id) || !valid_component(&ext) {
                    return Err(format!("invalid storage entry name: {name}"));
                }
                if self.extensions.insert(id.clone(), ext).is_some() {
                    return Err(format!("duplicate logical file ID: {id}"));
                }
            }
        }
        tracing::info!(
            "local storage cache initialized with {} entries",
            self.extensions.len()
        );
        Ok(())
    }

    /// Resolve the filesystem path for a file ID using the extension cache.
    fn resolve_path(&self, id: &str) -> Option<PathBuf> {
        if !valid_component(id) {
            return None;
        }
        let ext = self.extensions.get(id).map(|e| e.value().clone())?;
        self.path_for(id, &ext)
    }

    /// Stat a file using the cache when available without reading its contents.
    async fn stat_cached(&self, id: &str) -> Result<FileMetadata, StorageError> {
        if let Some(cached) = self.meta_cache.get(id) {
            return Ok(cached.clone());
        }
        let path = self.resolve_path(id).ok_or(StorageError::NotFound)?;
        let meta = tokio::fs::symlink_metadata(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound
            } else {
                StorageError::Io(format!("metadata failed: {}", e))
            }
        })?;
        if !meta.is_file() {
            return Err(StorageError::Io(
                "storage path is not a regular file".into(),
            ));
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin")
            .to_string();
        let file_meta = FileMetadata {
            size: meta.len(),
            etag: etag_from_metadata(&meta),
            extension: ext,
        };
        self.meta_cache.insert(id.to_string(), file_meta.clone());
        Ok(file_meta)
    }
}

struct LocalReservation {
    path: PathBuf,
}

impl Drop for LocalReservation {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl LocalBackend {
    fn path_for(&self, id: &str, ext: &str) -> Option<PathBuf> {
        if !valid_component(id) || !valid_component(ext) {
            return None;
        }
        let path = self.files_dir.join(format!("{id}.{ext}"));
        path.parent().filter(|parent| *parent == self.files_dir)?;
        Some(path)
    }

    async fn reserve_id(&self, id: &str) -> Result<LocalReservation, StorageError> {
        if !valid_component(id) {
            return Err(StorageError::Io("invalid logical ID".into()));
        }
        let path = self.files_dir.join(format!(".{id}.reserve"));
        tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    StorageError::Conflict
                } else {
                    StorageError::Io(format!("reserve ID failed: {e}"))
                }
            })?;
        if self.extensions.contains_key(id) {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(StorageError::Conflict);
        }
        Ok(LocalReservation { path })
    }

    async fn open_regular(&self, path: &PathBuf) -> Result<tokio::fs::File, StorageError> {
        if path.parent() != Some(self.files_dir.as_path()) {
            return Err(StorageError::Io(
                "storage path escaped files directory".into(),
            ));
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound
            } else {
                StorageError::Io(format!("open regular file failed: {e}"))
            }
        })?;
        let metadata = file
            .metadata()
            .await
            .map_err(|e| StorageError::Io(format!("file metadata failed: {e}")))?;
        if !metadata.is_file() {
            return Err(StorageError::Io(
                "storage path is not a regular file".into(),
            ));
        }
        Ok(file)
    }
}

#[async_trait::async_trait]
impl StorageBackend for LocalBackend {
    async fn put(&self, id: &str, filename: &str, data: Bytes) -> Result<(), StorageError> {
        let ext = safe_extension(filename);
        let path = self
            .path_for(id, &ext)
            .ok_or_else(|| StorageError::Io("invalid storage path".into()))?;
        let _reservation = self.reserve_id(id).await?;

        let free = fs2::available_space(&self.files_dir)
            .map_err(|e| StorageError::Io(format!("disk check failed: {}", e)))?;
        if free < self.min_free_space_bytes {
            return Err(StorageError::InsufficientStorage);
        }

        self.write_local(
            id,
            &path,
            &ext,
            Box::pin(futures::stream::once(async move { Ok(data) })),
        )
        .await?;
        Ok(())
    }

    async fn put_stream(
        &self,
        id: &str,
        filename: &str,
        mut data: ByteStream,
    ) -> Result<u64, StorageError> {
        let ext = safe_extension(filename);
        let path = self
            .path_for(id, &ext)
            .ok_or_else(|| StorageError::Io("invalid storage path".into()))?;
        let _reservation = self.reserve_id(id).await?;
        let free = fs2::available_space(&self.files_dir)
            .map_err(|e| StorageError::Io(format!("disk check failed: {}", e)))?;
        if free < self.min_free_space_bytes {
            return Err(StorageError::InsufficientStorage);
        }

        let temp_path = self.temp_path(id);
        let result = async {
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .await
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        StorageError::Conflict
                    } else {
                        StorageError::Io(format!("create temp failed: {}", e))
                    }
                })?;
            let mut total = 0u64;
            while let Some(chunk) = data.next().await {
                let chunk = chunk?;
                total = total.saturating_add(chunk.len() as u64);
                tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                    .await
                    .map_err(|e| StorageError::Io(format!("write failed: {}", e)))?;
            }
            tokio::io::AsyncWriteExt::flush(&mut file)
                .await
                .map_err(|e| StorageError::Io(format!("flush failed: {}", e)))?;
            drop(file);
            publish_new_file(&temp_path, &path).await?;
            Ok(total)
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temp_path).await;
        } else {
            self.extensions.insert(id.to_string(), ext);
            self.meta_cache.remove(id);
        }
        result
    }

    async fn get(&self, id: &str) -> Result<FileData, StorageError> {
        let path = self.resolve_path(id).ok_or(StorageError::NotFound)?;

        let mut file = self.open_regular(&path).await?;
        let meta = file
            .metadata()
            .await
            .map_err(|e| StorageError::Io(format!("metadata failed: {e}")))?;

        let etag = etag_from_metadata(&meta);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin")
            .to_string();

        let mut data = Vec::with_capacity(meta.len().min(usize::MAX as u64) as usize);
        tokio::io::AsyncReadExt::read_to_end(&mut file, &mut data)
            .await
            .map_err(|e| StorageError::Io(format!("read failed: {e}")))?;

        let file_meta = FileMetadata {
            size: meta.len(),
            etag,
            extension: ext,
        };
        self.meta_cache.insert(id.to_string(), file_meta.clone());

        Ok(FileData {
            data: Bytes::from(data),
            meta: file_meta,
        })
    }

    async fn stat(&self, id: &str) -> Result<FileMetadata, StorageError> {
        self.stat_cached(id).await
    }

    async fn get_range_stream(
        &self,
        id: &str,
        start: u64,
        end: u64,
    ) -> Result<ByteStream, StorageError> {
        use tokio::io::AsyncSeekExt;

        let meta = self.stat_cached(id).await?;
        let path = self.resolve_path(id).ok_or(StorageError::NotFound)?;

        if start > meta.size || end >= meta.size {
            return Err(StorageError::Io(format!(
                "range {}-{} out of bounds for {} byte file",
                start, end, meta.size
            )));
        }

        let mut file = self.open_regular(&path).await?;
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| StorageError::Io(format!("seek failed: {}", e)))?;
        let remaining = end - start + 1;
        let stream =
            futures::stream::unfold((file, remaining), |(mut file, remaining)| async move {
                if remaining == 0 {
                    return None;
                }
                let mut buf = vec![0u8; remaining.min(64 * 1024) as usize];
                match tokio::io::AsyncReadExt::read(&mut file, &mut buf).await {
                    Ok(0) => Some((
                        Err(StorageError::Io("unexpected EOF reading range".into())),
                        (file, 0),
                    )),
                    Ok(n) => {
                        buf.truncate(n);
                        Some((Ok(Bytes::from(buf)), (file, remaining - n as u64)))
                    }
                    Err(e) => Some((
                        Err(StorageError::Io(format!("read failed: {e}"))),
                        (file, 0),
                    )),
                }
            });
        Ok(Box::pin(stream))
    }

    async fn delete(&self, id: &str) -> Result<bool, StorageError> {
        let ext = match self.extensions.get(id) {
            Some(e) => e.value().clone(),
            None => return Ok(false),
        };
        let path = self.files_dir.join(format!("{}.{}", id, ext));
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {
                self.extensions.remove(id);
                self.meta_cache.remove(id);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(StorageError::Io(format!("delete failed: {}", e))),
        }
    }

    async fn rename(&self, old_id: &str, new_id: &str) -> Result<(), StorageError> {
        let _reservation = self.reserve_id(new_id).await?;
        let ext = self
            .extensions
            .get(old_id)
            .map(|e| e.value().clone())
            .ok_or(StorageError::NotFound)?;

        let old_path = self.files_dir.join(format!("{}.{}", old_id, ext));
        let old_meta = tokio::fs::symlink_metadata(&old_path)
            .await
            .map_err(|_| StorageError::NotFound)?;
        if !old_meta.is_file() {
            return Err(StorageError::Io(
                "rename source is not a regular file".into(),
            ));
        }
        let new_path = self
            .path_for(new_id, &ext)
            .ok_or_else(|| StorageError::Io("invalid storage path".into()))?;

        tokio::fs::hard_link(&old_path, &new_path)
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    StorageError::Conflict
                } else {
                    StorageError::Io(format!("rename link failed: {}", e))
                }
            })?;
        if let Err(e) = tokio::fs::remove_file(&old_path).await {
            let _ = tokio::fs::remove_file(&new_path).await;
            return Err(StorageError::Io(format!("rename cleanup failed: {}", e)));
        }

        self.extensions.remove(old_id);
        self.extensions.insert(new_id.to_string(), ext);
        self.meta_cache.remove(old_id);
        self.meta_cache.remove(new_id);
        Ok(())
    }

    fn storage_metrics(&self, min_free_bytes: u64) -> StorageMetrics {
        let total = fs2::total_space(&self.files_dir).unwrap_or(0);
        let free = fs2::available_space(&self.files_dir).unwrap_or(0);
        let used = total.saturating_sub(free);
        StorageMetrics {
            total_bytes: total,
            used_bytes: used,
            free_bytes: free,
            min_free_bytes,
            out_of_space: free < min_free_bytes,
        }
    }

    async fn concat(
        &self,
        target_id: &str,
        filename: &str,
        part_ids: &[&str],
    ) -> Result<(), StorageError> {
        let ext = safe_extension(filename);
        let target_path = self
            .path_for(target_id, &ext)
            .ok_or_else(|| StorageError::Io("invalid storage path".into()))?;
        let _reservation = self.reserve_id(target_id).await?;
        let free = fs2::available_space(&self.files_dir)
            .map_err(|e| StorageError::Io(format!("disk check failed: {}", e)))?;
        if free < self.min_free_space_bytes {
            return Err(StorageError::InsufficientStorage);
        }

        let temp_path = self.temp_path(target_id);
        let result = async {
            let mut target_file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .await
                .map_err(|e| StorageError::Io(format!("create target failed: {}", e)))?;

            for part_id in part_ids {
                let part_path = self.resolve_path(part_id).ok_or(StorageError::NotFound)?;
                let mut part = self.open_regular(&part_path).await?;
                let meta = part
                    .metadata()
                    .await
                    .map_err(|e| StorageError::Io(format!("part metadata failed: {e}")))?;
                if !meta.is_file() {
                    return Err(StorageError::Io(format!(
                        "part {part_id} is not a regular file"
                    )));
                }
                tokio::io::copy(&mut part, &mut target_file)
                    .await
                    .map_err(|e| StorageError::Io(format!("copy part {part_id} failed: {e}")))?;
            }

            target_file
                .flush()
                .await
                .map_err(|e| StorageError::Io(format!("flush failed: {}", e)))?;
            drop(target_file);
            publish_new_file(&temp_path, &target_path).await
        }
        .await;
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(e);
        }
        self.extensions.insert(target_id.to_string(), ext);

        for part_id in part_ids {
            if let Err(e) = self.delete(part_id).await {
                tracing::warn!("concat: failed to delete part {}: {}", part_id, e);
            }
        }

        Ok(())
    }

    async fn get_stream(&self, id: &str) -> Result<ByteStream, StorageError> {
        let path = self.resolve_path(id).ok_or(StorageError::NotFound)?;
        let file = self.open_regular(&path).await?;
        let stream = futures::stream::unfold(file, |mut file| async move {
            let mut buf = vec![0u8; 64 * 1024];
            match tokio::io::AsyncReadExt::read(&mut file, &mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    Some((Ok(Bytes::from(buf)), file))
                }
                Err(e) => Some((Err(StorageError::Io(format!("read failed: {}", e))), file)),
            }
        });
        Ok(Box::pin(stream))
    }
}

impl LocalBackend {
    fn temp_path(&self, id: &str) -> PathBuf {
        self.files_dir.join(format!(
            ".{}.{}.{}.tmp",
            id,
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    async fn write_local(
        &self,
        id: &str,
        path: &PathBuf,
        ext: &str,
        mut data: ByteStream,
    ) -> Result<(), StorageError> {
        let temp_path = self.temp_path(id);
        let result = async {
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .await
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        StorageError::Conflict
                    } else {
                        StorageError::Io(format!("create temp failed: {}", e))
                    }
                })?;
            while let Some(chunk) = data.next().await {
                let chunk = chunk?;
                tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                    .await
                    .map_err(|e| StorageError::Io(format!("write failed: {}", e)))?;
            }
            tokio::io::AsyncWriteExt::flush(&mut file)
                .await
                .map_err(|e| StorageError::Io(format!("flush failed: {}", e)))?;
            drop(file);
            publish_new_file(&temp_path, path).await
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temp_path).await;
        }
        if result.is_ok() {
            self.extensions.insert(id.to_string(), ext.to_string());
            self.meta_cache.remove(id);
        }
        result
    }
}

async fn publish_new_file(temp: &PathBuf, target: &PathBuf) -> Result<(), StorageError> {
    tokio::fs::hard_link(temp, target).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            StorageError::Conflict
        } else {
            StorageError::Io(format!("publish failed: {}", e))
        }
    })?;
    if let Err(e) = tokio::fs::remove_file(temp).await {
        tracing::warn!(
            "failed to remove published temp file {}: {}",
            temp.display(),
            e
        );
    }
    Ok(())
}

/// S3-compatible backend. Objects are stored under `files/{id}.{ext}`.
pub struct S3Backend {
    /// The S3 object store client.
    client: Arc<dyn object_store::ObjectStore>,
    /// Bucket name (stored for potential future use in path construction).
    _bucket: String,
}

impl S3Backend {
    pub fn new(
        bucket: &str,
        region: &str,
        endpoint: Option<&str>,
        access_key: &str,
        secret_key: &str,
        allow_http: bool,
    ) -> Result<Self, String> {
        let mut builder = object_store::aws::AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_region(region)
            .with_access_key_id(access_key)
            .with_secret_access_key(secret_key)
            .with_copy_if_not_exists(object_store::aws::S3CopyIfNotExists::Multipart);

        if let Some(ep) = endpoint {
            if ep.starts_with("http://") && !allow_http {
                return Err("S3_ENDPOINT uses HTTP; set S3_ALLOW_HTTP=true to allow it".into());
            }
            builder = builder.with_endpoint(ep);
            builder = builder.with_allow_http(allow_http);
        }

        let client = builder
            .build()
            .map_err(|e| format!("S3 client build failed: {}", e))?;

        Ok(Self {
            client: Arc::new(client),
            _bucket: bucket.to_string(),
        })
    }

    fn object_key(id: &str, ext: &str) -> object_store::path::Path {
        object_store::path::Path::from(format!("files/{}.{}", id, ext))
    }

    fn temp_key(id: &str, ext: &str) -> object_store::path::Path {
        object_store::path::Path::from(format!(
            "files/.{}.{}.{}.{}.tmp",
            id,
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed),
            ext
        ))
    }

    fn reservation_key(id: &str) -> object_store::path::Path {
        object_store::path::Path::from(format!("files/.reservations/{id}"))
    }

    async fn reserve_id(&self, id: &str) -> Result<object_store::path::Path, StorageError> {
        if !valid_component(id) {
            return Err(StorageError::Io("invalid logical ID".into()));
        }
        let reservation = Self::reservation_key(id);
        self.client
            .put_opts(
                &reservation,
                object_store::PutPayload::from(Bytes::new()),
                object_store::PutOptions {
                    mode: object_store::PutMode::Create,
                    ..Default::default()
                },
            )
            .await
            .map_err(map_object_store_error)?;
        match self.stat(id).await {
            Ok(_) => {
                let _ = self.client.delete(&reservation).await;
                return Err(StorageError::Conflict);
            }
            Err(StorageError::NotFound) => {}
            Err(error) => {
                let _ = self.client.delete(&reservation).await;
                return Err(error);
            }
        }
        Ok(reservation)
    }

    async fn release_id(&self, reservation: &object_store::path::Path) {
        if let Err(e) = self.client.delete(reservation).await {
            tracing::warn!("failed to release S3 ID reservation {reservation}: {e}");
        }
    }
}

#[async_trait::async_trait]
impl StorageBackend for S3Backend {
    async fn put(&self, id: &str, filename: &str, data: Bytes) -> Result<(), StorageError> {
        let ext = safe_extension(filename);
        let path = Self::object_key(id, &ext);
        let reservation = self.reserve_id(id).await?;

        let payload = object_store::PutPayload::from(data);

        let result = self
            .client
            .put_opts(
                &path,
                payload,
                object_store::PutOptions {
                    mode: object_store::PutMode::Create,
                    ..Default::default()
                },
            )
            .await
            .map_err(map_object_store_error);
        self.release_id(&reservation).await;
        result.map(|_| ())
    }

    async fn put_stream(
        &self,
        id: &str,
        filename: &str,
        mut data: ByteStream,
    ) -> Result<u64, StorageError> {
        let ext = safe_extension(filename);
        let path = Self::object_key(id, &ext);
        let temp_path = Self::temp_key(id, &ext);
        let reservation = self.reserve_id(id).await?;
        let upload = self
            .client
            .put_multipart(&temp_path)
            .await
            .map_err(|e| StorageError::Io(format!("S3 multipart init failed: {}", e)));
        let upload = match upload {
            Ok(upload) => upload,
            Err(error) => {
                self.release_id(&reservation).await;
                return Err(error);
            }
        };
        let mut writer = object_store::WriteMultipart::new(upload);
        let mut total = 0u64;
        while let Some(chunk) = data.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(e) => {
                    let _ = writer.abort().await;
                    self.release_id(&reservation).await;
                    return Err(e);
                }
            };
            total = total.saturating_add(chunk.len() as u64);
            writer.put(chunk);
            if let Err(e) = writer.wait_for_capacity(4).await {
                let _ = writer.abort().await;
                self.release_id(&reservation).await;
                return Err(StorageError::Io(format!("S3 upload failed: {}", e)));
            }
        }
        if let Err(e) = writer.finish().await {
            if let Err(delete_error) = self.client.delete(&temp_path).await {
                tracing::warn!(
                    "failed to remove S3 temp object {} after upload failure: {}",
                    temp_path,
                    delete_error
                );
            }
            self.release_id(&reservation).await;
            return Err(StorageError::Io(format!("S3 upload failed: {e}")));
        }
        let publish = self
            .client
            .copy_if_not_exists(&temp_path, &path)
            .await
            .map_err(map_object_store_error);
        if let Err(e) = self.client.delete(&temp_path).await {
            tracing::warn!("failed to remove S3 temp object {}: {}", temp_path, e);
        }
        self.release_id(&reservation).await;
        publish?;
        Ok(total)
    }

    async fn get(&self, id: &str) -> Result<FileData, StorageError> {
        use futures::StreamExt;

        let prefix = object_store::path::Path::from(format!("files/{}.", id));
        let mut list = self.client.list(Some(&prefix));

        let obj = list
            .next()
            .await
            .ok_or(StorageError::NotFound)?
            .map_err(|e| StorageError::Io(format!("S3 list failed: {}", e)))?;

        let get_result = self
            .client
            .get(&obj.location)
            .await
            .map_err(|e| StorageError::Io(format!("S3 get failed: {}", e)))?;

        let obj_meta = get_result.meta.clone();
        let etag = obj_meta.e_tag.clone().unwrap_or_else(|| {
            use std::time::{SystemTime, UNIX_EPOCH};
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            format!("\"{:x}\"", ts)
        });

        let data = get_result
            .bytes()
            .await
            .map_err(|e| StorageError::Io(format!("S3 read failed: {}", e)))?;

        let key_str = obj.location.as_ref();
        let ext = key_str.rsplit('.').next().unwrap_or("bin").to_string();

        Ok(FileData {
            data,
            meta: FileMetadata {
                size: obj_meta.size,
                etag,
                extension: ext,
            },
        })
    }

    async fn delete(&self, id: &str) -> Result<bool, StorageError> {
        use futures::StreamExt;

        let prefix = object_store::path::Path::from(format!("files/{}.", id));
        let mut list = self.client.list(Some(&prefix));

        let obj: object_store::ObjectMeta = match list.next().await {
            Some(Ok(obj)) => obj,
            _ => return Ok(false),
        };

        self.client
            .delete(&obj.location)
            .await
            .map_err(|e| StorageError::Io(format!("S3 delete failed: {}", e)))?;

        Ok(true)
    }

    async fn rename(&self, old_id: &str, new_id: &str) -> Result<(), StorageError> {
        let meta = self.stat(old_id).await?;
        let source = Self::object_key(old_id, &meta.extension);
        let target = Self::object_key(new_id, &meta.extension);
        let reservation = self.reserve_id(new_id).await?;
        let result = self
            .client
            .copy_if_not_exists(&source, &target)
            .await
            .map_err(map_object_store_error);
        self.release_id(&reservation).await;
        result?;
        self.delete(old_id).await?;
        Ok(())
    }

    async fn stat(&self, id: &str) -> Result<FileMetadata, StorageError> {
        use futures::StreamExt;

        let prefix = object_store::path::Path::from(format!("files/{}.", id));
        let mut list = self.client.list(Some(&prefix));

        let obj = list
            .next()
            .await
            .ok_or(StorageError::NotFound)?
            .map_err(|e| StorageError::Io(format!("S3 list failed: {}", e)))?;

        let key_str = obj.location.as_ref();
        let ext = key_str.rsplit('.').next().unwrap_or("bin").to_string();
        let etag = obj.e_tag.clone().unwrap_or_else(|| {
            use std::time::{SystemTime, UNIX_EPOCH};
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            format!("\"{:x}\"", ts)
        });

        Ok(FileMetadata {
            size: obj.size as u64,
            etag,
            extension: ext,
        })
    }

    async fn get_range_stream(
        &self,
        id: &str,
        start: u64,
        end: u64,
    ) -> Result<ByteStream, StorageError> {
        use futures::StreamExt;

        self.stat(id).await?;
        let prefix = object_store::path::Path::from(format!("files/{}.", id));
        let mut list = self.client.list(Some(&prefix));
        let obj = list
            .next()
            .await
            .ok_or(StorageError::NotFound)?
            .map_err(|e| StorageError::Io(format!("S3 list failed: {}", e)))?;

        let result = self
            .client
            .get_opts(
                &obj.location,
                object_store::GetOptions {
                    range: Some(object_store::GetRange::Bounded(start..end + 1)),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| StorageError::Io(format!("S3 range get failed: {}", e)))?;
        let stream = result.into_stream().map(|result| {
            result.map_err(|e| StorageError::Io(format!("S3 range read failed: {e}")))
        });
        Ok(Box::pin(stream))
    }

    async fn get_stream(&self, id: &str) -> Result<ByteStream, StorageError> {
        let prefix = object_store::path::Path::from(format!("files/{}.", id));
        let mut list = self.client.list(Some(&prefix));
        let obj = list
            .next()
            .await
            .ok_or(StorageError::NotFound)?
            .map_err(|e| StorageError::Io(format!("S3 list failed: {}", e)))?;
        let stream = self
            .client
            .get(&obj.location)
            .await
            .map_err(|e| StorageError::Io(format!("S3 get failed: {}", e)))?
            .into_stream()
            .map(|result| result.map_err(|e| StorageError::Io(format!("S3 read failed: {}", e))));
        Ok(Box::pin(stream))
    }

    fn storage_metrics(&self, _min_free_bytes: u64) -> StorageMetrics {
        StorageMetrics {
            total_bytes: 0,
            used_bytes: 0,
            free_bytes: u64::MAX,
            min_free_bytes: _min_free_bytes,
            out_of_space: false,
        }
    }

    async fn concat(
        &self,
        target_id: &str,
        filename: &str,
        part_ids: &[&str],
    ) -> Result<(), StorageError> {
        use futures::StreamExt;

        let ext = safe_extension(filename);
        let reservation = self.reserve_id(target_id).await?;
        let target_path = Self::object_key(target_id, &ext);
        let temp_path = Self::temp_key(target_id, &ext);
        let upload = match self.client.put_multipart(&temp_path).await {
            Ok(upload) => upload,
            Err(e) => {
                self.release_id(&reservation).await;
                return Err(StorageError::Io(format!("S3 multipart init failed: {e}")));
            }
        };
        let mut writer = object_store::WriteMultipart::new(upload);
        let upload_result = async {
            for part_id in part_ids {
                let prefix = object_store::path::Path::from(format!("files/{}.", part_id));
                let mut list = self.client.list(Some(&prefix));
                let obj = list
                    .next()
                    .await
                    .ok_or(StorageError::NotFound)?
                    .map_err(|e| {
                        StorageError::Io(format!("S3 list failed for {}: {}", part_id, e))
                    })?;

                let get_result = self.client.get(&obj.location).await.map_err(|e| {
                    StorageError::Io(format!("S3 get failed for {}: {}", part_id, e))
                })?;

                let mut stream = get_result.into_stream();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| {
                        StorageError::Io(format!("S3 read failed for {part_id}: {e}"))
                    })?;
                    writer.put(chunk);
                    writer
                        .wait_for_capacity(4)
                        .await
                        .map_err(|e| StorageError::Io(format!("S3 concat upload failed: {e}")))?;
                }
            }
            Ok::<(), StorageError>(())
        }
        .await;
        if let Err(error) = upload_result {
            let _ = writer.abort().await;
            self.release_id(&reservation).await;
            return Err(error);
        }
        if let Err(e) = writer.finish().await {
            if let Err(delete_error) = self.client.delete(&temp_path).await {
                tracing::warn!(
                    "failed to remove S3 temp object {} after concat failure: {}",
                    temp_path,
                    delete_error
                );
            }
            self.release_id(&reservation).await;
            return Err(StorageError::Io(format!("S3 concat upload failed: {e}")));
        }
        let publish = self
            .client
            .copy_if_not_exists(&temp_path, &target_path)
            .await
            .map_err(map_object_store_error);
        let _ = self.client.delete(&temp_path).await;
        self.release_id(&reservation).await;
        publish?;

        for part_id in part_ids {
            if let Err(e) = self.delete(part_id).await {
                tracing::warn!("concat: failed to delete part {}: {}", part_id, e);
            }
        }

        Ok(())
    }
}

fn map_object_store_error(error: object_store::Error) -> StorageError {
    match error {
        object_store::Error::AlreadyExists { .. } | object_store::Error::Precondition { .. } => {
            StorageError::Conflict
        }
        error => StorageError::Io(error.to_string()),
    }
}

// Helper functions and stuff

pub fn extract_extension(filename: &str) -> &str {
    std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn safe_extension(filename: &str) -> String {
    let ext = extract_extension(filename);
    if valid_component(ext) {
        ext.to_ascii_lowercase()
    } else {
        "bin".into()
    }
}

fn etag_from_metadata(meta: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!(
            "\"{:x}-{:x}-{:x}\"",
            meta.ino(),
            meta.mtime() as u64,
            meta.len()
        )
    }
    #[cfg(not(unix))]
    {
        format!(
            "\"{:x}-{:x}\"",
            meta.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
            meta.len()
        )
    }
}

/// Guess the MIME type from a file extension.
pub fn guess_mime(ext: &str) -> String {
    let mime = mime_guess::from_path(format!("file.{}", ext)).first_or_octet_stream();
    if mime.type_() == mime_guess::mime::TEXT {
        format!("{}; charset=utf-8", mime)
    } else {
        mime.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_extension_with_ext() {
        assert_eq!(extract_extension("test.png"), "png");
    }

    #[test]
    fn extract_extension_no_ext() {
        assert_eq!(extract_extension("noext"), "bin");
    }

    #[test]
    fn extract_extension_double_ext() {
        assert_eq!(extract_extension("file.tar.gz"), "gz");
    }

    #[test]
    fn guess_mime_txt() {
        let mime = guess_mime("txt");
        assert!(mime.starts_with("text/plain"));
        assert!(mime.contains("charset=utf-8"));
    }

    #[test]
    fn guess_mime_html() {
        let mime = guess_mime("html");
        assert!(mime.starts_with("text/html"));
    }

    #[test]
    fn guess_mime_png() {
        assert_eq!(guess_mime("png"), "image/png");
    }

    #[test]
    fn guess_mime_unknown() {
        let mime = guess_mime("xyz");
        assert!(!mime.is_empty());
    }

    #[test]
    fn guess_mime_no_ext() {
        assert_eq!(guess_mime("bin"), "application/octet-stream");
    }

    #[test]
    fn s3_object_key() {
        let key = S3Backend::object_key("abc123", "png");
        assert_eq!(key.as_ref(), "files/abc123.png");
    }

    async fn setup_local_backend() -> (tempfile::TempDir, LocalBackend) {
        let dir = tempfile::tempdir().unwrap();
        let backend = LocalBackend::new(dir.path().to_path_buf(), 0).unwrap();
        (dir, backend)
    }

    #[tokio::test]
    async fn local_put_and_get() {
        let (_dir, backend) = setup_local_backend().await;
        backend
            .put("file1", "test.txt", Bytes::from("hello"))
            .await
            .unwrap();
        let data = backend.get("file1").await.unwrap();
        assert_eq!(data.data.as_ref(), b"hello");
        assert_eq!(data.meta.extension, "txt");
    }

    #[tokio::test]
    async fn local_put_does_not_overwrite_existing_file() {
        let (_dir, backend) = setup_local_backend().await;
        backend
            .put("file1", "test.txt", Bytes::from("first"))
            .await
            .unwrap();
        assert!(matches!(
            backend
                .put("file1", "test.txt", Bytes::from("second"))
                .await,
            Err(StorageError::Conflict)
        ));
        assert_eq!(backend.get("file1").await.unwrap().data.as_ref(), b"first");
    }

    #[tokio::test]
    async fn local_logical_id_is_unique_across_extensions() {
        let (_dir, backend) = setup_local_backend().await;
        backend
            .put("same", "first.txt", Bytes::from_static(b"first"))
            .await
            .unwrap();
        assert!(matches!(
            backend
                .put("same", "second.png", Bytes::from_static(b"second"))
                .await,
            Err(StorageError::Conflict)
        ));
        assert_eq!(backend.stat("same").await.unwrap().extension, "txt");
    }

    #[tokio::test]
    async fn local_logical_id_reservation_is_atomic_across_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(LocalBackend::new(dir.path().to_path_buf(), 0).unwrap());
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let mut tasks = Vec::new();
        for filename in ["first.txt", "second.png"] {
            let backend = Arc::clone(&backend);
            let barrier = Arc::clone(&barrier);
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                backend
                    .put("raced", filename, Bytes::from_static(b"data"))
                    .await
            }));
        }
        barrier.wait().await;
        let results = futures::future::join_all(tasks).await;
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Ok(Ok(()))))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Ok(Err(StorageError::Conflict))))
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_cache_rejects_symlink_entries() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), dir.path().join("linked.txt")).unwrap();
        let backend = LocalBackend::new(dir.path().to_path_buf(), 0).unwrap();
        assert!(backend.init_cache().await.is_err());
    }

    #[tokio::test]
    async fn local_stream_failure_removes_partial_file() {
        let (dir, backend) = setup_local_backend().await;
        let stream = futures::stream::iter([
            Ok(Bytes::from_static(b"partial")),
            Err(StorageError::Io("request failed".into())),
        ]);
        assert!(backend
            .put_stream("broken", "file.txt", Box::pin(stream))
            .await
            .is_err());
        assert!(backend.stat("broken").await.is_err());
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn local_stream_does_not_overwrite_existing_file() {
        let (_dir, backend) = setup_local_backend().await;
        backend
            .put("file1", "test.txt", Bytes::from("first"))
            .await
            .unwrap();
        let stream = futures::stream::once(async { Ok(Bytes::from_static(b"second")) });
        assert!(matches!(
            backend
                .put_stream("file1", "test.txt", Box::pin(stream))
                .await,
            Err(StorageError::Conflict)
        ));
        assert_eq!(backend.get("file1").await.unwrap().data.as_ref(), b"first");
    }

    #[tokio::test]
    async fn local_delete() {
        let (_dir, backend) = setup_local_backend().await;
        backend
            .put("d1", "f.bin", Bytes::from("data"))
            .await
            .unwrap();
        let deleted = backend.delete("d1").await.unwrap();
        assert!(deleted);
        assert!(backend.get("d1").await.is_err());
    }

    #[tokio::test]
    async fn local_delete_not_found() {
        let (_dir, backend) = setup_local_backend().await;
        let deleted = backend.delete("nonexistent").await.unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn local_rename() {
        let (_dir, backend) = setup_local_backend().await;
        backend
            .put("old", "f.txt", Bytes::from("content"))
            .await
            .unwrap();
        backend.rename("old", "new").await.unwrap();
        assert!(backend.get("old").await.is_err());
        let data = backend.get("new").await.unwrap();
        assert_eq!(data.data.as_ref(), b"content");
    }

    #[tokio::test]
    async fn local_rename_not_found() {
        let (_dir, backend) = setup_local_backend().await;
        let result = backend.rename("missing", "new").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn local_get_not_found() {
        let (_dir, backend) = setup_local_backend().await;
        let result = backend.get("nope").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn local_concat() {
        let (_dir, backend) = setup_local_backend().await;
        backend
            .put("p1", "a.txt", Bytes::from("hello"))
            .await
            .unwrap();
        backend
            .put("p2", "b.txt", Bytes::from(" world"))
            .await
            .unwrap();
        backend
            .concat("merged", "merged.txt", &["p1", "p2"])
            .await
            .unwrap();
        let data = backend.get("merged").await.unwrap();
        assert_eq!(data.data.as_ref(), b"hello world");
        assert!(backend.get("p1").await.is_err());
        assert!(backend.get("p2").await.is_err());
    }

    #[tokio::test]
    async fn local_put_stores_file() {
        let dir = tempfile::tempdir().unwrap();
        let backend = LocalBackend::new(dir.path().to_path_buf(), 0).unwrap();
        backend
            .put("deep-id", "file.txt", Bytes::from("data"))
            .await
            .unwrap();
        let data = backend.get("deep-id").await.unwrap();
        assert_eq!(data.data.as_ref(), b"data");
    }

    #[tokio::test]
    async fn local_storage_metrics() {
        let (_dir, backend) = setup_local_backend().await;
        let metrics = backend.storage_metrics(0);
        assert!(metrics.total_bytes > 0);
        assert!(!metrics.out_of_space);
    }

    #[tokio::test]
    async fn local_cache_init() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("abc.txt"), "data").unwrap();
        let backend = LocalBackend::new(dir.path().to_path_buf(), 0).unwrap();
        backend.init_cache().await.unwrap();
        assert_eq!(
            backend.extensions.get("abc").map(|e| e.value().clone()),
            Some("txt".into())
        );
    }
}
