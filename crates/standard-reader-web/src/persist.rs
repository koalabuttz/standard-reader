//! OPFS persistence glue — the async I/O half of the web store cache.
//!
//! The worker thread blocks on `recv()` and can never await, so all async OPFS work runs on the
//! **main thread** (where `draw_web`'s rAF loop is a live event loop). [`store::MemStore`] emits
//! [`PersistOp`](crate::store::PersistOp)s over a channel; the main thread drains them and calls
//! [`Opfs::write`]. At startup [`load_opfs`] hydrates an [`InitialState`] from the on-disk layout:
//!
//! - `index.json` — the structured snapshot (no bodies): publications, follows, cursors, settings,
//!   per-doc `{meta, read}`, and the list of blob CIDs.
//! - `prefs.json` — the complete appearance configuration (theme/layout/per-blog overrides).
//! - `b/<key>` — one document's `RichDoc` body (`key` = [`body_key`] of its AT-URI).
//! - `i/<cid>` — one image blob (CID is already filesystem-safe).
//!
//! Everything here is **best-effort**: a missing file is a cache miss, and any failure (no OPFS,
//! quota, corrupt bytes) degrades to in-memory — the reader must never break on a disk problem.

use std::collections::{HashMap, HashSet, VecDeque};

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    File, FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions,
    FileSystemGetFileOptions, FileSystemWritableFileStream,
};

use standard_frontend::prefs::Prefs;

use crate::store::{IndexDto, InitialState, PersistOp};

/// The structured snapshot file (in the OPFS root).
pub const INDEX_FILE: &str = "index.json";
/// User appearance preferences, separate from the re-fetchable content cache.
pub const PREFS_FILE: &str = "prefs.json";
/// Subdirectory holding per-document `RichDoc` bodies.
pub const BODY_DIR: &str = "b";
/// Subdirectory holding per-image blobs.
pub const BLOB_DIR: &str = "i";

/// Everything needed to start the worker and UI after the async OPFS read phase.
#[derive(Default)]
pub struct BootstrapState {
    pub store: InitialState,
    pub prefs: Prefs,
}

/// One concrete write selected from the coalescing queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueuedWrite {
    Body { key: String, bytes: Vec<u8> },
    Blob { cid: String, bytes: Vec<u8> },
    Index(Vec<u8>),
    Prefs(Vec<u8>),
}

impl QueuedWrite {
    pub fn target(&self) -> (Option<&'static str>, &str, &[u8]) {
        match self {
            Self::Body { key, bytes } => (Some(BODY_DIR), key, bytes),
            Self::Blob { cid, bytes } => (Some(BLOB_DIR), cid, bytes),
            Self::Index(bytes) => (None, INDEX_FILE, bytes),
            Self::Prefs(bytes) => (None, PREFS_FILE, bytes),
        }
    }

    pub fn label(&self) -> String {
        let (dir, name, _) = self.target();
        match dir {
            Some(dir) => format!("{dir}/{name}"),
            None => name.to_string(),
        }
    }
}

/// Coalescing single-writer queue. Payload files are always selected before the index snapshot
/// that references them; hot mutable snapshots and repeated body updates keep only their latest
/// bytes. Blob CIDs become write-once only after OPFS confirms success.
#[derive(Default)]
pub struct WriteQueue {
    body_order: VecDeque<String>,
    bodies: HashMap<String, Vec<u8>>,
    blob_order: VecDeque<String>,
    blobs: HashMap<String, Vec<u8>>,
    completed_blobs: HashSet<String>,
    index: Option<Vec<u8>>,
    prefs: Option<Vec<u8>>,
}

impl WriteQueue {
    pub fn new(completed_blobs: impl IntoIterator<Item = String>) -> Self {
        Self {
            completed_blobs: completed_blobs.into_iter().collect(),
            ..Self::default()
        }
    }

