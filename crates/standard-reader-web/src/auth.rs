//! Direct browser OAuth: Atrium's DPoP/PKCE/PAR client over synchronous worker XHR, with the
//! transaction and session material durably committed to OPFS by the browser main thread.
//!
//! The shared frontend worker is deliberately synchronous. That is a good fit here: OAuth runs in
//! the Web Worker, XHR is synchronous there, and Atrium's otherwise-async futures can be driven to
//! completion without an async runtime. OPFS remains main-thread async, so every security-critical
//! snapshot write carries an acknowledgement; redirect/refresh/logout cannot outrun durability.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::mpsc::{Sender, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};

use atrium_api::agent::{Agent, SessionManager};
use atrium_api::com::atproto::repo::{create_record, delete_record};
use atrium_api::types::TryIntoUnknown;
use atrium_api::types::string::{AtIdentifier, Datetime, Did, Nsid, RecordKey};
use atrium_common::store::Store;
use atrium_identity::did::{CommonDidResolver, CommonDidResolverConfig, DEFAULT_PLC_DIRECTORY_URL};
use atrium_identity::handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig, DnsTxtResolver};
use atrium_oauth::store::session::{Session, SessionStore};
use atrium_oauth::store::state::{InternalStateData, StateStore};
use atrium_oauth::{
    AtprotoClientMetadata, AuthMethod, AuthorizeOptions, CallbackParams, GrantType, KnownScope,
    OAuthClient, OAuthClientConfig, OAuthResolverConfig, Scope,
};
use atrium_xrpc::{HttpClient, http};
use serde::{Deserialize, Serialize};
use standard_frontend::account::Account;
use standard_frontend::auth_provider::{
    AuthError, AuthProvider, LoginOutcome, SUBSCRIPTION_PERMISSION_SCOPE,
    has_exact_subscription_scope,
};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

const CLIENT_ID: &str = "https://www.davidlewis.xyz/standard-reader/web_client_metadata.json";
const CLIENT_URI: &str = "https://www.davidlewis.xyz/standard-reader";
const REDIRECT_URI: &str = "https://www.davidlewis.xyz/standard-reader/app/";
const SUBSCRIPTION_NSID: &str = "site.standard.graph.subscription";
const AUTH_SCHEMA: u32 = 1;
const AUTH_STATE_TTL_MS: u64 = 10 * 60 * 1_000;
#[cfg(target_arch = "wasm32")]
const MAX_OAUTH_RESPONSE_BYTES: u32 = 4 * 1024 * 1024;
pub const AUTH_FILE: &str = "auth.json";

type Client = OAuthClient<
    BrowserStore,
    BrowserStore,
    CommonDidResolver<WebHttpClient>,
    AtprotoHandleResolver<DohDnsTxtResolver, WebHttpClient>,
    WebHttpClient,
>;

/// Captured before the shell removes OAuth parameters from the address bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthReturn {
    Success(CallbackParams),
    Error {
        state: Option<String>,
        error: String,
        description: Option<String>,
    },
}

/// One pending authorization transaction plus its local expiry metadata.
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredState {
    created_at_ms: u64,
    data: InternalStateData,
}

/// The one non-cache OPFS document. Tokens and the private DPoP key live only here and in the
/// worker's memory; never in localStorage, a URL, the DOM, or a log.
#[derive(Clone, Serialize, Deserialize)]
pub struct AuthSnapshot {
    schema: u32,
    account: Option<Account>,
    states: BTreeMap<String, StoredState>,
    sessions: BTreeMap<String, Session>,
}

impl Default for AuthSnapshot {
    fn default() -> Self {
        Self {
            schema: AUTH_SCHEMA,
            account: None,
            states: BTreeMap::new(),
            sessions: BTreeMap::new(),
        }
    }
}

