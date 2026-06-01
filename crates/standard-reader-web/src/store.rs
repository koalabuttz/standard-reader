//! The web [`Store`] + [`FrontendStore`]: an in-memory cache (HashMaps).
//!
//! M1b keeps everything in RAM — a page reload starts fresh. Persistence (OPFS/IndexedDB) is M2.
//! In-memory operations never fail, so the associated error is [`Infallible`].

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;

use standard_core::model::{Document, Publication, RichDoc};
use standard_core::search::tokenize;
use standard_core::store::{Store, StoredDoc};
use standard_frontend::frontend_store::FrontendStore;

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
}

impl MemStore {
    pub fn new() -> Self {
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

impl Store for MemStore {
    type Error = Infallible;

    fn upsert_publication(&mut self, publication: &Publication) -> Result<(), Infallible> {
        self.publications
            .insert(publication.uri.clone(), publication.clone());
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
        let read = self.documents.get(&meta.uri).map(|d| d.read).unwrap_or(false);
        self.documents.insert(
            meta.uri.clone(),
            StoredDoc {
                meta: meta.clone(),
                body: body.cloned(),
                read,
            },
        );
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
        Ok(())
    }

    fn older_cursor(&self, did: &str) -> Result<Option<String>, Infallible> {
        Ok(self.older_cursors.get(did).cloned())
    }

    fn set_older_cursor(&mut self, did: &str, cursor: &str) -> Result<(), Infallible> {
        self.older_cursors
            .insert(did.to_string(), cursor.to_string());
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
        self.follows
            .entry(publication_uri.to_string())
            .or_default();
        Ok(())
    }

    fn unfollow(&mut self, publication_uri: &str) -> Result<(), Infallible> {
        self.follows.remove(publication_uri);
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
        Ok(())
    }

    fn show_images(&self) -> Result<bool, Infallible> {
        Ok(self.show_images)
    }

    fn set_show_images(&mut self, on: bool) -> Result<(), Infallible> {
        self.show_images = on;
        Ok(())
    }
}
