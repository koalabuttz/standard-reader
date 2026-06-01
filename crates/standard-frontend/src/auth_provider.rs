//! The auth seam: sign-in + subscription writes the worker needs, as a **synchronous (blocking)**
//! trait — matching how the synchronous worker already drives auth (it `block_on`s an async client
//! today; the runtime is an implementation detail of the impl). The desktop impl owns a tokio
//! runtime and blocks on `atrium-oauth`; a future web impl can drive the browser's redirect OAuth
//! via its own host mechanism. The trait carries only plain types (no atrium), so it moves into
//! `standard-frontend` at the crate carve.

use crate::account::Account;

/// Any failure in the auth path — boxed so atrium / reqwest / io errors all convert with `?`.
pub type AuthError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub trait AuthProvider {
    /// Validate + restore a persisted session. `Ok(None)` = no stored session.
    fn restore(&self) -> Result<Option<Account>, AuthError>;
    /// Run the sign-in flow for a handle/DID; `progress` reports each step to the UI/log.
    fn login(&self, ident: &str, progress: &dyn Fn(String)) -> Result<Account, AuthError>;
    /// Revoke the session upstream (best-effort) and forget it locally.
    fn logout(&self) -> Result<(), AuthError>;
    /// Create a `site.standard.graph.subscription` record in the user's repo; returns its rkey.
    fn create_subscription(&self, did: &str, publication_uri: &str) -> Result<String, AuthError>;
    /// Delete the subscription record identified by `rkey` from the user's repo.
    fn delete_subscription(&self, did: &str, rkey: &str) -> Result<(), AuthError>;
}
