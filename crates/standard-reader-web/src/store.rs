//! The web [`Store`] + [`FrontendStore`]: an in-memory cache (HashMaps) that **persists to OPFS**.
//!
//! Reads/writes stay in RAM (synchronous, [`Infallible`]) — the worker calls them on its blocked
//! thread, which can't await. Persistence is layered on top: every mutator additively emits a
//! [`PersistOp`] over a channel to the main thread, which does the async OPFS I/O (see
//! [`crate::persist`]). At startup the store is hydrated from an [`InitialState`] the main thread
//! loaded from OPFS *before* `worker::spawn`, so cache-first reads serve offline with no worker
//! change. Op emission is best-effort: a dropped receiver (OPFS unavailable) just means no-persist.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::mpsc::Sender;

use serde::{Deserialize, Serialize};
use standard_core::model::{Document, Publication, RichDoc};
use standard_core::search::tokenize;
use standard_core::store::{Store, StoredDoc};
use standard_frontend::frontend_store::FrontendStore;

use crate::persist::{blob_file_key, body_key};

/// Bumped when the serialized `RichDoc` body shape changes (decoder upgrades) — mirrors the desktop
/// store's `CACHE_SCHEMA` (`crates/standard-tui/src/store.rs`). On a mismatch, [`InitialState`] drops
/// cached bodies + walk cursors (re-fetched) while keeping format-stable publications/follows/blobs.
pub const CACHE_SCHEMA: &str = "1";

/// A persistence op handed to the main thread (the only place async OPFS I/O can run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistOp {
    /// The full re-serialized `index.json` (structured snapshot sans bodies). Idempotent as a
    /// snapshot → the main thread coalesces to the latest before writing.
    Index(Vec<u8>),
    /// One document's serialized `RichDoc` body → `b/<key>` (write-on-change).
    Body { key: String, bytes: Vec<u8> },
    /// One image blob → `i/<cid>` (write-once; CIDs are filesystem-safe).
    Blob { cid: String, bytes: Vec<u8> },
    /// The complete user-appearance preferences → `prefs.json` (latest snapshot wins).
    Prefs(Vec<u8>),
}

/// One document's row in `index.json`: metadata + read-state, **without** the body (bodies live in
/// their own `b/<key>` files, so the index stays small + cheap to re-serialize on every change).
#[derive(Debug, Serialize, Deserialize)]
struct DocEntry {
    meta: Document,
    read: bool,
}

/// The on-disk `index.json`: the whole structured store minus document bodies + blob bytes. The
/// `docs`/`blob_cids` lists let the loader enumerate the per-record `b/`/`i/` files without an OPFS
/// directory scan (which web-sys can't do ergonomically).
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexDto {
    schema: String,
    publications: Vec<Publication>,
    /// (publication uri, atproto rkey "" = unknown).
    follows: Vec<(String, String)>,
    sync_cursors: Vec<(String, String)>,
    older_cursors: Vec<(String, String)>,
    show_images: bool,
    docs: Vec<DocEntry>,
    blob_cids: Vec<String>,
}

impl IndexDto {
    /// Whether the cached bodies/cursors are still compatible (else the loader drops them).
    pub fn schema_matches(&self) -> bool {
        self.schema == CACHE_SCHEMA
    }

    /// The `b/<key>` body files this index references (so the loader knows what to read).
    pub fn body_keys(&self) -> Vec<String> {
        self.docs.iter().map(|d| body_key(&d.meta.uri)).collect()
    }

    /// The `i/<cid>` blob files this index references.
    pub fn blob_cids(&self) -> &[String] {
        &self.blob_cids
    }
}

/// The hydrated cache the main thread loads from OPFS and hands to [`MemStore::new`] before the
/// worker starts. `Default` (empty + `show_images: true`) is a fresh / OPFS-unavailable start.
pub struct InitialState {
    pub publications: HashMap<String, Publication>,
    pub documents: HashMap<String, StoredDoc>,
    pub blobs: HashMap<String, Vec<u8>>,
    pub follows: HashMap<String, String>,
    pub sync_cursors: HashMap<String, String>,
    pub older_cursors: HashMap<String, String>,
    pub show_images: bool,
}