    pub fn push(&mut self, op: PersistOp) {
        match op {
            PersistOp::Index(bytes) => self.index = Some(bytes),
            PersistOp::Prefs(bytes) => self.prefs = Some(bytes),
            PersistOp::Body { key, bytes } => {
                if !self.bodies.contains_key(&key) {
                    self.body_order.push_back(key.clone());
                }
                self.bodies.insert(key, bytes);
            }
            PersistOp::Blob { cid, bytes } => {
                if self.completed_blobs.contains(&cid) || self.blobs.contains_key(&cid) {
                    return;
                }
                self.blob_order.push_back(cid.clone());
                self.blobs.insert(cid, bytes);
            }
        }
    }

    pub fn pop_next(&mut self) -> Option<QueuedWrite> {
        while let Some(key) = self.body_order.pop_front() {
            if let Some(bytes) = self.bodies.remove(&key) {
                return Some(QueuedWrite::Body { key, bytes });
            }
        }
        while let Some(cid) = self.blob_order.pop_front() {
            if let Some(bytes) = self.blobs.remove(&cid) {
                return Some(QueuedWrite::Blob { cid, bytes });
            }
        }
        self.index
            .take()
            .map(QueuedWrite::Index)
            .or_else(|| self.prefs.take().map(QueuedWrite::Prefs))
    }

    /// Record durable completion. Failed blobs deliberately remain eligible for a future emit.
    pub fn mark_succeeded(&mut self, write: &QueuedWrite) {
        if let QueuedWrite::Blob { cid, .. } = write {
            self.completed_blobs.insert(cid.clone());
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
            && self.blobs.is_empty()
            && self.index.is_none()
            && self.prefs.is_none()
    }
}

#[derive(Debug)]
pub struct OpfsError(String);

impl std::fmt::Display for OpfsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "opfs: {}", self.0)
    }
}

impl std::error::Error for OpfsError {}

fn js_err(ctx: &str, e: JsValue) -> OpfsError {
    OpfsError(format!("{ctx}: {e:?}"))
}

async fn await_js(promise: js_sys::Promise, ctx: &str) -> Result<JsValue, OpfsError> {
    JsFuture::from(promise).await.map_err(|e| js_err(ctx, e))
}

/// A handle to the origin-private file system root. Main-thread only (the handles are `!Send`).
pub struct Opfs {
    root: FileSystemDirectoryHandle,
}

impl Opfs {
    /// `navigator.storage.getDirectory()` — the OPFS root. Available on the main thread in a
    /// cross-origin-isolated context (our COOP/COEP headers don't disable it).
    pub async fn open() -> Result<Opfs, OpfsError> {
        let win = web_sys::window().ok_or_else(|| OpfsError("no window".into()))?;
        let storage = win.navigator().storage();
        let root = await_js(storage.get_directory(), "getDirectory")
            .await?
            .dyn_into::<FileSystemDirectoryHandle>()
            .map_err(|e| js_err("root handle", e))?;
        Ok(Opfs { root })
    }

    /// Resolve a directory handle: `None` → the root; `Some(name)` → a (optionally created) subdir.
    async fn resolve_dir(
        &self,
        sub: Option<&str>,
        create: bool,
    ) -> Result<FileSystemDirectoryHandle, OpfsError> {
        match sub {
            None => Ok(self.root.clone()),
            Some(name) => {
                let opts = FileSystemGetDirectoryOptions::new();
                opts.set_create(create);
                await_js(
                    self.root.get_directory_handle_with_options(name, &opts),
                    "get_directory_handle",
                )
                .await?
                .dyn_into::<FileSystemDirectoryHandle>()
                .map_err(|e| js_err("dir handle", e))
            }
        }
    }

