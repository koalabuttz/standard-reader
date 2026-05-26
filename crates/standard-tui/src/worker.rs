//! The background worker: the only thread that does I/O. It owns the `Transport`, the
//! `RedbStore`, and the decode `Registry`, and turns [`ToWorker`] commands into
//! [`FromWorker`] results on a channel — keeping the render loop non-blocking (the core is
//! synchronous; this is how the desktop frontend gets async behavior). Cache-first: it
//! answers from `redb` instantly, then refreshes from the network and sends an update.

use std::error::Error;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use standard_core::atp::AtUri;
use standard_core::decode::Registry;
use standard_core::model::{Document, Publication, RichDoc};
use standard_core::read;
use standard_core::store::Store;

use crate::store::RedbStore;
use crate::transport::ReqwestTransport;

/// Commands from the UI to the worker.
pub enum ToWorker {
    LoadHome,
    AddFeed(String),
    OpenFeed(String),
    OpenDoc(String),
    Search(String),
    Refresh(String),
    SetRead(String, bool),
    Unfollow(String),
    Quit,
}

/// Results from the worker to the UI.
pub enum FromWorker {
    Feeds(Vec<Publication>),
    Docs { publication: String, docs: Vec<Document> },
    Doc { uri: String, body: RichDoc },
    Results(Vec<Document>),
    Status(String),
    Error(String),
}

/// Spawn the worker; returns the command sender and the result receiver.
pub fn spawn(cache_path: PathBuf) -> (Sender<ToWorker>, Receiver<FromWorker>) {
    let (cmd_tx, cmd_rx) = channel::<ToWorker>();
    let (evt_tx, evt_rx) = channel::<FromWorker>();
    thread::spawn(move || run(cache_path, cmd_rx, evt_tx));
    (cmd_tx, evt_rx)
}

fn run(cache_path: PathBuf, cmd_rx: Receiver<ToWorker>, evt_tx: Sender<FromWorker>) {
    let store = match RedbStore::open(&cache_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = evt_tx.send(FromWorker::Error(format!("opening cache: {e}")));
            return;
        }
    };
    let mut ctx = Ctx {
        transport: ReqwestTransport::new(),
        store,
        registry: Registry::with_defaults(),
        tx: evt_tx,
    };
    while let Ok(msg) = cmd_rx.recv() {
        if matches!(msg, ToWorker::Quit) {
            break;
        }
        if let Err(e) = ctx.handle(msg) {
            let _ = ctx.tx.send(FromWorker::Error(e.to_string()));
        }
    }
}

struct Ctx {
    transport: ReqwestTransport,
    store: RedbStore,
    registry: Registry,
    tx: Sender<FromWorker>,
}

type Done = Result<(), Box<dyn Error>>;

impl Ctx {
    fn send(&self, evt: FromWorker) {
        let _ = self.tx.send(evt);
    }

    fn handle(&mut self, msg: ToWorker) -> Done {
        match msg {
            ToWorker::LoadHome => self.load_home(),
            ToWorker::AddFeed(input) => self.add_feed(&input),
            ToWorker::OpenFeed(uri) => self.open_feed(&uri),
            ToWorker::OpenDoc(uri) => self.open_doc(&uri),
            ToWorker::Search(query) => self.search(&query),
            ToWorker::Refresh(uri) => self.refresh_docs(&uri),
            ToWorker::SetRead(uri, read) => Ok(self.store.set_read(&uri, read)?),
            ToWorker::Unfollow(uri) => {
                self.store.unfollow(&uri)?;
                self.load_home()
            }
            ToWorker::Quit => Ok(()),
        }
    }

    /// Followed publications, straight from the cache.
    fn load_home(&self) -> Done {
        let mut pubs = Vec::new();
        for uri in self.store.follows()? {
            if let Some(p) = self.store.publication(&uri)? {
                pubs.push(p);
            }
        }
        self.send(FromWorker::Feeds(pubs));
        Ok(())
    }

