//! Where files live (uploads, pictures): any `object_store` store, by URL: `file:///<dir>` on one node,
//! `s3://<bucket>[/<prefix>]` shared by several, `memory:///` for tests.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt as _, PutPayload};
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("not a storage URL the server knows: {0}")]
    Url(String),
    #[error("not a valid object key")]
    Key,
    #[error("storage failed: {0}")]
    Store(#[from] object_store::Error),
}

/// The files of the server, by key (`/`-separated, no `.` or `..` parts).
#[async_trait]
pub trait Storage: Send + Sync + std::fmt::Debug {
    /// Stores `bytes` under `key`, replacing what was there.
    async fn put(&self, key: &str, bytes: Bytes) -> Result<(), StorageError>;

    /// The bytes under `key`, if any.
    async fn get(&self, key: &str) -> Result<Option<Bytes>, StorageError>;

    /// Removes `key`; removing a missing key is no error.
    async fn delete(&self, key: &str) -> Result<(), StorageError>;
}

/// A [`Storage`] over an `object_store` store.
#[derive(Debug)]
pub struct ObjectStorage {
    store: Arc<dyn ObjectStore>,
    prefix: Path,
}

impl ObjectStorage {
    /// The store `url` names, with `options` (S3's region, endpoint and keys, as `object_store` names them). A
    /// `file://` directory is made when missing.
    ///
    /// # Errors
    /// [`StorageError::Url`] for a URL `object_store` doesn't take.
    pub fn open<'a>(
        url: &str,
        options: impl IntoIterator<Item = (&'a String, &'a String)>,
    ) -> Result<Self, StorageError> {
        let parsed = Url::parse(url).map_err(|_| StorageError::Url(url.to_owned()))?;
        if parsed.scheme() == "file" {
            let dir = parsed.to_file_path().map_err(|()| StorageError::Url(url.to_owned()))?;
            std::fs::create_dir_all(&dir).map_err(|error| StorageError::Url(error.to_string()))?;
        }
        let options = options.into_iter().map(|(key, value)| (key.clone(), value.clone()));
        let (store, prefix) =
            object_store::parse_url_opts(&parsed, options).map_err(|error| StorageError::Url(error.to_string()))?;
        Ok(Self {
            store: Arc::from(store),
            prefix,
        })
    }

    fn path(&self, key: &str) -> Result<Path, StorageError> {
        let key = Path::parse(key).map_err(|_| StorageError::Key)?;
        if key.as_ref().is_empty() {
            return Err(StorageError::Key);
        }
        Ok(self.prefix.parts().chain(key.parts()).collect())
    }
}

#[async_trait]
impl Storage for ObjectStorage {
    async fn put(&self, key: &str, bytes: Bytes) -> Result<(), StorageError> {
        self.store.put(&self.path(key)?, PutPayload::from(bytes)).await?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>, StorageError> {
        match self.store.get(&self.path(key)?).await {
            Ok(found) => Ok(Some(found.bytes().await?)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        match self.store.delete(&self.path(key)?).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}