    /// Read a file's bytes. A missing file (or missing subdir) is `Ok(None)` — a cache miss, not
    /// an error.
    pub async fn read(&self, sub: Option<&str>, name: &str) -> Result<Option<Vec<u8>>, OpfsError> {
        // A subdir that was never written yet ⇒ no such file ⇒ absent.
        let Ok(dir) = self.resolve_dir(sub, false).await else {
            return Ok(None);
        };
        let opts = FileSystemGetFileOptions::new();
        opts.set_create(false);
        // create:false rejects when the file is absent — treat any handle-get failure as a miss.
        let fh = match JsFuture::from(dir.get_file_handle_with_options(name, &opts)).await {
            Ok(v) => v
                .dyn_into::<FileSystemFileHandle>()
                .map_err(|e| js_err("file handle", e))?,
            Err(_) => return Ok(None),
        };
        let file = await_js(fh.get_file(), "get_file")
            .await?
            .dyn_into::<File>()
            .map_err(|e| js_err("file", e))?;
        let buf = await_js(file.array_buffer(), "array_buffer")
            .await?
            .dyn_into::<js_sys::ArrayBuffer>()
            .map_err(|e| js_err("array_buffer", e))?;
        let view = js_sys::Uint8Array::new(&buf);
        let mut bytes = vec![0u8; view.length() as usize];
        view.copy_to(&mut bytes);
        Ok(Some(bytes))
    }

    /// Write a file (truncating overwrite — `create_writable` opens a fresh stream that replaces
    /// the contents on `close`). Creates the subdir on first use.
    pub async fn write(
        &self,
        sub: Option<&str>,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), OpfsError> {
        let dir = self.resolve_dir(sub, true).await?;
        let opts = FileSystemGetFileOptions::new();
        opts.set_create(true);
        let fh = await_js(
            dir.get_file_handle_with_options(name, &opts),
            "get_file_handle",
        )
        .await?
        .dyn_into::<FileSystemFileHandle>()
        .map_err(|e| js_err("file handle", e))?;
        let stream = await_js(fh.create_writable(), "create_writable")
            .await?
            .dyn_into::<FileSystemWritableFileStream>()
            .map_err(|e| js_err("writable", e))?;
        // `write_with_u8_array(&[u8])` would expose a view of wasm's linear memory. This build uses
        // wasm threads, so that view is backed by `SharedArrayBuffer`, which OPFS rejects. Construct
        // a JS-owned Uint8Array first: `new_from_slice` copies into a normal ArrayBuffer.
        let data = js_sys::Uint8Array::new_from_slice(bytes);
        let write = stream
            .write_with_js_u8_array(&data)
            .map_err(|e| js_err("write", e))?;
        await_js(write, "write await").await?;
        await_js(stream.close(), "close").await?; // close() is inherited from WritableStream
        Ok(())
    }
}

