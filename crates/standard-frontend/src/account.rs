//! The signed-in identity — plain, platform-agnostic data shared by the worker, the UI, and the
//! desktop auth layer. It lives here (not tied to `atrium-oauth`) so it can move into the
//! `standard-frontend` crate at the carve; the desktop `auth` module re-exports it.

use serde::{Deserialize, Serialize};

/// The signed-in identity, persisted in `account.json` so startup needn't hit the network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub did: String,
    pub handle: String,
}