impl AuthSnapshot {
    pub fn decode(bytes: Option<&[u8]>) -> Self {
        let Some(bytes) = bytes else {
            return Self::default();
        };
        let Ok(mut value) = serde_json::from_slice::<Self>(bytes) else {
            return Self::default();
        };
        if value.schema != AUTH_SCHEMA {
            return Self::default();
        }
        value.prune_expired(now_ms());
        value
    }

    fn prune_expired(&mut self, now: u64) {
        self.states
            .retain(|_, state| now.saturating_sub(state.created_at_ms) <= AUTH_STATE_TTL_MS);
    }
}

/// Security-critical writes use their own request/ack path instead of the cache's best-effort,
/// coalescing write queue.
pub struct AuthStorageRequest {
    pub bytes: Vec<u8>,
    pub ack: SyncSender<Result<(), String>>,
}

#[derive(Debug, Clone)]
pub struct AuthStoreError(String);

impl fmt::Display for AuthStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for AuthStoreError {}

/// Atrium state + session store sharing one atomically replaced OPFS snapshot.
#[derive(Clone)]
struct BrowserStore {
    state: Arc<Mutex<AuthSnapshot>>,
    persist_tx: Sender<AuthStorageRequest>,
}

impl BrowserStore {
    fn new(initial: AuthSnapshot, persist_tx: Sender<AuthStorageRequest>) -> Self {
        Self {
            state: Arc::new(Mutex::new(initial)),
            persist_tx,
        }
    }

    fn snapshot(&self) -> Result<AuthSnapshot, AuthStoreError> {
        self.state
            .lock()
            .map(|state| state.clone())
            .map_err(|_| AuthStoreError("browser auth store lock poisoned".into()))
    }

    /// Mutate, serialize, and wait for durable OPFS completion. A failed write restores the prior
    /// in-memory value, so callers never observe a commit that did not survive reload.
    fn commit(&self, update: impl FnOnce(&mut AuthSnapshot)) -> Result<(), AuthStoreError> {
        let (before, bytes) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| AuthStoreError("browser auth store lock poisoned".into()))?;
            let before = state.clone();
            update(&mut state);
            let bytes = serde_json::to_vec(&*state)
                .map_err(|e| AuthStoreError(format!("serializing browser auth: {e}")))?;
            (before, bytes)
        };

        let (ack_tx, ack_rx) = sync_channel(1);
        let result = self
            .persist_tx
            .send(AuthStorageRequest { bytes, ack: ack_tx })
            .map_err(|_| AuthStoreError("browser auth storage service stopped".into()))
            .and_then(|_| {
                ack_rx
                    .recv()
                    .map_err(|_| {
                        AuthStoreError("browser auth storage acknowledgement lost".into())
                    })?
                    .map_err(AuthStoreError)
            });
        if result.is_err()
            && let Ok(mut state) = self.state.lock()
        {
            *state = before;
        }
        result
    }

    fn account(&self) -> Result<Option<Account>, AuthStoreError> {
        let state = self.snapshot()?;
        Ok(state.account.or_else(|| {
            state.sessions.keys().next().map(|did| Account {
                did: did.clone(),
                handle: did.clone(),
            })
        }))
    }

    fn set_account(&self, account: Option<Account>) -> Result<(), AuthStoreError> {
        self.commit(|state| state.account = account)
    }

    fn discard_state(&self, key: &str) -> Result<(), AuthStoreError> {
        self.commit(|state| {
            state.states.remove(key);
        })
    }

    fn clear_all(&self) -> Result<(), AuthStoreError> {
        self.commit(|state| *state = AuthSnapshot::default())
    }

    fn granted_scope(&self, did: &Did) -> Result<Option<String>, AuthStoreError> {
        Ok(self
            .snapshot()?
            .sessions
            .get(did.as_ref())
            .and_then(|session| session.token_set.scope.clone()))
    }
}

impl Store<String, InternalStateData> for BrowserStore {
    type Error = AuthStoreError;

