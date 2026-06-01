//! The frontend cache seam: operations the worker needs beyond the core [`Store`].
//!
//! `standard-core`'s [`Store`] covers documents / publications / blobs / cursors / search. The
//! frontend additionally owns the **local follow-list** (with each follow's atproto subscription
//! rkey) and the **show-images** preference. Those live here as a `Store` supertrait so the shared
//! worker can be generic over the cache — desktop satisfies it with `redb`, a future web frontend
//! with OPFS/IndexedDB. (Moves into `standard-frontend` at the crate carve.)

use standard_core::store::Store;

pub trait FrontendStore: Store {
    /// Followed publication URIs — the app's own subscriptions, persisted without atproto auth.
    fn follows(&self) -> Result<Vec<String>, Self::Error>;
    fn is_followed(&self, publication_uri: &str) -> Result<bool, Self::Error>;
    fn follow(&mut self, publication_uri: &str) -> Result<(), Self::Error>;
    fn unfollow(&mut self, publication_uri: &str) -> Result<(), Self::Error>;
    /// The upstream `site.standard.graph.subscription` rkey for a follow, if known (empty → `None`).
    fn follow_rkey(&self, publication_uri: &str) -> Result<Option<String>, Self::Error>;
    fn set_follow_rkey(&mut self, publication_uri: &str, rkey: &str) -> Result<(), Self::Error>;
    /// The persisted "download + render images" preference (defaults to on).
    fn show_images(&self) -> Result<bool, Self::Error>;
    fn set_show_images(&mut self, on: bool) -> Result<(), Self::Error>;
}
