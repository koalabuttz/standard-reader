//! The background worker: the only thread that does I/O. It owns the `Transport`, the
//! `RedbStore`, and the decode `Registry`, and turns [`ToWorker`] commands into
//! [`FromWorker`] results on a channel — keeping the render loop non-blocking (the core is
//! synchronous; this is how the desktop frontend gets async behavior). Cache-first: it
//! answers from `redb` instantly, then refreshes from the network and sends an update.

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use standard_core::atp::{AtUri, Transport};
use standard_core::decode::Registry;
use standard_core::model::{Document, ImageSource, Publication, RichDoc};
use standard_core::read;
use standard_core::store::Store;

use crate::auth::{Account, Auth};
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
    LoadImage {
        key: String,
        source: ImageSource,
    },
    SetShowImages(bool),
    /// Sign in via OAuth using a handle or DID.
    Login(String),
    /// Sign out: revoke + forget the session.
    Logout,
    /// Push the given local-only follows up as `site.standard.graph.subscription` records
    /// (the "Subscribe" answer to the sync-reconciliation prompt).
    SubscribeLocal(Vec<String>),
    Quit,
}

/// Results from the worker to the UI.
pub enum FromWorker {
    Feeds(Vec<Publication>),
    Docs {
        publication: String,
        docs: Vec<Document>,
    },
    Doc {
        uri: String,
        body: RichDoc,
    },
    Results(Vec<Document>),
    Image {
        key: String,
        image: image::DynamicImage,
    },
    ShowImages(bool),
    /// The current signed-in identity (or `None` when signed out).
    Account(Option<Account>),
    /// Follows present locally but not on atproto — the reconciliation prompt's contents,
    /// as `(publication_uri, display_name)` pairs.
    SyncDiff {
        local_only: Vec<(String, String)>,
    },
    Status(String),
    Error(String),
}

/// Spawn the worker; returns the command sender and the result receiver.
pub fn spawn(cache_path: PathBuf, config_dir: PathBuf) -> (Sender<ToWorker>, Receiver<FromWorker>) {
    let (cmd_tx, cmd_rx) = channel::<ToWorker>();
    let (evt_tx, evt_rx) = channel::<FromWorker>();
    thread::spawn(move || run(cache_path, config_dir, cmd_rx, evt_tx));
    (cmd_tx, evt_rx)
}

fn run(
    cache_path: PathBuf,
    config_dir: PathBuf,
    cmd_rx: Receiver<ToWorker>,
    evt_tx: Sender<FromWorker>,
) {
    let store = match RedbStore::open(&cache_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = evt_tx.send(FromWorker::Error(format!("opening cache: {e}")));
            return;
        }
    };
    let log_path = config_dir.join("sr.log");
    // Fresh log per run, so it stays small and shows only the current session (it's a debug
    // log, not a persistent record). Truncate-on-start beats unbounded append.
    truncate_log(&log_path);
    let auth = build_auth(&config_dir, &evt_tx);
    append_log(
        &log_path,
        &format!(
            "worker started; auth {}",
            if auth.is_some() {
                "enabled"
            } else {
                "disabled"
            }
        ),
    );
    let mut ctx = Ctx {
        transport: ReqwestTransport::new(),
        store,
        registry: Registry::with_defaults(),
        pds_cache: HashMap::new(),
        account: None,
        auth,
        log_path,
        tx: evt_tx,
    };
    // Report the persisted text-only preference up front.
    ctx.send(FromWorker::ShowImages(
        ctx.store.show_images().unwrap_or(true),
    ));
    // Restore a saved OAuth session (if any) and reconcile subscriptions before serving.
    ctx.restore_session();

    while let Ok(msg) = cmd_rx.recv() {
        if matches!(msg, ToWorker::Quit) {
            break;
        }
        // Catch panics (e.g. deep in the async auth stack) so they surface as an error instead
        // of silently killing the worker and freezing the UI. The runtime survives an unwound
        // `block_on`, so the next command still works.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx.handle(msg))) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                append_log(&ctx.log_path, &format!("error: {e}"));
                let _ = ctx.tx.send(FromWorker::Error(e.to_string()));
            }
            Err(panic) => {
                let what = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                append_log(&ctx.log_path, &format!("PANIC: {what}"));
                let _ = ctx
                    .tx
                    .send(FromWorker::Error(format!("internal error: {what}")));
            }
        }
    }
}

