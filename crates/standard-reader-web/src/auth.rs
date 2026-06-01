//! M1b has no sign-in: a stub [`AuthProvider`] passed as `None::<NoAuth>` to the worker. The worker
//! guards every auth call, so these bodies never actually run — `restore` returning `Ok(None)` just
//! means "signed out." Real browser OAuth is M3.

use standard_frontend::account::Account;
use standard_frontend::auth_provider::{AuthError, AuthProvider};

pub struct NoAuth;

impl AuthProvider for NoAuth {
    fn restore(&self) -> Result<Option<Account>, AuthError> {
        Ok(None)
    }
    fn login(&self, _ident: &str, _progress: &dyn Fn(String)) -> Result<Account, AuthError> {
        Err("sign-in is not available in this build".into())
    }
    fn logout(&self) -> Result<(), AuthError> {
        Ok(())
    }
    fn create_subscription(&self, _did: &str, _publication_uri: &str) -> Result<String, AuthError> {
        Err("sign-in is not available in this build".into())
    }
    fn delete_subscription(&self, _did: &str, _rkey: &str) -> Result<(), AuthError> {
        Err("sign-in is not available in this build".into())
    }
}