/// Map a document AT-URI to a filesystem-safe `b/<key>` filename. AT-URIs carry `/`, `:`, `.`
/// (illegal in OPFS names), so we hash. **FNV-1a (64-bit)** — small, dependency-free, and *stable*
/// across releases (unlike `DefaultHasher`, whose algorithm may change, which would orphan the
/// whole body cache). A collision merely overwrites one re-fetchable body; harmless for a cache.
pub fn body_key(uri: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in uri.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn decode_prefs(bytes: Option<&[u8]>) -> Prefs {
    bytes
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
        .unwrap_or_default()
}

/// Hydrate cache + preferences from OPFS before `worker::spawn`. Each top-level file is independent:
/// corrupt cache data cannot discard valid preferences, and corrupt preferences cannot discard the
/// offline reading cache. Referenced reads are sequential (simple; bounded concurrency can follow).
pub async fn load_opfs(opfs: &Opfs) -> BootstrapState {
    let prefs_bytes = opfs.read(None, PREFS_FILE).await.ok().flatten();
    let prefs = decode_prefs(prefs_bytes.as_deref());
    let store = load_store(opfs).await;
    BootstrapState { store, prefs }
}

async fn load_store(opfs: &Opfs) -> InitialState {
    let Ok(Some(bytes)) = opfs.read(None, INDEX_FILE).await else {
        return InitialState::default();
    };
    let Ok(dto) = serde_json::from_slice::<IndexDto>(&bytes) else {
        return InitialState::default();
    };

    // Bodies only if the schema still matches (else they'd fail to deserialize / are stale).
    let mut bodies = HashMap::new();
    if dto.schema_matches() {
        for key in dto.body_keys() {
            if let Ok(Some(b)) = opfs.read(Some(BODY_DIR), &key).await {
                bodies.insert(key, b);
            }
        }
    }
    // Blobs are raw bytes — format-stable, kept across a schema bump (no re-fetch needed).
    let mut blobs = HashMap::new();
    for cid in dto.blob_cids() {
        if let Ok(Some(b)) = opfs.read(Some(BLOB_DIR), cid).await {
            blobs.insert(cid.clone(), b);
        }
    }

    InitialState::from_index(dto, &bodies, blobs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use standard_frontend::prefs::LayoutKind;

    #[test]
    fn body_key_is_stable_and_filesystem_safe() {
        assert_eq!(body_key(""), "cbf29ce484222325");
        let key = body_key("at://did:plc:test/site.standard.document/post");
        assert_eq!(
            key,
            body_key("at://did:plc:test/site.standard.document/post")
        );
        assert_eq!(key.len(), 16);
        assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn queue_coalesces_snapshots_and_bodies_then_writes_payloads_first() {
        let mut queue = WriteQueue::default();
        queue.push(PersistOp::Index(vec![1]));
        queue.push(PersistOp::Prefs(vec![2]));
        queue.push(PersistOp::Body {
            key: "body".into(),
            bytes: vec![3],
        });
        queue.push(PersistOp::Body {
            key: "body".into(),
            bytes: vec![4],
        });
        queue.push(PersistOp::Blob {
            cid: "cid".into(),
            bytes: vec![5],
        });
        queue.push(PersistOp::Index(vec![6]));
        queue.push(PersistOp::Prefs(vec![7]));

        assert_eq!(
            queue.pop_next(),
            Some(QueuedWrite::Body {
                key: "body".into(),
                bytes: vec![4]
            })
        );
        assert_eq!(
            queue.pop_next(),
            Some(QueuedWrite::Blob {
                cid: "cid".into(),
                bytes: vec![5]
            })
        );
        assert_eq!(queue.pop_next(), Some(QueuedWrite::Index(vec![6])));
        assert_eq!(queue.pop_next(), Some(QueuedWrite::Prefs(vec![7])));
        assert!(queue.pop_next().is_none());
    }

    #[test]
    fn blob_is_deduplicated_only_after_success() {
        let mut queue = WriteQueue::new(["already".to_string()]);
        queue.push(PersistOp::Blob {
            cid: "already".into(),
            bytes: vec![1],
        });
        assert!(queue.is_empty(), "startup-loaded cid is already durable");

        queue.push(PersistOp::Blob {
            cid: "retry".into(),
            bytes: vec![2],
        });
        queue.push(PersistOp::Blob {
            cid: "retry".into(),
            bytes: vec![3],
        });
        let failed = queue.pop_next().unwrap();
        assert_eq!(failed.target().2, &[2]);

        // No success mark: a later store emit remains eligible to retry.
        queue.push(PersistOp::Blob {
            cid: "retry".into(),
            bytes: vec![4],
        });
        let succeeded = queue.pop_next().unwrap();
        queue.mark_succeeded(&succeeded);
        queue.push(PersistOp::Blob {
            cid: "retry".into(),
            bytes: vec![5],
        });
        assert!(queue.is_empty(), "successful cid is now write-once");
    }

    #[test]
    fn preferences_round_trip_and_corruption_falls_back_to_onboarding_defaults() {
        let mut prefs = Prefs {
            theme: "light".into(),
            layout: LayoutKind::ThreePane,
            sidebar_width: 44,
            posts_width: 50,
            onboarded: true,
            ..Prefs::default()
        };
        prefs.edit_blog("at://did:plc:test/site.standard.publication/main", |blog| {
            blog.layout = Some(LayoutKind::OnePane);
            blog.theme = Some("sepia".into());
        });
        let bytes = serde_json::to_vec(&prefs).unwrap();
        assert_eq!(decode_prefs(Some(&bytes)), prefs);
        assert_eq!(decode_prefs(Some(b"not json")), Prefs::default());
        assert_eq!(decode_prefs(None), Prefs::default());
        assert!(!decode_prefs(None).onboarded);
    }
}