/// Build the auth context (a tokio runtime + the OAuth client). Auth is optional: if either
/// the runtime or the client can't be built, the reader still works for (unauthenticated)
/// reads — only sign-in and subscription sync are disabled.
fn build_auth(config_dir: &std::path::Path, tx: &Sender<FromWorker>) -> Option<AuthCtx> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = tx.send(FromWorker::Error(format!(
                "auth disabled (no runtime): {e}"
            )));
            return None;
        }
    };
    match Auth::new(config_dir) {
        Ok(auth) => Some(AuthCtx { runtime, auth }),
        Err(e) => {
            let _ = tx.send(FromWorker::Error(format!("auth disabled: {e}")));
            None
        }
    }
}

struct Ctx {
    transport: ReqwestTransport,
    store: RedbStore,
    registry: Registry,
    /// did → PDS endpoint, so image-blob fetches don't re-resolve per image.
    pds_cache: HashMap<String, String>,
    /// The signed-in identity, or `None` when signed out.
    account: Option<Account>,
    /// The OAuth runtime + client, or `None` if auth couldn't be initialized.
    auth: Option<AuthCtx>,
    /// A debug log (`<config>/sr.log`) — the TUI owns the terminal, so progress/errors that
    /// can't fit the status line go here. `tail -f` it to watch the sign-in flow.
    log_path: PathBuf,
    tx: Sender<FromWorker>,
}

/// Reset the debug log to empty (called once at worker start). Best-effort.
fn truncate_log(path: &std::path::Path) {
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path);
}