    /// Resolve a handle/DID/URL to its publications, follow + cache them, fetch their docs.
    fn add_feed(&mut self, input: &str) -> Done {
        let target = normalize(input);
        let identity = read::resolve_identity(&self.transport, &target)?;
        let publications = read::list_publications(&self.transport, &identity)?;
        if publications.is_empty() {
            self.send(FromWorker::Status(format!("no publications found at {target}")));
            return Ok(());
        }
        let mut added = 0;
        for publication in &publications {
            self.store.upsert_publication(publication)?;
            if !self.store.is_followed(&publication.uri)? {
                self.store.follow(&publication.uri)?;
                added += 1;
            }
        }
        // Cache the repo's documents so the new feed is readable immediately/offline.
        let (repo_docs, _) = read::list_documents(&self.transport, &identity, None)?;
        for doc in &repo_docs {
            self.store.upsert_document(doc, None)?;
        }
        self.send(FromWorker::Status(if added == 0 {
            format!("already following {target}")
        } else {
            format!("followed {added} publication(s) from {target}")
        }));
        self.load_home()
    }

    /// A feed's documents: cached first (instant), then a network refresh.
    fn open_feed(&mut self, pub_uri: &str) -> Done {
        let cached = self.store.documents_for(pub_uri)?;
        if !cached.is_empty() {
            self.send(FromWorker::Docs { publication: pub_uri.to_string(), docs: cached });
        }
        self.refresh_docs(pub_uri)
    }

    /// Re-fetch a publication's documents from the network and cache them.
    fn refresh_docs(&mut self, pub_uri: &str) -> Done {
        let uri = AtUri::parse(pub_uri).ok_or("malformed publication AT-URI")?;
        let (_, repo) = read::get_publication(&self.transport, &uri)?;
        let (repo_docs, _) = read::list_documents(&self.transport, &repo, None)?;
        for doc in &repo_docs {
            self.store.upsert_document(doc, None)?;
        }
        // A repo can host several publications; this feed is the docs whose `site` matches.
        let docs = repo_docs.into_iter().filter(|d| d.publication == pub_uri).collect();
        self.send(FromWorker::Docs { publication: pub_uri.to_string(), docs });
        Ok(())
    }

    /// A document's decoded body: cached if present, else fetched + decoded + cached. Opening
    /// marks it read.
    fn open_doc(&mut self, doc_uri: &str) -> Done {
        if let Some(stored) = self.store.document(doc_uri)?
            && let Some(body) = stored.body {
                self.store.set_read(doc_uri, true)?;
                self.send(FromWorker::Doc { uri: doc_uri.to_string(), body });
                return Ok(());
            }
        let uri = AtUri::parse(doc_uri).ok_or("malformed document AT-URI")?;
        let pds = read::resolve_pds(&self.transport, &uri.did)?;
        let (meta, body) = read::get_document(&self.transport, &self.registry, &uri, &pds)?;
        self.store.upsert_document(&meta, Some(&body))?;
        self.store.set_read(doc_uri, true)?;
        self.send(FromWorker::Doc { uri: doc_uri.to_string(), body });
        Ok(())
    }

    fn search(&self, query: &str) -> Done {
        let mut results = Vec::new();
        for uri in self.store.search(query)? {
            if let Some(stored) = self.store.document(&uri)? {
                results.push(stored.meta);
            }
        }
        self.send(FromWorker::Results(results));
        Ok(())
    }
}

/// Normalize user input to a resolvable handle/DID: pass `did:…` through; strip a URL
/// scheme/path down to its host; drop a leading `@`.
fn normalize(input: &str) -> String {
    let s = input.trim();
    if s.starts_with("did:") {
        return s.to_string();
    }
    let s = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    let host = s.split('/').next().unwrap_or(s);
    host.trim_start_matches('@').to_string()
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalizes_inputs() {
        assert_eq!(normalize("  david.yapfest.club "), "david.yapfest.club");
        assert_eq!(normalize("https://half-baked.pckt.blog/a/post"), "half-baked.pckt.blog");
        assert_eq!(normalize("@alice.test"), "alice.test");
        assert_eq!(normalize("did:plc:abc"), "did:plc:abc");
    }
}
