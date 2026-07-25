//! The auth seam: sign-in + subscription writes the worker needs, as a **synchronous (blocking)**
//! trait — matching how the synchronous worker already drives auth (it `block_on`s an async client
//! today; the runtime is an implementation detail of the impl). The desktop impl owns a tokio
//! runtime and blocks on `atrium-oauth`; the web impl returns a redirect for its shell to navigate.
//! The trait carries only plain types (no atrium), so it moves into
//! `standard-frontend` at the crate carve.

use crate::account::Account;

/// Any failure in the auth path — boxed so atrium / reqwest / io errors all convert with `?`.
pub type AuthError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Identity plus only the record operations the reader performs while mirroring subscriptions.
pub const ATPROTO_IDENTITY_SCOPE: &str = "atproto";
pub const SUBSCRIPTION_PERMISSION_SCOPE: &str =
    "repo:site.standard.graph.subscription?action=create&action=delete";
const SUBSCRIPTION_REPO_SCOPE: &str = "repo:site.standard.graph.subscription";

/// Reject old transitional grants and any unexpected extra permission. Existing sessions issued
/// before the granular-scope migration must re-authorize instead of silently retaining app-password
/// level access.
pub fn has_exact_subscription_scope(granted: Option<&str>) -> bool {
    let scopes = granted
        .unwrap_or_default()
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    if scopes.len() != 2 {
        return false;
    }
    scopes.contains(&ATPROTO_IDENTITY_SCOPE)
        && scopes.iter().copied().any(is_exact_subscription_permission)
}

fn is_exact_subscription_permission(scope: &str) -> bool {
    let Some((resource, query)) = scope.split_once('?') else {
        return false;
    };
    if resource != SUBSCRIPTION_REPO_SCOPE {
        return false;
    }
    let mut actions = query.split('&').collect::<Vec<_>>();
    actions.sort_unstable();
    actions == ["action=create", "action=delete"]
}

/// What a shell's sign-in step produced.
///
/// Native shells can complete a loopback flow before returning. Browser shells must first persist
/// PKCE/DPoP transaction state, then navigate the whole page to the authorization server and
/// finish the exchange during [`AuthProvider::restore`] after the redirect returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    Authenticated(Account),
    Redirect(String),
}

pub trait AuthProvider {
    /// Validate + restore a persisted session. `Ok(None)` = no stored session.
    fn restore(&self) -> Result<Option<Account>, AuthError>;
    /// Run the sign-in flow for a handle/DID; `progress` reports each step to the UI/log.
    fn login(&self, ident: &str, progress: &dyn Fn(String)) -> Result<LoginOutcome, AuthError>;
    /// Revoke the session upstream (best-effort) and forget it locally.
    fn logout(&self) -> Result<(), AuthError>;
    /// Create a `site.standard.graph.subscription` record in the user's repo; returns its rkey.
    fn create_subscription(&self, did: &str, publication_uri: &str) -> Result<String, AuthError>;
    /// Delete the subscription record identified by `rkey` from the user's repo.
    fn delete_subscription(&self, did: &str, rkey: &str) -> Result<(), AuthError>;
}

#[cfg(test)]
mod tests {
    use super::{
        ATPROTO_IDENTITY_SCOPE, SUBSCRIPTION_PERMISSION_SCOPE, SUBSCRIPTION_REPO_SCOPE,
        has_exact_subscription_scope,
    };

    #[test]
    fn accepts_only_the_two_required_permissions_in_either_order() {
        let forward = format!("{ATPROTO_IDENTITY_SCOPE} {SUBSCRIPTION_PERMISSION_SCOPE}");
        let reverse = format!("{SUBSCRIPTION_PERMISSION_SCOPE} {ATPROTO_IDENTITY_SCOPE}");
        let reversed_actions = format!(
            "{ATPROTO_IDENTITY_SCOPE} {SUBSCRIPTION_REPO_SCOPE}?action=delete&action=create"
        );
        assert!(has_exact_subscription_scope(Some(&forward)));
        assert!(has_exact_subscription_scope(Some(&reverse)));
        assert!(has_exact_subscription_scope(Some(&reversed_actions)));
    }

    #[test]
    fn rejects_transitional_missing_and_extra_permissions() {
        assert!(!has_exact_subscription_scope(Some(
            "atproto transition:generic"
        )));
        assert!(!has_exact_subscription_scope(Some("atproto")));
        assert!(!has_exact_subscription_scope(Some(&format!(
            "atproto {SUBSCRIPTION_PERMISSION_SCOPE} blob:*/*"
        ))));
        assert!(!has_exact_subscription_scope(Some(
            "atproto repo:site.standard.graph.subscription?action=create&action=update"
        )));
        assert!(!has_exact_subscription_scope(None));
    }
}