/// Append a timestamped line to the debug log (best-effort; never fails the caller).
fn append_log(path: &std::path::Path, msg: &str) {
    use std::io::Write;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

/// The async side, kept off `standard-core`: a worker-owned tokio runtime and the OAuth client.
struct AuthCtx {
    runtime: tokio::runtime::Runtime,
    auth: Auth,
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
            ToWorker::Unfollow(uri) => self.unfollow(&uri),
            ToWorker::LoadImage { key, source } => self.load_image(key, source),
            ToWorker::SetShowImages(on) => Ok(self.store.set_show_images(on)?),
            ToWorker::Login(ident) => self.login(&ident),
            ToWorker::Logout => self.logout(),
            ToWorker::SubscribeLocal(uris) => self.subscribe_local(uris),
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
    ///
    /// A **handle/DID** subscribes to the whole repo — every publication it publishes. A
    /// **publisher URL** names one specific publication (a repo can host several: Bailey's
    /// DID owns `retrobailey.leaflet.pub` *and* two others), so it follows just that one.
    fn add_feed(&mut self, input: &str) -> Done {
        let target = normalize(input);
        let (identity, publications) = match read::resolve_identity(&self.transport, &target) {
            Ok(identity) => {
                let pubs = read::list_publications(&self.transport, &identity)?;
                (identity, pubs)
            }
            // Not an atproto handle. If this is a publisher URL (e.g. a *.leaflet.pub
            // subdomain — no well-known DID, no _atproto DNS), discover the one publication
            // the page advertises via its `<link rel="site.standard.publication">` and follow
            // just that — not every publication in the owner's repo.
            Err(handle_err) => match self.resolve_via_page(input, &target) {
                Some((identity, publication)) => (identity, vec![publication]),
                None => return Err(handle_err.into()),
            },
        };
        if publications.is_empty() {
            self.send(FromWorker::Status(format!(
                "no publications found at {target}"
            )));
            return Ok(());
        }
        let mut added = 0;
        for publication in &publications {
            self.store.upsert_publication(publication)?;
            if !self.store.is_followed(&publication.uri)? {
                self.follow(&publication.uri)?;
                added += 1;
            }
        }
        // Cache the repo's full document history so the new feed is readable immediately/offline.
        let repo_docs = read::list_all_documents(&self.transport, &identity)?;
        for doc in &repo_docs {
            self.store.upsert_document(doc, None)?;
        }
        // Seed each followed publication's incremental high-water mark.
        for publication in &publications {
            self.record_watermark(&publication.uri, &repo_docs)?;
        }
        self.send(FromWorker::Status(if added == 0 {
            format!("already following {target}")
        } else {
            format!("followed {added} publication(s) from {target}")
        }));
        self.load_home()
    }

    /// Resolve a publisher URL to its `(Identity, Publication)` via the page's standard.site
    /// discovery `<link>`. The fallback when handle resolution fails: a vanity host like
    /// `retrobailey.leaflet.pub` is no atproto handle, but its page advertises the AT-URI of
    /// the one publication it serves. `None` if the input isn't a fetchable URL or the page
    /// advertises no publication.
    fn resolve_via_page(&self, input: &str, host: &str) -> Option<(read::Identity, Publication)> {
        if host.starts_with("did:") {
            return None; // a DID that failed to resolve isn't a web page to scrape
        }
        let url = if input.trim_start().starts_with("http") {
            input.trim().to_string()
        } else {
            format!("https://{host}/")
        };
        let at_uri = read::discover_publication_uri(&self.transport, &url)
            .ok()
            .flatten()?;
        let uri = AtUri::parse(&at_uri)?;
        // Fetch just that publication record (resolves its DID → PDS along the way).
        let (publication, identity) = read::get_publication(&self.transport, &uri).ok()?;
        Some((identity, publication))
    }

    /// A feed's documents: cached first (instant), then a network refresh.
    fn open_feed(&mut self, pub_uri: &str) -> Done {
        let cached = self.store.documents_for(pub_uri)?;
        if !cached.is_empty() {
            self.send(FromWorker::Docs {
                publication: pub_uri.to_string(),
                docs: cached,
            });
        }
        self.refresh_docs(pub_uri)
    }

    /// Re-fetch a publication's documents and cache them. First time (nothing cached yet) → a full
    /// **backfill** so older posts aren't unreachable; afterwards → a cheap **incremental** walk
    /// that stops once it reaches documents already in the cache, so a refresh only pulls what's
    /// new. Either way the feed is then served from the cache (newest-first).
    fn refresh_docs(&mut self, pub_uri: &str) -> Done {
        let uri = AtUri::parse(pub_uri).ok_or("malformed publication AT-URI")?;
        let (_, repo) = read::get_publication(&self.transport, &uri)?;

        if self.store.documents_for(pub_uri)?.is_empty() {
            let all = read::list_all_documents(&self.transport, &repo)?;
            for doc in &all {
                self.store.upsert_document(doc, None)?;
            }
            self.record_watermark(pub_uri, &all)?;
        } else {
            self.fetch_new_documents(&repo, pub_uri)?;
        }

        // A repo can host several publications; serve the cache filtered to this feed.
        let docs = self.store.documents_for(pub_uri)?;
        self.send(FromWorker::Docs {
            publication: pub_uri.to_string(),
            docs,
        });
        Ok(())
    }

    /// Walk the repo's documents newest-first via `listRecords`, caching new ones and stopping as
    /// soon as a page introduces nothing new (everything already cached) or reaches this
    /// publication's stored high-water mark — the incremental refresh. Updates that mark to the
    /// newest document seen for the publication.
    fn fetch_new_documents(&mut self, repo: &read::Identity, pub_uri: &str) -> Done {
        let watermark = self.store.sync_cursor(pub_uri)?;
        let mut cursor: Option<String> = None;
        let mut newest_for_pub: Option<String> = None;
        loop {
            let (docs, next) = read::list_documents(&self.transport, repo, cursor.as_deref())?;
            if docs.is_empty() {
                break;
            }
            let mut page_had_new = false;
            let mut reached_mark = false;
            for doc in &docs {
                if newest_for_pub.is_none() && doc.publication == pub_uri {
                    newest_for_pub = Some(doc.uri.clone());
                }
                if watermark.as_deref() == Some(doc.uri.as_str()) {
                    reached_mark = true;
                    break;
                }
                // Cache-probe stop (robust for multi-publication repos): a doc already stored means
                // we've caught up on this page.
                if self.store.document(&doc.uri)?.is_none() {
                    self.store.upsert_document(doc, None)?;
                    page_had_new = true;
                }
            }
            if reached_mark || !page_had_new || next.is_none() {
                break;
            }
            cursor = next;
        }
        if let Some(newest) = newest_for_pub {
            self.store.set_sync_cursor(pub_uri, &newest)?;
        }
        Ok(())
    }

    /// Record the newest document URI for `pub_uri` (from a newest-first list) as its incremental
    /// sync high-water mark, so a later refresh can stop once it reaches it.
    fn record_watermark(&mut self, pub_uri: &str, docs: &[Document]) -> Done {
        if let Some(doc) = docs.iter().find(|d| d.publication == pub_uri) {
            self.store.set_sync_cursor(pub_uri, &doc.uri)?;
        }
        Ok(())
    }

    /// A document's decoded body: cached if present, else fetched + decoded + cached. Opening
    /// marks it read.
    fn open_doc(&mut self, doc_uri: &str) -> Done {
        if let Some(stored) = self.store.document(doc_uri)?
            && let Some(body) = stored.body
        {
            self.store.set_read(doc_uri, true)?;
            self.send(FromWorker::Doc {
                uri: doc_uri.to_string(),
                body,
            });
            return Ok(());
        }
        let uri = AtUri::parse(doc_uri).ok_or("malformed document AT-URI")?;
        let pds = read::resolve_pds(&self.transport, &uri.did)?;
        let (meta, body) = read::get_document(&self.transport, &self.registry, &uri, &pds)?;
        self.store.upsert_document(&meta, Some(&body))?;
        self.store.set_read(doc_uri, true)?;
        self.send(FromWorker::Doc {
            uri: doc_uri.to_string(),
            body,
        });
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

    /// Fetch an image's bytes (cache-first), decode, and hand the `DynamicImage` to the UI
    /// thread to encode for the terminal. If the bytes don't decode (e.g. AVIF, which the lean
    /// `image` build doesn't support), retry through the bsky CDN's transcode-to-JPEG — so
    /// JPEG/PNG/WebP stay direct-from-PDS and only undecodable formats touch the CDN.
    fn load_image(&mut self, key: String, source: ImageSource) -> Done {
        let bytes = self.image_bytes(&source)?;
        let err = match image::load_from_memory(&bytes) {
            Ok(image) => {
                self.send(FromWorker::Image { key, image });
                return Ok(());
            }
            Err(e) => e,
        };
        // Undecodable. Fall back to the CDN transcode if we can name the blob (did+cid).
        if let Some(cdn) = cdn_image_url(&source) {
            append_log(
                &self.log_path,
                &format!("image decode failed ({err}); retrying via CDN: {cdn}"),
            );
            if let Ok(transcoded) = self.cached_url(&cdn)
                && let Ok(image) = image::load_from_memory(&transcoded)
            {
                self.send(FromWorker::Image { key, image });
                return Ok(());
            }
        }
        self.send(FromWorker::Status(format!("image decode failed: {err}")));
        Ok(())
    }

    /// An image's original bytes, cache-first (blob CID for `Blob`, URL for `Url`).
    fn image_bytes(&mut self, source: &ImageSource) -> Result<Vec<u8>, Box<dyn Error>> {
        match source {
            ImageSource::Blob { did, cid } => {
                if let Some(b) = self.store.blob(cid)? {
                    return Ok(b);
                }
                let pds = self.pds_for(did)?;
                let b = read::get_blob(&self.transport, &pds, did, cid)?;
                self.store.put_blob(cid, &b)?;
                Ok(b)
            }
            ImageSource::Url(url) => self.cached_url(url),
        }
    }

    /// Fetch a URL's bytes, cache-first (keyed by the URL). Used for plain image URLs and the
    /// CDN transcode fallback.
    fn cached_url(&mut self, url: &str) -> Result<Vec<u8>, Box<dyn Error>> {
        if let Some(b) = self.store.blob(url)? {
            return Ok(b);
        }
        let b = self.transport.get(url)?;
        self.store.put_blob(url, &b)?;
        Ok(b)
    }

    /// PDS endpoint for a DID, resolving (and caching) on first use.
    fn pds_for(&mut self, did: &str) -> Result<String, Box<dyn Error>> {
        if let Some(pds) = self.pds_cache.get(did) {
            return Ok(pds.clone());
        }
        let pds = read::resolve_pds(&self.transport, did)?;
        self.pds_cache.insert(did.to_string(), pds.clone());
        Ok(pds)
    }

    // --- Auth + subscription sync ------------------------------------------------

    /// Restore a persisted OAuth session on startup; on success, reconcile subscriptions.
    fn restore_session(&mut self) {
        let Some(auth) = &self.auth else {
            self.send(FromWorker::Account(None));
            return;
        };
        match auth.runtime.block_on(auth.auth.restore()) {
            Ok(Some(account)) => {
                self.account = Some(account.clone());
                self.send(FromWorker::Account(Some(account.clone())));
                if let Err(e) = self.sync_subscriptions(&account) {
                    self.send(FromWorker::Error(format!("syncing subscriptions: {e}")));
                }
            }
            Ok(None) => self.send(FromWorker::Account(None)),
            Err(e) => {
                self.send(FromWorker::Account(None));
                self.send(FromWorker::Error(format!("restoring session: {e}")));
            }
        }
    }

    /// The OAuth browser flow: open the browser, await the loopback callback, persist the
    /// session, then reconcile subscriptions. The worker is blocked for the flow's duration.
    fn login(&mut self, ident: &str) -> Done {
        let Some(auth) = &self.auth else {
            self.send(FromWorker::Error("log-in is unavailable".into()));
            return Ok(());
        };
        let ident = normalize(ident);
        append_log(&self.log_path, &format!("login: start, ident={ident}"));
        // Report each step to both the status line and the log (the worker is blocked in
        // `block_on` meanwhile; the UI thread drains the channel and re-renders). Clones so the
        // closure doesn't borrow `self` across the `block_on`.
        let progress_tx = self.tx.clone();
        let progress_log = self.log_path.clone();
        let progress = move |msg: String| {
            append_log(&progress_log, &format!("login: {msg}"));
            let _ = progress_tx.send(FromWorker::Status(msg));
        };
        match auth.runtime.block_on(auth.auth.login(&ident, progress)) {
            Ok(account) => {
                append_log(&self.log_path, &format!("login: ok, did={}", account.did));
                self.account = Some(account.clone());
                self.send(FromWorker::Account(Some(account.clone())));
                self.send(FromWorker::Status(format!(
                    "logged in as @{}",
                    account.handle
                )));
                self.sync_subscriptions(&account)?;
            }
            Err(e) => {
                append_log(&self.log_path, &format!("login: failed: {e}"));
                self.send(FromWorker::Error(format!("log-in failed: {e}")));
            }
        }
        Ok(())
    }

    /// Revoke the session upstream and forget it locally. Local follows stay (now unsynced).
    fn logout(&mut self) -> Done {
        if let Some(auth) = &self.auth {
            let _ = auth.runtime.block_on(auth.auth.logout());
        }
        self.account = None;
        self.send(FromWorker::Account(None));
        self.send(FromWorker::Status("logged out".into()));
        Ok(())
    }

    /// Reconcile the local follow-list with the account's atproto subscriptions:
    /// import remote-only subscriptions silently; record rkeys for those in both; collect
    /// local-only follows into a [`FromWorker::SyncDiff`] for the user to resolve.
    fn sync_subscriptions(&mut self, account: &Account) -> Done {
        // The account's own subscriptions, read unauthenticated from its repo.
        let identity = read::resolve_identity(&self.transport, &account.did)?;
        let remote: HashMap<String, String> = read::list_subscriptions(&self.transport, &identity)?
            .into_iter()
            .map(|s| (s.publication, rkey_from_uri(&s.uri)))
            .collect();

        let diff = diff_subscriptions(&self.store.follows()?, &remote);

        // Remote-only → fetch + cache, follow, record rkey.
        for (pub_uri, rkey) in &diff.remote_only {
            if let Err(e) = self.cache_publication(pub_uri) {
                // Best-effort: a since-deleted publication shouldn't abort the whole sync.
                self.send(FromWorker::Status(format!(
                    "couldn't import {pub_uri}: {e}"
                )));
            }
            self.store.follow(pub_uri)?;
            self.store.set_follow_rkey(pub_uri, rkey)?;
        }
        // Already followed → just record the upstream rkey.
        for (pub_uri, rkey) in &diff.in_both {
            self.store.set_follow_rkey(pub_uri, rkey)?;
        }

        // Local-only → ask the user (could be an intentional add or a stale unfollow).
        let mut local_only = Vec::new();
        for pub_uri in diff.local_only {
            let name = self
                .store
                .publication(&pub_uri)?
                .map(|p| p.name)
                .unwrap_or_else(|| pub_uri.clone());
            local_only.push((pub_uri, name));
        }

        self.load_home()?;
        if !local_only.is_empty() {
            self.send(FromWorker::SyncDiff { local_only });
        }
        Ok(())
    }

    /// Push the given local-only follows up as subscription records (the modal's "Subscribe").
    fn subscribe_local(&mut self, pub_uris: Vec<String>) -> Done {
        let Some(did) = self.account.as_ref().map(|a| a.did.clone()) else {
            self.send(FromWorker::Error("not logged in".into()));
            return Ok(());
        };
        let mut subscribed = 0;
        for pub_uri in &pub_uris {
            match self.create_subscription(&did, pub_uri) {
                Ok(rkey) => {
                    self.store.set_follow_rkey(pub_uri, &rkey)?;
                    subscribed += 1;
                }
                Err(e) => self.send(FromWorker::Status(format!(
                    "couldn't subscribe to {pub_uri}: {e}"
                ))),
            }
        }
        self.send(FromWorker::Status(format!(
            "subscribed to {subscribed} feed(s)"
        )));
        self.load_home()
    }

    /// Add a local follow, mirroring it upstream as a subscription record when signed in.
    fn follow(&mut self, pub_uri: &str) -> Done {
        let newly = !self.store.is_followed(pub_uri)?;
        self.store.follow(pub_uri)?;
        if newly && let Some(did) = self.account.as_ref().map(|a| a.did.clone()) {
            match self.create_subscription(&did, pub_uri) {
                Ok(rkey) => self.store.set_follow_rkey(pub_uri, &rkey)?,
                Err(e) => self.send(FromWorker::Status(format!(
                    "followed locally; upstream sync failed: {e}"
                ))),
            }
        }
        Ok(())
    }

    /// Remove a local follow, deleting its upstream subscription record when one is known.
    fn unfollow(&mut self, pub_uri: &str) -> Done {
        if let Some(did) = self.account.as_ref().map(|a| a.did.clone())
            && let Some(rkey) = self.store.follow_rkey(pub_uri)?
            && let Some(auth) = &self.auth
            && let Err(e) = auth
                .runtime
                .block_on(auth.auth.delete_subscription(&did, &rkey))
        {
            self.send(FromWorker::Status(format!(
                "removed locally; upstream delete failed: {e}"
            )));
        }
        self.store.unfollow(pub_uri)?;
        self.load_home()
    }

    /// Create one `site.standard.graph.subscription` record (auth required); returns its rkey.
    fn create_subscription(&self, did: &str, pub_uri: &str) -> Result<String, Box<dyn Error>> {
        let auth = self.auth.as_ref().ok_or("auth unavailable")?;
        auth.runtime
            .block_on(auth.auth.create_subscription(did, pub_uri))
            .map_err(|e| -> Box<dyn Error> { e.to_string().into() })
    }

    /// Fetch + cache a publication and its documents (no UI emission); used for sync imports.
    fn cache_publication(&mut self, pub_uri: &str) -> Done {
        let uri = AtUri::parse(pub_uri).ok_or("malformed publication AT-URI")?;
        let (publication, repo) = read::get_publication(&self.transport, &uri)?;
        self.store.upsert_publication(&publication)?;
        let repo_docs = read::list_all_documents(&self.transport, &repo)?;
        for doc in &repo_docs {
            self.store.upsert_document(doc, None)?;
        }
        self.record_watermark(pub_uri, &repo_docs)?;
        Ok(())
    }
}

/// `at://<did>/<collection>/<rkey>` → `<rkey>` (the trailing path segment).
fn rkey_from_uri(uri: &str) -> String {
    uri.rsplit('/').next().unwrap_or_default().to_string()
}

/// The bsky CDN transcode URL for an image — used as a fallback when the original bytes don't
/// decode (e.g. AVIF). Needs the blob's `did`+`cid`: a `Blob` source has them directly; a `Url`
/// source must be a `com.atproto.sync.getBlob` URL (GreenGale emits these). Returns `None` for
/// arbitrary external image URLs (nothing to transcode).
fn cdn_image_url(source: &ImageSource) -> Option<String> {
    let (did, cid) = match source {
        ImageSource::Blob { did, cid } => (did.clone(), cid.clone()),
        ImageSource::Url(url) => getblob_did_cid(url)?,
    };
    Some(format!(
        "https://cdn.bsky.app/img/feed_fullsize/plain/{did}/{cid}@jpeg"
    ))
}

/// Extract `(did, cid)` from a `com.atproto.sync.getBlob` URL's query string.
fn getblob_did_cid(url: &str) -> Option<(String, String)> {
    if !url.contains("getBlob") {
        return None;
    }
    let query = url.split_once('?')?.1;
    let (mut did, mut cid) = (None, None);
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            match key {
                "did" => did = Some(percent_decode(value)),
                "cid" => cid = Some(percent_decode(value)),
                _ => {}
            }
        }
    }
    Some((did?, cid?))
}