impl Default for InitialState {
    fn default() -> Self {
        Self {
            publications: HashMap::new(),
            documents: HashMap::new(),
            blobs: HashMap::new(),
            follows: HashMap::new(),
            sync_cursors: HashMap::new(),
            older_cursors: HashMap::new(),
            show_images: true,
        }
    }
}

impl InitialState {
    /// Rebuild the cache from a loaded `index.json` plus already-read body/blob bytes. On a schema
    /// mismatch, keep publications/follows/show_images/blobs (format-stable) but drop bodies +
    /// cursors so the worker re-lists + re-decodes. `bodies` is keyed by [`body_key`]`(meta.uri)`.
    pub fn from_index(
        dto: IndexDto,
        bodies: &HashMap<String, Vec<u8>>,
        blobs: HashMap<String, Vec<u8>>,
    ) -> Self {
        let schema_ok = dto.schema == CACHE_SCHEMA;
        let documents: HashMap<String, StoredDoc> = dto
            .docs
            .into_iter()
            .map(|d| {
                let body = if schema_ok {
                    bodies
                        .get(&body_key(&d.meta.uri))
                        .and_then(|b| serde_json::from_slice::<RichDoc>(b).ok())
                } else {
                    None
                };
                (
                    d.meta.uri.clone(),
                    StoredDoc {
                        meta: d.meta,
                        body,
                        read: d.read,
                    },
                )
            })
            .collect();
        Self {
            publications: dto
                .publications
                .into_iter()
                .map(|p| (p.uri.clone(), p))
                .collect(),
            documents,
            blobs,
            follows: dto.follows.into_iter().collect(),
            sync_cursors: if schema_ok {
                dto.sync_cursors.into_iter().collect()
            } else {
                HashMap::new()
            },
            older_cursors: if schema_ok {
                dto.older_cursors.into_iter().collect()
            } else {
                HashMap::new()
            },
            show_images: dto.show_images,
        }
    }
}

pub struct MemStore {
    publications: HashMap<String, Publication>,
    documents: HashMap<String, StoredDoc>,
    blobs: HashMap<String, Vec<u8>>,
    /// publication uri → atproto rkey ("" when unknown / local-only).
    follows: HashMap<String, String>,
    /// publication uri → incremental sync high-water cursor.
    sync_cursors: HashMap<String, String>,
    /// repo DID → "load older" cursor.
    older_cursors: HashMap<String, String>,
    show_images: bool,
    /// Persist ops to the main thread (best-effort; a dropped receiver = no-persist).
    persist_tx: Sender<PersistOp>,
}

impl MemStore {
    /// Build the store from a hydrated [`InitialState`] (loaded from OPFS) + the persist channel.
    pub fn new(persist_tx: Sender<PersistOp>, initial: InitialState) -> Self {
        Self {
            publications: initial.publications,
            documents: initial.documents,
            blobs: initial.blobs,
            follows: initial.follows,
            sync_cursors: initial.sync_cursors,
            older_cursors: initial.older_cursors,
            show_images: initial.show_images,
            persist_tx,
        }
    }

    /// Re-serialize the structured snapshot (no bodies) and hand it to the main thread to write.
    /// Called after every structured mutation; cheap (metas only) and coalesced on the far side.
    fn emit_index(&self) {
        let dto = IndexDto {
            schema: CACHE_SCHEMA.to_string(),
            publications: self.publications.values().cloned().collect(),
            follows: self
                .follows
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            sync_cursors: self
                .sync_cursors
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            older_cursors: self
                .older_cursors
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            show_images: self.show_images,
            docs: self
                .documents
                .values()
                .map(|d| DocEntry {
                    meta: d.meta.clone(),
                    read: d.read,
                })
                .collect(),
            blob_cids: self.blobs.keys().cloned().collect(),
        };
        if let Ok(bytes) = serde_json::to_vec(&dto) {
            let _ = self.persist_tx.send(PersistOp::Index(bytes));
        }
    }
}

impl Store for MemStore {
    type Error = Infallible;

    fn upsert_publication(&mut self, publication: &Publication) -> Result<(), Infallible> {
        self.publications
            .insert(publication.uri.clone(), publication.clone());
        self.emit_index();
        Ok(())
    }

    fn publication(&self, uri: &str) -> Result<Option<Publication>, Infallible> {
        Ok(self.publications.get(uri).cloned())
    }