    async fn get(&self, key: &String) -> Result<Option<InternalStateData>, Self::Error> {
        Ok(self
            .snapshot()?
            .states
            .get(key)
            .filter(|state| now_ms().saturating_sub(state.created_at_ms) <= AUTH_STATE_TTL_MS)
            .map(|state| state.data.clone()))
    }

    async fn set(&self, key: String, value: InternalStateData) -> Result<(), Self::Error> {
        self.commit(|state| {
            state.states.insert(
                key,
                StoredState {
                    created_at_ms: now_ms(),
                    data: value,
                },
            );
        })
    }

    async fn del(&self, key: &String) -> Result<(), Self::Error> {
        self.discard_state(key)
    }

    async fn clear(&self) -> Result<(), Self::Error> {
        self.commit(|state| state.states.clear())
    }
}

impl StateStore for BrowserStore {}

impl Store<Did, Session> for BrowserStore {
    type Error = AuthStoreError;

    async fn get(&self, key: &Did) -> Result<Option<Session>, Self::Error> {
        Ok(self.snapshot()?.sessions.get(key.as_ref()).cloned())
    }

    async fn set(&self, key: Did, value: Session) -> Result<(), Self::Error> {
        self.commit(|state| {
            state.sessions.insert(key.as_ref().to_string(), value);
        })
    }

    async fn del(&self, key: &Did) -> Result<(), Self::Error> {
        self.commit(|state| {
            state.sessions.remove(key.as_ref());
        })
    }

    async fn clear(&self) -> Result<(), Self::Error> {
        self.commit(|state| state.sessions.clear())
    }
}

impl SessionStore for BrowserStore {}

pub struct WebAuth {
    client: Client,
    store: BrowserStore,
    callback: Mutex<Option<OAuthReturn>>,
}

impl WebAuth {
    pub fn new(
        initial: AuthSnapshot,
        callback: Option<OAuthReturn>,
        persist_tx: Sender<AuthStorageRequest>,
    ) -> Result<Self, AuthError> {
        let store = BrowserStore::new(initial, persist_tx);
        let client = build_client(store.clone())?;
        Ok(Self {
            client,
            store,
            callback: Mutex::new(callback),
        })
    }

    async fn restore_inner(&self) -> Result<Option<Account>, AuthError> {
        let callback = self
            .callback
            .lock()
            .map_err(|_| "browser OAuth callback lock poisoned")?
            .take();
        if let Some(callback) = callback {
            match callback {
                OAuthReturn::Success(params) => {
                    let (session, original_ident) = self.client.callback(params).await?;
                    let did = session
                        .did()
                        .await
                        .ok_or("the OAuth session had no DID")?
                        .clone();
                    self.ensure_subscription_scope(&did).await?;
                    let did = did.as_ref().to_string();
                    let ident = original_ident.unwrap_or_else(|| did.clone());
                    let handle = if ident.starts_with("did:") {
                        did.clone()
                    } else {
                        ident.trim().trim_start_matches('@').to_string()
                    };
                    let account = Account { did, handle };
                    self.store.set_account(Some(account.clone()))?;
                    return Ok(Some(account));
                }
                OAuthReturn::Error {
                    state,
                    error,
                    description,
                } => {
                    if let Some(state) = state {
                        let _ = self.store.discard_state(&state);
                    }
                    return Err(match description {
                        Some(description) => format!("{error}: {description}").into(),
                        None => error.into(),
                    });
                }
            }
        }

        let Some(account) = self.store.account()? else {
            return Ok(None);
        };
        let did = Did::new(account.did.clone())?;
        self.ensure_subscription_scope(&did).await?;
        self.client.restore(&did).await?;
        self.ensure_subscription_scope(&did).await?;
        Ok(Some(account))
    }