/// Minimal `%XX` / `+` URL-decode (enough for getBlob query values, e.g. `%3A` → `:`).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&input[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// How the local follow-list and the account's atproto subscriptions differ.
#[derive(Debug, Default, PartialEq, Eq)]
struct SubscriptionDiff {
    /// On atproto but not followed locally — import: fetch + cache + follow + record rkey.
    remote_only: Vec<(String, String)>,
    /// Followed both places — just record the upstream rkey.
    in_both: Vec<(String, String)>,
    /// Followed locally but not on atproto — ask the user (add vs. stale unfollow is ambiguous).
    local_only: Vec<String>,
}

/// Partition a sync. `local` = followed publication URIs; `remote` = publication uri → rkey.
/// Output vectors are sorted so the result is deterministic (the `remote` map is unordered).
fn diff_subscriptions(local: &[String], remote: &HashMap<String, String>) -> SubscriptionDiff {
    let local_set: std::collections::HashSet<&str> = local.iter().map(String::as_str).collect();
    let mut diff = SubscriptionDiff::default();
    for (pub_uri, rkey) in remote {
        let entry = (pub_uri.clone(), rkey.clone());
        if local_set.contains(pub_uri.as_str()) {
            diff.in_both.push(entry);
        } else {
            diff.remote_only.push(entry);
        }
    }
    for pub_uri in local {
        if !remote.contains_key(pub_uri) {
            diff.local_only.push(pub_uri.clone());
        }
    }
    diff.remote_only.sort();
    diff.in_both.sort();
    diff.local_only.sort();
    diff
}