    fn upsert_document(
        &mut self,
        meta: &Document,
        body: Option<&RichDoc>,
    ) -> Result<(), Infallible> {
        // Preserve read-state across a re-fetch.
        let read = self
            .documents
            .get(&meta.uri)
            .map(|d| d.read)
            .unwrap_or(false);
        self.documents.insert(
            meta.uri.clone(),
            StoredDoc {
                meta: meta.clone(),
                body: body.cloned(),
                read,
            },
        );
        // Persist the body to its own `b/<key>` file when one arrived (bodies aren't in the index).
        if let Some(body) = body
            && let Ok(bytes) = serde_json::to_vec(body)
        {
            let _ = self.persist_tx.send(PersistOp::Body {
                key: body_key(&meta.uri),
                bytes,
            });
        }
        self.emit_index();
        Ok(())
    }

    fn document(&self, uri: &str) -> Result<Option<StoredDoc>, Infallible> {
        Ok(self.documents.get(uri).cloned())
    }

    fn documents_for(&self, publication_uri: &str) -> Result<Vec<Document>, Infallible> {
        let mut out: Vec<Document> = self
            .documents
            .values()
            .filter(|d| d.meta.publication == publication_uri)
            .map(|d| d.meta.clone())
            .collect();
        // Newest first (RFC-3339 sorts lexically) — matches the redb store.
        out.sort_by(|a, b| b.published_at.cmp(&a.published_at));
        Ok(out)
    }

    fn set_read(&mut self, uri: &str, read: bool) -> Result<(), Infallible> {
        if let Some(doc) = self.documents.get_mut(uri) {
            doc.read = read;
        }
        self.emit_index();
        Ok(())
    }

    fn read_uris(&self, publication_uri: &str) -> Result<Vec<String>, Infallible> {
        Ok(self
            .documents
            .values()
            .filter(|d| d.meta.publication == publication_uri && d.read)
            .map(|d| d.meta.uri.clone())
            .collect())
    }

    fn unread_count(&self, publication_uri: &str) -> Result<usize, Infallible> {
        Ok(self
            .documents
            .values()
            .filter(|d| d.meta.publication == publication_uri && !d.read)
            .count())
    }

    fn search(&self, query: &str) -> Result<Vec<String>, Infallible> {
        let terms: Vec<String> = tokenize(query).collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits = Vec::new();
        for (uri, doc) in &self.documents {
            if let Some(text) = &doc.meta.text_content {
                let doc_terms: HashSet<String> = tokenize(text).collect();
                if terms.iter().all(|t| doc_terms.contains(t)) {
                    hits.push(uri.clone());
                }
            }
        }
        Ok(hits)
    }

    fn put_blob(&mut self, cid: &str, bytes: &[u8]) -> Result<(), Infallible> {
        self.blobs.insert(cid.to_string(), bytes.to_vec());
        let _ = self.persist_tx.send(PersistOp::Blob {
            cid: blob_file_key(cid),
            bytes: bytes.to_vec(),
        });
        // Re-emit the index so the new cid lands in `blob_cids` (so it's found on next load).
        self.emit_index();
        Ok(())
    }

    fn blob(&self, cid: &str) -> Result<Option<Vec<u8>>, Infallible> {
        Ok(self.blobs.get(cid).cloned())
    }

    fn sync_cursor(&self, publication_uri: &str) -> Result<Option<String>, Infallible> {
        Ok(self.sync_cursors.get(publication_uri).cloned())
    }

    fn set_sync_cursor(&mut self, publication_uri: &str, cursor: &str) -> Result<(), Infallible> {
        self.sync_cursors
            .insert(publication_uri.to_string(), cursor.to_string());
        self.emit_index();
        Ok(())
    }

    fn older_cursor(&self, did: &str) -> Result<Option<String>, Infallible> {
        Ok(self.older_cursors.get(did).cloned())
    }

    fn set_older_cursor(&mut self, did: &str, cursor: &str) -> Result<(), Infallible> {
        self.older_cursors
            .insert(did.to_string(), cursor.to_string());
        self.emit_index();
        Ok(())
    }
}

impl FrontendStore for MemStore {
    fn follows(&self) -> Result<Vec<String>, Infallible> {
        Ok(self.follows.keys().cloned().collect())
    }