    async fn ensure_subscription_scope(&self, did: &Did) -> Result<(), AuthError> {
        let granted = self.store.granted_scope(did)?;
        if has_exact_subscription_scope(granted.as_deref()) {
            return Ok(());
        }

        let _ = self.client.revoke(did).await;
        self.store.clear_all()?;
        Err("the saved OAuth session has obsolete or unexpected permissions; sign in again".into())
    }

    async fn login_inner(
        &self,
        ident: &str,
        progress: &dyn Fn(String),
    ) -> Result<LoginOutcome, AuthError> {
        progress(format!(
            "requesting authorization for {ident} (identity + PAR)…"
        ));
        let url = self
            .client
            .authorize(
                ident,
                AuthorizeOptions {
                    scopes: oauth_scopes(),
                    // Atrium stores this inside the transaction; it never appears in the OAuth
                    // `state` parameter and gives the callback a friendly account handle.
                    state: Some(ident.to_string()),
                    ..Default::default()
                },
            )
            .await?;
        progress("authorization transaction saved; redirecting…".into());
        Ok(LoginOutcome::Redirect(url))
    }

    async fn logout_inner(&self) -> Result<(), AuthError> {
        if let Some(account) = self.store.account()?
            && let Ok(did) = Did::new(account.did)
        {
            let _ = self.client.revoke(&did).await;
        }
        self.store.clear_all()?;
        Ok(())
    }

    async fn create_subscription_inner(
        &self,
        did: &str,
        publication_uri: &str,
    ) -> Result<String, AuthError> {
        let did = Did::new(did.to_string())?;
        let agent = Agent::new(self.client.restore(&did).await?);
        let record = SubscriptionRecord {
            r#type: SUBSCRIPTION_NSID,
            publication: publication_uri,
            created_at: Datetime::now().as_str().to_string(),
        }
        .try_into_unknown()?;
        let output = agent
            .api
            .com
            .atproto
            .repo
            .create_record(
                create_record::InputData {
                    collection: Nsid::new(SUBSCRIPTION_NSID.to_string())?,
                    repo: AtIdentifier::Did(did),
                    record,
                    rkey: None,
                    swap_commit: None,
                    validate: None,
                }
                .into(),
            )
            .await?;
        Ok(rkey_from_uri(&output.uri))
    }

    async fn delete_subscription_inner(&self, did: &str, rkey: &str) -> Result<(), AuthError> {
        let did = Did::new(did.to_string())?;
        let agent = Agent::new(self.client.restore(&did).await?);
        agent
            .api
            .com
            .atproto
            .repo
            .delete_record(
                delete_record::InputData {
                    collection: Nsid::new(SUBSCRIPTION_NSID.to_string())?,
                    repo: AtIdentifier::Did(did),
                    rkey: RecordKey::new(rkey.to_string())?,
                    swap_commit: None,
                    swap_record: None,
                }
                .into(),
            )
            .await?;
        Ok(())
    }
}

impl AuthProvider for WebAuth {
    fn restore(&self) -> Result<Option<Account>, AuthError> {
        futures_lite::future::block_on(self.restore_inner())
    }

    fn login(&self, ident: &str, progress: &dyn Fn(String)) -> Result<LoginOutcome, AuthError> {
        futures_lite::future::block_on(self.login_inner(ident, progress))
    }

    fn logout(&self) -> Result<(), AuthError> {
        futures_lite::future::block_on(self.logout_inner())
    }

    fn create_subscription(&self, did: &str, publication_uri: &str) -> Result<String, AuthError> {
        futures_lite::future::block_on(self.create_subscription_inner(did, publication_uri))
    }

    fn delete_subscription(&self, did: &str, rkey: &str) -> Result<(), AuthError> {
        futures_lite::future::block_on(self.delete_subscription_inner(did, rkey))
    }
}

#[derive(Serialize)]
struct SubscriptionRecord<'a> {
    #[serde(rename = "$type")]
    r#type: &'a str,
    publication: &'a str,
    #[serde(rename = "createdAt")]
    created_at: String,
}