/// Normalize user input to a resolvable handle/DID: pass `did:…` through; strip a URL
/// scheme/path down to its host; drop a leading `@`.
fn normalize(input: &str) -> String {
    let s = input.trim();
    if s.starts_with("did:") {
        return s.to_string();
    }
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let host = s.split('/').next().unwrap_or(s);
    host.trim_start_matches('@').to_string()
}

#[cfg(test)]
mod tests {
    use super::{SubscriptionDiff, diff_subscriptions, normalize, rkey_from_uri};
    use std::collections::HashMap;

    #[test]
    fn cdn_url_from_blob_and_getblob_url() {
        use standard_core::model::ImageSource;
        // A blob source → CDN transcode URL directly.
        assert_eq!(
            super::cdn_image_url(&ImageSource::Blob {
                did: "did:plc:abc".into(),
                cid: "bafcid".into()
            })
            .as_deref(),
            Some("https://cdn.bsky.app/img/feed_fullsize/plain/did:plc:abc/bafcid@jpeg")
        );
        // A getBlob URL (percent-encoded did) → did/cid extracted, CDN URL built.
        let url =
            "https://yapfest.club/xrpc/com.atproto.sync.getBlob?did=did%3Aplc%3Axyz&cid=bafblob";
        assert_eq!(
            super::cdn_image_url(&ImageSource::Url(url.into())).as_deref(),
            Some("https://cdn.bsky.app/img/feed_fullsize/plain/did:plc:xyz/bafblob@jpeg")
        );
        // An arbitrary external image URL → no transcode target.
        assert_eq!(
            super::cdn_image_url(&ImageSource::Url("https://example.com/pic.png".into())),
            None
        );
    }