    fn is_followed(&self, publication_uri: &str) -> Result<bool, Infallible> {
        Ok(self.follows.contains_key(publication_uri))
    }

    fn follow(&mut self, publication_uri: &str) -> Result<(), Infallible> {
        // Don't clobber an existing rkey when re-following.
        self.follows.entry(publication_uri.to_string()).or_default();
        self.emit_index();
        Ok(())
    }

    fn unfollow(&mut self, publication_uri: &str) -> Result<(), Infallible> {
        self.follows.remove(publication_uri);
        self.emit_index();
        Ok(())
    }

    fn follow_rkey(&self, publication_uri: &str) -> Result<Option<String>, Infallible> {
        Ok(self
            .follows
            .get(publication_uri)
            .filter(|r| !r.is_empty())
            .cloned())
    }

    fn set_follow_rkey(&mut self, publication_uri: &str, rkey: &str) -> Result<(), Infallible> {
        self.follows
            .insert(publication_uri.to_string(), rkey.to_string());
        self.emit_index();
        Ok(())
    }

    fn show_images(&self) -> Result<bool, Infallible> {
        Ok(self.show_images)
    }

    fn set_show_images(&mut self, on: bool) -> Result<(), Infallible> {
        self.show_images = on;
        self.emit_index();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Receiver, channel};

    use standard_core::model::{Block, Inline};

    type EmittedState = (IndexDto, HashMap<String, Vec<u8>>, HashMap<String, Vec<u8>>);

    fn publication() -> Publication {
        Publication {
            uri: "at://did:plc:test/site.standard.publication/main".into(),
            url: "https://example.test".into(),
            name: "Example".into(),
            description: Some("A test publication".into()),
            icon: None,
        }
    }

    fn document(publication: &str) -> Document {
        Document {
            uri: "at://did:plc:test/site.standard.document/post".into(),
            title: "Post".into(),
            description: None,
            publication: publication.into(),
            published_at: "2026-07-20T12:00:00Z".into(),
            updated_at: None,
            publishing_platform: None,
            cover_image: None,
            text_content: Some("offline searchable text".into()),
            tags: vec!["test".into()],
            path: Some("/post".into()),
        }
    }

    fn body() -> RichDoc {
        RichDoc {
            blocks: vec![Block::Paragraph(vec![Inline::Strong(vec![Inline::Text(
                "offline body".into(),
            )])])],
        }
    }

    fn fresh_store() -> (MemStore, Receiver<PersistOp>) {
        let (tx, rx) = channel();
        (MemStore::new(tx, InitialState::default()), rx)
    }

    fn emitted_state(rx: &Receiver<PersistOp>) -> EmittedState {
        let mut index = None;
        let mut bodies = HashMap::new();
        let mut blobs = HashMap::new();
        for op in rx.try_iter() {
            match op {
                PersistOp::Index(bytes) => index = serde_json::from_slice(&bytes).ok(),
                PersistOp::Body { key, bytes } => {
                    bodies.insert(key, bytes);
                }
                PersistOp::Blob { cid, bytes } => {
                    blobs.insert(cid, bytes);
                }
                PersistOp::Prefs(_) => {}
            }
        }
        (index.expect("an index snapshot"), bodies, blobs)
    }

