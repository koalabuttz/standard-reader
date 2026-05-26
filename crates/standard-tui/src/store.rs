//! The desktop [`Store`]: an offline cache backed by `redb` (pure-Rust embedded KV).
//!
//! This is the second of the two platform seams (the other is the `Transport`). The core
//! reads and writes through the `Store` trait; here we satisfy it with `redb`. Values are
//! serde_json bytes of the `model` types (which already derive serde); keys are AT-URIs /
//! CIDs. Because it's a *cache*, a schema change is a re-fetch, not a migration.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::path::Path;

use redb::{Database, MultimapTableDefinition, ReadableDatabase, ReadableTable, TableDefinition};
use standard_core::model::{Document, Publication, RichDoc};
use standard_core::search::tokenize;
use standard_core::store::{Store, StoredDoc};

// uri → json(Publication)
const PUBLICATIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("publications");
// uri → json(StoredDoc)  (metadata + decoded body + read-state in one value)
const DOCUMENTS: TableDefinition<&str, &[u8]> = TableDefinition::new("documents");
// cid → raw blob bytes
const BLOBS: TableDefinition<&str, &[u8]> = TableDefinition::new("blobs");
// publication uri → listRecords sync cursor
const CURSORS: TableDefinition<&str, &str> = TableDefinition::new("cursors");
// publication uri → doc uri  (backs documents_for)
const DOCS_BY_PUB: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("docs_by_pub");
// search term → doc uri  (persisted inverted index over textContent)
const INDEX: MultimapTableDefinition<&str, &str> = MultimapTableDefinition::new("index");
// publication uri → ()  (the local follow-list; the app's own subscriptions, no auth)
const FOLLOWS: TableDefinition<&str, ()> = TableDefinition::new("follows");

/// Errors from the cache: a redb failure or a (de)serialization failure.
#[derive(Debug)]
pub enum CacheError {
    Redb(redb::Error),
    Json(serde_json::Error),
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheError::Redb(e) => write!(f, "cache (redb) error: {e}"),
            CacheError::Json(e) => write!(f, "cache (serde) error: {e}"),
        }
    }
}

impl Error for CacheError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            CacheError::Redb(e) => Some(e),
            CacheError::Json(e) => Some(e),
        }
    }
}

macro_rules! from_redb {
    ($($t:ty),* $(,)?) => {
        $(impl From<$t> for CacheError {
            fn from(e: $t) -> Self { CacheError::Redb(e.into()) }
        })*
    };
}
from_redb!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError,
);
impl From<serde_json::Error> for CacheError {
    fn from(e: serde_json::Error) -> Self {
        CacheError::Json(e)
    }
}

type Result<T> = std::result::Result<T, CacheError>;

fn to_bytes<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(value)?)
}

fn from_bytes<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(bytes)?)
}

pub struct RedbStore {
    db: Database,
}

impl RedbStore {
    /// Open (creating if needed) a file-backed cache.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let store = Self {
            db: Database::create(path)?,
        };
        store.init_tables()?;
        Ok(store)
    }

    /// An in-memory cache (tests).
    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let db = Database::builder().create_with_backend(redb::backends::InMemoryBackend::new())?;
        let store = Self { db };
        store.init_tables()?;
        Ok(store)
    }

    /// Create every table once so later read transactions never hit `TableDoesNotExist`.
    fn init_tables(&self) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            w.open_table(PUBLICATIONS)?;
            w.open_table(DOCUMENTS)?;
            w.open_table(BLOBS)?;
            w.open_table(CURSORS)?;
            w.open_multimap_table(DOCS_BY_PUB)?;
            w.open_multimap_table(INDEX)?;
            w.open_table(FOLLOWS)?;
        }
        w.commit()?;
        Ok(())
    }

    /// The local follow-list — publication URIs the user follows (the app's own
    /// subscriptions, persisted without atproto auth). OAuth later mirrors this to
    /// `site.standard.graph.subscription`.
    pub fn follow(&mut self, publication_uri: &str) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut table = w.open_table(FOLLOWS)?;
            table.insert(publication_uri, ())?;
        }
        w.commit()?;
        Ok(())
    }

    pub fn unfollow(&mut self, publication_uri: &str) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut table = w.open_table(FOLLOWS)?;
            table.remove(publication_uri)?;
        }
        w.commit()?;
        Ok(())
    }

    pub fn is_followed(&self, publication_uri: &str) -> Result<bool> {
        let r = self.db.begin_read()?;
        let table = r.open_table(FOLLOWS)?;
        Ok(table.get(publication_uri)?.is_some())
    }

    /// Followed publication URIs.
    pub fn follows(&self) -> Result<Vec<String>> {
        let r = self.db.begin_read()?;
        let table = r.open_table(FOLLOWS)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            out.push(entry?.0.value().to_string());
        }
        Ok(out)
    }

    /// Every cached publication. Not part of the `Store` trait (which is keyed lookup);
    /// a frontend listing convenience for rendering the cache offline.
    pub fn all_publications(&self) -> Result<Vec<Publication>> {
        let r = self.db.begin_read()?;
        let table = r.open_table(PUBLICATIONS)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            out.push(from_bytes(entry?.1.value())?);
        }
        Ok(out)
    }

    /// Read a `StoredDoc` by uri (shared by `document`, `documents_for`, `set_read`).
    fn read_doc(&self, uri: &str) -> Result<Option<StoredDoc>> {
        let r = self.db.begin_read()?;
        let table = r.open_table(DOCUMENTS)?;
        match table.get(uri)? {
            Some(guard) => Ok(Some(from_bytes(guard.value())?)),
            None => Ok(None),
        }
    }
}