fn build_client(store: BrowserStore) -> Result<Client, AuthError> {
    let http = WebHttpClient;
    let resolver_http = Arc::new(http);
    let resolver = OAuthResolverConfig {
        did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
            plc_directory_url: DEFAULT_PLC_DIRECTORY_URL.to_string(),
            http_client: Arc::clone(&resolver_http),
        }),
        handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
            dns_txt_resolver: DohDnsTxtResolver { http },
            http_client: Arc::clone(&resolver_http),
        }),
        authorization_server_metadata: Default::default(),
        protected_resource_metadata: Default::default(),
    };
    Ok(OAuthClient::new(OAuthClientConfig {
        client_metadata: AtprotoClientMetadata {
            client_id: CLIENT_ID.to_string(),
            client_uri: Some(CLIENT_URI.to_string()),
            redirect_uris: vec![REDIRECT_URI.to_string()],
            token_endpoint_auth_method: AuthMethod::None,
            grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
            scopes: oauth_scopes(),
            jwks_uri: None,
            token_endpoint_auth_signing_alg: None,
        },
        keys: None,
        resolver,
        state_store: store.clone(),
        session_store: store,
        http_client: http,
    })?)
}

fn oauth_scopes() -> Vec<Scope> {
    vec![
        Scope::Known(KnownScope::Atproto),
        Scope::Unknown(SUBSCRIPTION_PERMISSION_SCOPE.to_string()),
    ]
}

fn rkey_from_uri(uri: &str) -> String {
    uri.rsplit('/').next().unwrap_or_default().to_string()
}

#[derive(Clone, Copy)]
struct WebHttpClient;

impl HttpClient for WebHttpClient {
    async fn send_http(
        &self,
        request: http::Request<Vec<u8>>,
    ) -> Result<http::Response<Vec<u8>>, Box<dyn Error + Send + Sync + 'static>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = request;
            Err("browser HTTP is only available on wasm32".into())
        }
        #[cfg(target_arch = "wasm32")]
        {
            use web_sys::{XmlHttpRequest, XmlHttpRequestResponseType};

            let (parts, body) = request.into_parts();
            let xhr = XmlHttpRequest::new().map_err(|e| js_error("creating XHR", e))?;
            xhr.open_with_async(parts.method.as_str(), &parts.uri.to_string(), false)
                .map_err(|e| js_error("opening XHR", e))?;
            xhr.set_response_type(XmlHttpRequestResponseType::Arraybuffer);
            for (name, value) in &parts.headers {
                xhr.set_request_header(
                    name.as_str(),
                    value
                        .to_str()
                        .map_err(|e| format!("invalid request header {name}: {e}"))?,
                )
                .map_err(|e| js_error("setting XHR request header", e))?;
            }
            let js_body = js_sys::Uint8Array::new_from_slice(&body);
            xhr.send_with_opt_js_u8_array(Some(&js_body))
                .map_err(|e| js_error("sending XHR", e))?;

            let status = xhr
                .status()
                .map_err(|e| js_error("reading XHR status", e))?;
            let mut builder = http::Response::builder().status(status);
            let raw_headers = xhr
                .get_all_response_headers()
                .map_err(|e| js_error("reading XHR response headers", e))?;
            for line in raw_headers.lines() {
                if let Some((name, value)) = line.split_once(':') {
                    builder = builder.header(name.trim(), value.trim());
                }
            }
            let value = xhr
                .response()
                .map_err(|e| js_error("reading XHR response", e))?;
            let array = value
                .dyn_into::<js_sys::ArrayBuffer>()
                .map_err(|e| js_error("XHR response was not an ArrayBuffer", e))?;
            let view = js_sys::Uint8Array::new(&array);
            if view.length() > MAX_OAUTH_RESPONSE_BYTES {
                return Err(format!(
                    "OAuth response exceeded the {} MiB limit",
                    MAX_OAUTH_RESPONSE_BYTES / 1024 / 1024
                )
                .into());
            }
            let mut bytes = vec![0; view.length() as usize];
            view.copy_to(&mut bytes);
            Ok(builder.body(bytes)?)
        }
    }
}

