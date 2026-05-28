//! The cache contract.
//!
//! Offline reading is a first-class requirement, so the engine reads and writes
//! through this trait rather than a concrete database. The desktop frontend backs
//! it with `redb`; because it's a *cache*, swapping backends (rusqlite, a Vita SD
//! store, …) is a new impl, and "migration" is just a re-fetch.

use serde::{Deserialize, Serialize};

use crate::model::{Document, Publication, RichDoc};

/// A cached document: its metadata, optionally its decoded body, and read-state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredDoc {
    pub meta: Document,
    pub body: Option<RichDoc>,
    pub read: bool,
}

/// Persistent, offline-capable cache for publications, documents, read-state, and
/// image blobs. All methods are synchronous (see [`crate::atp::Transport`]).
pub trait Store {
    type Error: std::error::Error + Send + Sync + 'static;

    fn upsert_publication(&mut self, publication: &Publication) -> Result<(), Self::Error>;
    fn publication(&self, uri: &str) -> Result<Option<Publication>, Self::Error>;

    fn upsert_document(
        &mut self,
        meta: &Document,
        body: Option<&RichDoc>,
    ) -> Result<(), Self::Error>;
    fn document(&self, uri: &str) -> Result<Option<StoredDoc>, Self::Error>;
    /// Documents for a publication, newest first.
    fn documents_for(&self, publication_uri: &str) -> Result<Vec<Document>, Self::Error>;

    fn set_read(&mut self, uri: &str, read: bool) -> Result<(), Self::Error>;

    /// Full-text-ish search over cached `textContent`; returns matching doc URIs.
    fn search(&self, query: &str) -> Result<Vec<String>, Self::Error>;

    fn put_blob(&mut self, cid: &str, bytes: &[u8]) -> Result<(), Self::Error>;
    fn blob(&self, cid: &str) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Incremental-sync high-water mark for a publication — the newest document URI seen, so a
    /// refresh can stop once it walks back to already-cached records. Opaque to the store.
    fn sync_cursor(&self, publication_uri: &str) -> Result<Option<String>, Self::Error>;
    fn set_sync_cursor(&mut self, publication_uri: &str, cursor: &str) -> Result<(), Self::Error>;
}