impl Store for RedbStore {
    type Error = CacheError;

    fn upsert_publication(&mut self, publication: &Publication) -> Result<()> {
        let bytes = to_bytes(publication)?;
        let w = self.db.begin_write()?;
        {
            let mut table = w.open_table(PUBLICATIONS)?;
            table.insert(publication.uri.as_str(), bytes.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    fn publication(&self, uri: &str) -> Result<Option<Publication>> {
        let r = self.db.begin_read()?;
        let table = r.open_table(PUBLICATIONS)?;
        match table.get(uri)? {
            Some(guard) => Ok(Some(from_bytes(guard.value())?)),
            None => Ok(None),
        }
    }

    fn upsert_document(&mut self, meta: &Document, body: Option<&RichDoc>) -> Result<()> {
        // Preserve a prior read-state across re-fetch.
        let read = self.read_doc(&meta.uri)?.map(|d| d.read).unwrap_or(false);
        let stored = StoredDoc {
            meta: meta.clone(),
            body: body.cloned(),
            read,
        };
        let bytes = to_bytes(&stored)?;

        let w = self.db.begin_write()?;
        {
            let mut docs = w.open_table(DOCUMENTS)?;
            docs.insert(meta.uri.as_str(), bytes.as_slice())?;

            let mut by_pub = w.open_multimap_table(DOCS_BY_PUB)?;
            by_pub.insert(meta.publication.as_str(), meta.uri.as_str())?;

            // Index textContent (multimap dedupes, so re-upsert is idempotent).
            if let Some(text) = &meta.text_content {
                let mut index = w.open_multimap_table(INDEX)?;
                for term in tokenize(text) {
                    index.insert(term.as_str(), meta.uri.as_str())?;
                }
            }
        }
        w.commit()?;
        Ok(())
    }

    fn document(&self, uri: &str) -> Result<Option<StoredDoc>> {
        self.read_doc(uri)
    }

    fn documents_for(&self, publication_uri: &str) -> Result<Vec<Document>> {
        let r = self.db.begin_read()?;
        let by_pub = r.open_multimap_table(DOCS_BY_PUB)?;
        let docs = r.open_table(DOCUMENTS)?;

        let mut out = Vec::new();
        for entry in by_pub.get(publication_uri)? {
            let uri = entry?;
            if let Some(guard) = docs.get(uri.value())? {
                out.push(from_bytes::<StoredDoc>(guard.value())?.meta);
            }
        }
        // Newest first (RFC-3339 sorts lexically; mixed TZ offsets are an accepted v1 caveat).
        out.sort_by(|a, b| b.published_at.cmp(&a.published_at));
        Ok(out)
    }

    fn set_read(&mut self, uri: &str, read: bool) -> Result<()> {
        let Some(mut doc) = self.read_doc(uri)? else {
            return Ok(()); // nothing cached to mark
        };
        doc.read = read;
        let bytes = to_bytes(&doc)?;
        let w = self.db.begin_write()?;
        {
            let mut table = w.open_table(DOCUMENTS)?;
            table.insert(uri, bytes.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    fn search(&self, query: &str) -> Result<Vec<String>> {
        let r = self.db.begin_read()?;
        let index = r.open_multimap_table(INDEX)?;

        let postings = |term: &str| -> Result<BTreeSet<String>> {
            let mut set = BTreeSet::new();
            for entry in index.get(term)? {
                set.insert(entry?.value().to_string());
            }
            Ok(set)
        };

        let mut terms = tokenize(query);
        let Some(first) = terms.next() else {
            return Ok(Vec::new());
        };
        let mut hits = postings(&first)?;
        for term in terms {
            let next = postings(&term)?;
            hits.retain(|uri| next.contains(uri));
            if hits.is_empty() {
                break;
            }
        }
        Ok(hits.into_iter().collect())
    }

    fn put_blob(&mut self, cid: &str, bytes: &[u8]) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut table = w.open_table(BLOBS)?;
            table.insert(cid, bytes)?;
        }
        w.commit()?;
        Ok(())
    }

    fn blob(&self, cid: &str) -> Result<Option<Vec<u8>>> {
        let r = self.db.begin_read()?;
        let table = r.open_table(BLOBS)?;
        Ok(table.get(cid)?.map(|g| g.value().to_vec()))
    }

    fn sync_cursor(&self, publication_uri: &str) -> Result<Option<String>> {
        let r = self.db.begin_read()?;
        let table = r.open_table(CURSORS)?;
        Ok(table.get(publication_uri)?.map(|g| g.value().to_string()))
    }

    fn set_sync_cursor(&mut self, publication_uri: &str, cursor: &str) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut table = w.open_table(CURSORS)?;
            table.insert(publication_uri, cursor)?;
        }
        w.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use standard_core::model::{Block, Inline};

    fn doc(uri: &str, pub_uri: &str, published: &str, text: &str) -> Document {
        Document {
            uri: uri.into(),
            title: "t".into(),
            description: None,
            publication: pub_uri.into(),
            published_at: published.into(),
            updated_at: None,
            cover_image: None,
            text_content: Some(text.into()),
            tags: vec![],
        }
    }

    #[test]
    fn publication_round_trip() {
        let mut s = RedbStore::in_memory().unwrap();
        let p = Publication {
            uri: "at://d/site.standard.publication/1".into(),
            url: "https://x.test".into(),
            name: "X".into(),
            description: None,
            icon: None,
        };
        s.upsert_publication(&p).unwrap();
        assert_eq!(s.publication(&p.uri).unwrap().as_ref(), Some(&p));
        assert_eq!(s.publication("at://nope/x/1").unwrap(), None);
    }

    #[test]
    fn document_body_survives_serialization() {
        let mut s = RedbStore::in_memory().unwrap();
        let d = doc("at://d/c/1", "at://d/p/1", "2026-01-01", "hello");
        let body = RichDoc {
            blocks: vec![Block::Paragraph(vec![Inline::Strong(vec![Inline::Text(
                "hi".into(),
            )])])],
        };
        s.upsert_document(&d, Some(&body)).unwrap();
        let stored = s.document(&d.uri).unwrap().unwrap();
        assert_eq!(stored.body.as_ref(), Some(&body));
        assert!(!stored.read);
    }

    #[test]
    fn documents_for_is_newest_first() {
        let mut s = RedbStore::in_memory().unwrap();
        let p = "at://d/p/1";
        s.upsert_document(&doc("at://d/c/old", p, "2025-01-01", "a"), None)
            .unwrap();
        s.upsert_document(&doc("at://d/c/new", p, "2026-01-01", "b"), None)
            .unwrap();
        s.upsert_document(
            &doc("at://d/c/other", "at://d/p/2", "2027-01-01", "c"),
            None,
        )
        .unwrap();

        let uris: Vec<_> = s
            .documents_for(p)
            .unwrap()
            .into_iter()
            .map(|d| d.uri)
            .collect();
        assert_eq!(uris, ["at://d/c/new", "at://d/c/old"]);
    }

    #[test]
    fn set_read_persists_and_survives_reupsert() {
        let mut s = RedbStore::in_memory().unwrap();
        let d = doc("at://d/c/1", "at://d/p/1", "2026-01-01", "x");
        s.upsert_document(&d, None).unwrap();
        s.set_read(&d.uri, true).unwrap();
        assert!(s.document(&d.uri).unwrap().unwrap().read);

        // Re-fetching the doc must not clobber read-state.
        s.upsert_document(&d, None).unwrap();
        assert!(s.document(&d.uri).unwrap().unwrap().read);
    }

    #[test]
    fn blob_and_cursor_round_trip() {
        let mut s = RedbStore::in_memory().unwrap();
        s.put_blob("bafcid", &[1, 2, 3]).unwrap();
        assert_eq!(s.blob("bafcid").unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(s.blob("missing").unwrap(), None);

        assert_eq!(s.sync_cursor("at://d/p/1").unwrap(), None);
        s.set_sync_cursor("at://d/p/1", "cursor123").unwrap();
        assert_eq!(
            s.sync_cursor("at://d/p/1").unwrap().as_deref(),
            Some("cursor123")
        );
    }

    #[test]
    fn follow_list_round_trips() {
        let mut s = RedbStore::in_memory().unwrap();
        assert!(s.follows().unwrap().is_empty());
        assert!(!s.is_followed("at://d/p/1").unwrap());

        s.follow("at://d/p/1").unwrap();
        s.follow("at://d/p/2").unwrap();
        assert!(s.is_followed("at://d/p/1").unwrap());
        assert_eq!(s.follows().unwrap().len(), 2);

        s.unfollow("at://d/p/1").unwrap();
        assert!(!s.is_followed("at://d/p/1").unwrap());
        assert_eq!(s.follows().unwrap(), ["at://d/p/2"]);
    }

    #[test]
    fn search_has_and_semantics() {
        let mut s = RedbStore::in_memory().unwrap();
        s.upsert_document(
            &doc(
                "at://d/c/1",
                "at://d/p/1",
                "2026",
                "bottom surgery is a big change",
            ),
            None,
        )
        .unwrap();
        s.upsert_document(
            &doc(
                "at://d/c/2",
                "at://d/p/1",
                "2026",
                "a lifechanging surgery story",
            ),
            None,
        )
        .unwrap();

        assert_eq!(s.search("surgery").unwrap().len(), 2);
        assert_eq!(s.search("bottom surgery").unwrap(), ["at://d/c/1"]);
        assert!(s.search("nonexistent").unwrap().is_empty());
    }
}