#[derive(Clone)]
struct DohDnsTxtResolver {
    http: WebHttpClient,
}

impl DnsTxtResolver for DohDnsTxtResolver {
    async fn resolve(
        &self,
        query: &str,
    ) -> Result<Vec<String>, Box<dyn Error + Send + Sync + 'static>> {
        let uri = format!("https://dns.google/resolve?name={query}&type=TXT");
        let response = self
            .http
            .send_http(http::Request::get(uri).body(Vec::new())?)
            .await?;
        if !response.status().is_success() {
            return Ok(Vec::new());
        }
        let doc: serde_json::Value = serde_json::from_slice(response.body())?;
        Ok(doc
            .get("Answer")
            .and_then(|answer| answer.as_array())
            .map(|answers| {
                answers
                    .iter()
                    .filter_map(|answer| answer.get("data").and_then(|data| data.as_str()))
                    .map(|value| value.trim_matches('"').to_string())
                    .collect()
            })
            .unwrap_or_default())
    }
}

#[cfg(target_arch = "wasm32")]
fn js_error(context: &str, value: wasm_bindgen::JsValue) -> Box<dyn Error + Send + Sync + 'static> {
    format!("{context}: {value:?}").into()
}

#[cfg(target_arch = "wasm32")]
fn now_ms() -> u64 {
    js_sys::Date::now().max(0.0) as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_snapshot_rejects_corruption_and_unknown_schema() {
        assert_eq!(AuthSnapshot::decode(Some(b"not json")).schema, AUTH_SCHEMA);
        let bytes = br#"{"schema":99,"account":null,"states":{},"sessions":{}}"#;
        assert_eq!(AuthSnapshot::decode(Some(bytes)).schema, AUTH_SCHEMA);
    }

    #[test]
    fn auth_snapshot_round_trips_account() {
        let snapshot = AuthSnapshot {
            account: Some(Account {
                did: "did:plc:test".into(),
                handle: "alice.test".into(),
            }),
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        assert_eq!(AuthSnapshot::decode(Some(&bytes)).account, snapshot.account);
    }

    #[test]
    fn rkey_uses_final_uri_segment() {
        assert_eq!(
            rkey_from_uri("at://did:plc:test/site.standard.graph.subscription/abc"),
            "abc"
        );
    }

    #[test]
    fn acknowledged_auth_write_commits() {
        let (tx, rx) = std::sync::mpsc::channel();
        let store = BrowserStore::new(AuthSnapshot::default(), tx);
        let writer_store = store.clone();
        let account = Account {
            did: "did:plc:test".into(),
            handle: "alice.test".into(),
        };
        let expected = account.clone();
        let writer = std::thread::spawn(move || writer_store.set_account(Some(account)));

        let request = rx.recv().unwrap();
        let durable = AuthSnapshot::decode(Some(&request.bytes));
        assert_eq!(durable.account, Some(expected.clone()));
        request.ack.send(Ok(())).unwrap();
        writer.join().unwrap().unwrap();
        assert_eq!(store.account().unwrap(), Some(expected));
    }

    #[test]
    fn failed_auth_write_rolls_back_memory() {
        let (tx, rx) = std::sync::mpsc::channel();
        let store = BrowserStore::new(AuthSnapshot::default(), tx);
        let writer_store = store.clone();
        let writer = std::thread::spawn(move || {
            writer_store.set_account(Some(Account {
                did: "did:plc:test".into(),
                handle: "alice.test".into(),
            }))
        });

        let request = rx.recv().unwrap();
        request.ack.send(Err("quota exceeded".into())).unwrap();
        assert!(writer.join().unwrap().is_err());
        assert_eq!(store.account().unwrap(), None);
    }
}