    #[test]
    fn complete_cache_snapshot_hydrates_offline_state() {
        let (mut store, rx) = fresh_store();
        let publication = publication();
        let document = document(&publication.uri);
        let body = body();

        store.upsert_publication(&publication).unwrap();
        store.follow(&publication.uri).unwrap();
        store.set_follow_rkey(&publication.uri, "3krkey").unwrap();
        store.upsert_document(&document, Some(&body)).unwrap();
        store.set_read(&document.uri, true).unwrap();
        store.put_blob("bafycid", &[1, 2, 3]).unwrap();
        store.set_sync_cursor(&publication.uri, "newest").unwrap();
        store.set_older_cursor("did:plc:test", "older").unwrap();
        store.set_show_images(false).unwrap();

        let (index, bodies, blobs) = emitted_state(&rx);
        let initial = InitialState::from_index(index, &bodies, blobs);
        let (tx, _rx) = channel();
        let hydrated = MemStore::new(tx, initial);

        assert_eq!(
            hydrated.publication(&publication.uri).unwrap(),
            Some(publication)
        );
        assert_eq!(
            hydrated.document(&document.uri).unwrap().unwrap().body,
            Some(body)
        );
        assert!(hydrated.document(&document.uri).unwrap().unwrap().read);
        assert_eq!(hydrated.blob("bafycid").unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(
            hydrated
                .follow_rkey(&document.publication)
                .unwrap()
                .as_deref(),
            Some("3krkey")
        );
        assert_eq!(
            hydrated
                .sync_cursor(&document.publication)
                .unwrap()
                .as_deref(),
            Some("newest")
        );
        assert_eq!(
            hydrated.older_cursor("did:plc:test").unwrap().as_deref(),
            Some("older")
        );
        assert!(!hydrated.show_images().unwrap());
        assert_eq!(
            hydrated.search("offline searchable").unwrap(),
            vec![document.uri]
        );
    }

    #[test]
    fn schema_mismatch_keeps_stable_state_but_invalidates_bodies_and_cursors() {
        let (mut store, rx) = fresh_store();
        let publication = publication();
        let document = document(&publication.uri);
        store.upsert_publication(&publication).unwrap();
        store.follow(&publication.uri).unwrap();
        store.upsert_document(&document, Some(&body())).unwrap();
        store.set_read(&document.uri, true).unwrap();
        store.put_blob("bafycid", &[9]).unwrap();
        store.set_sync_cursor(&publication.uri, "newest").unwrap();
        store.set_older_cursor("did:plc:test", "older").unwrap();
        store.set_show_images(false).unwrap();

        let (mut index, bodies, blobs) = emitted_state(&rx);
        index.schema = "old".into();
        let initial = InitialState::from_index(index, &bodies, blobs);

        assert_eq!(
            initial.publications.get(&publication.uri),
            Some(&publication)
        );
        assert!(initial.follows.contains_key(&publication.uri));
        assert_eq!(initial.blobs.get("bafycid"), Some(&vec![9]));
        assert!(!initial.show_images);
        assert!(initial.documents.get(&document.uri).unwrap().read);
        assert!(initial.documents.get(&document.uri).unwrap().body.is_none());
        assert!(initial.sync_cursors.is_empty());
        assert!(initial.older_cursors.is_empty());
    }

    #[test]
    fn corrupt_or_missing_body_degrades_to_metadata_only() {
        let (mut store, rx) = fresh_store();
        let publication = publication();
        let document = document(&publication.uri);
        store.upsert_document(&document, Some(&body())).unwrap();
        let (index, mut bodies, blobs) = emitted_state(&rx);
        bodies.insert(body_key(&document.uri), b"not json".to_vec());

        let initial = InitialState::from_index(index, &bodies, blobs);
        let stored = initial.documents.get(&document.uri).unwrap();
        assert_eq!(stored.meta, document);
        assert!(stored.body.is_none());
    }

    #[test]
    fn payload_ops_are_emitted_before_referencing_index() {
        let (mut store, rx) = fresh_store();
        let document = document("at://did:plc:test/site.standard.publication/main");
        store.upsert_document(&document, Some(&body())).unwrap();
        assert!(matches!(rx.recv().unwrap(), PersistOp::Body { .. }));
        assert!(matches!(rx.recv().unwrap(), PersistOp::Index(_)));

        store.put_blob("bafycid", &[1]).unwrap();
        assert!(matches!(rx.recv().unwrap(), PersistOp::Blob { .. }));
        assert!(matches!(rx.recv().unwrap(), PersistOp::Index(_)));
    }

    #[test]
    fn url_backed_blob_uses_an_opfs_safe_persistence_key() {
        let (mut store, rx) = fresh_store();
        let url = "https://cdn.example.test/images/header.png?size=full";
        store.put_blob(url, &[1, 2, 3]).unwrap();

        let PersistOp::Blob { cid, bytes } = rx.recv().unwrap() else {
            panic!("blob payload must be emitted before its index");
        };
        assert_eq!(cid, blob_file_key(url));
        assert!(!cid.contains('/'));
        assert_eq!(bytes, vec![1, 2, 3]);
        assert!(matches!(rx.recv().unwrap(), PersistOp::Index(_)));
    }
}