    #[test]
    fn rkey_is_the_trailing_segment() {
        assert_eq!(
            rkey_from_uri("at://did:plc:x/site.standard.graph.subscription/3kabc"),
            "3kabc"
        );
        assert_eq!(rkey_from_uri("bare"), "bare");
    }

    #[test]
    fn diff_partitions_remote_only_in_both_and_local_only() {
        let local = vec!["at://p/both".to_string(), "at://p/localonly".to_string()];
        let remote = HashMap::from([
            ("at://p/both".to_string(), "rk_both".to_string()),
            ("at://p/remoteonly".to_string(), "rk_remote".to_string()),
        ]);

        let diff = diff_subscriptions(&local, &remote);
        assert_eq!(
            diff,
            SubscriptionDiff {
                remote_only: vec![("at://p/remoteonly".into(), "rk_remote".into())],
                in_both: vec![("at://p/both".into(), "rk_both".into())],
                local_only: vec!["at://p/localonly".into()],
            }
        );
    }

    #[test]
    fn diff_with_no_remote_makes_everything_local_only() {
        let local = vec!["at://p/a".to_string(), "at://p/b".to_string()];
        let diff = diff_subscriptions(&local, &HashMap::new());
        assert!(diff.remote_only.is_empty() && diff.in_both.is_empty());
        assert_eq!(diff.local_only, ["at://p/a", "at://p/b"]);
    }

    #[test]
    fn normalizes_inputs() {
        assert_eq!(normalize("  david.yapfest.club "), "david.yapfest.club");
        assert_eq!(
            normalize("https://half-baked.pckt.blog/a/post"),
            "half-baked.pckt.blog"
        );
        assert_eq!(normalize("@alice.test"), "alice.test");
        assert_eq!(normalize("did:plc:abc"), "did:plc:abc");
    }
}
