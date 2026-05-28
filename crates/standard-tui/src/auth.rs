//! OAuth sign-in (atrium-oauth, DPoP, browser loopback) + subscription writes.
//!
//! All of atrium's async/tokio surface is confined to this module and the worker's runtime;
//! `standard-core` stays synchronous. atproto requires auth only for *identity* (sign-in) and
//! *writes* (subscription create/delete) — public reads keep using the blocking `Transport`.
//!
//! Two deliberate leanness choices:
//! - A custom rustls [`HttpClient`] (`RustlsHttpClient`) instead of atrium's `default-client`,
//!   which pulls `reqwest/default-tls` (openssl, a C dep). We reuse reqwest's async side.
//! - The handle→DID step uses atrium's HTTPS well-known resolver (a stub DNS resolver forces
//!   the fallback), so we don't pull a DNS stack. This matches `read::resolve_did`'s own
//!   well-known-only behaviour; a DID typed directly skips resolution entirely.
//!
//! The OAuth session (DPoP key + tokens) is persisted to a `0600` JSON file under the config
//! dir via [`FileSessionStore`]; a tiny `account.json` sidecar remembers the signed-in
//! did/handle so startup can present "@handle" without a network round-trip.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use atrium_api::agent::{Agent, SessionManager};
use atrium_api::com::atproto::repo::{create_record, delete_record};
use atrium_api::types::TryIntoUnknown;
use atrium_api::types::string::{AtIdentifier, Datetime, Did, Nsid, RecordKey};
use atrium_common::store::Store;
use atrium_identity::did::{CommonDidResolver, CommonDidResolverConfig, DEFAULT_PLC_DIRECTORY_URL};
use atrium_identity::handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig, DnsTxtResolver};
use atrium_oauth::store::session::{Session, SessionStore};
use atrium_oauth::store::state::MemoryStateStore;
use atrium_oauth::{
    AtprotoClientMetadata, AtprotoLocalhostClientMetadata, AuthMethod, AuthorizeOptions,
    CallbackParams, GrantType, KnownScope, OAuthClient, OAuthClientConfig, OAuthResolverConfig,
    Scope,
};
use atrium_xrpc::HttpClient;
use atrium_xrpc::http;
use serde::{Deserialize, Serialize};

/// Any failure in the auth path. Boxed so every atrium / reqwest / io error converts with `?`.
pub type AuthError = Box<dyn Error + Send + Sync + 'static>;
type AuthResult<T> = Result<T, AuthError>;

/// A concrete error for the session store. `atrium`'s `Store::Error` bound requires a type that
/// implements `Error` (a boxed trait object doesn't), so the file store can't use [`AuthError`].
#[derive(Debug)]
pub struct SessionStoreError(String);

impl std::fmt::Display for SessionStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session store: {}", self.0)
    }
}

impl Error for SessionStoreError {}

impl From<std::io::Error> for SessionStoreError {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}

impl From<serde_json::Error> for SessionStoreError {
    fn from(e: serde_json::Error) -> Self {
        Self(e.to_string())
    }
}

/// The loopback redirect the localhost OAuth client registers. Must match the port we bind.
const CALLBACK_PORT: u16 = 4599;
const REDIRECT_URI: &str = "http://127.0.0.1:4599/callback";
/// How long to wait for the browser redirect before giving up (the worker is blocked meanwhile).
const CALLBACK_TIMEOUT_SECS: u64 = 300;
/// The lexicon collection the reader mirrors its follow-list into.
const SUBSCRIPTION_NSID: &str = "site.standard.graph.subscription";

/// The hosted OAuth `client_id` — a URL serving this app's `client_metadata.json`. The
/// authorization server fetches it, so it must resolve directly (the apex `davidlewis.xyz`
/// 301-redirects to `www`, so we use `www`). The matching document lives in the website repo
/// at `standard-reader/client_metadata.json`. Used by default; set `SR_OAUTH_LOCALHOST` to fall
/// back to the no-hosting dev client.
const HOSTED_CLIENT_ID: &str = "https://www.davidlewis.xyz/standard-reader/client_metadata.json";
const CLIENT_URI: &str = "https://www.davidlewis.xyz";

/// The signed-in identity, persisted in `account.json` so startup needn't hit the network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub did: String,
    pub handle: String,
}

// --- The concrete OAuth client ---------------------------------------------------

/// The fully-monomorphised client type: our rustls HTTP, file-backed sessions, in-memory
/// transient state, and the no-DNS identity resolvers.
type Client = OAuthClient<
    MemoryStateStore,
    FileSessionStore,
    CommonDidResolver<RustlsHttpClient>,
    AtprotoHandleResolver<DohDnsTxtResolver, RustlsHttpClient>,
    RustlsHttpClient,
>;

/// Owns the OAuth client and the on-disk paths. Async methods are driven by the worker's
/// tokio runtime; nothing here is called off that single worker thread.
pub struct Auth {
    client: Client,
    session_path: PathBuf,
    account_path: PathBuf,
}

impl Auth {
    /// Build the client and resolve the session/account file paths under `config_dir`.
    pub fn new(config_dir: &Path) -> AuthResult<Self> {
        let session_path = config_dir.join("session.json");
        let account_path = config_dir.join("account.json");
        let client = build_client(session_path.clone())?;
        Ok(Self {
            client,
            session_path,
            account_path,
        })
    }

    /// The persisted signed-in account, if any — a pure file read (no network, no validation).
    pub fn current_account(&self) -> Option<Account> {
        let bytes = fs::read(&self.account_path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Run the full browser OAuth flow: PAR/authorize → open the browser → wait on the
    /// loopback callback → exchange the code → persist the session. Returns the new account.
    ///
    /// `progress` is called at each step (binding, authorize, browser, waiting, exchange) so
    /// the frontend can report where the flow is — and surface the authorize URL, in case the
    /// browser doesn't open on its own (e.g. a headless/Crostini box).
    pub async fn login(&self, ident: &str, progress: impl Fn(String)) -> AuthResult<Account> {
        // Reserve the port before kicking off the browser so there's no redirect race. Bind to
        // all interfaces, not just loopback: the registered redirect is `127.0.0.1`, but on
        // ChromeOS/Crostini the browser runs outside the container and the forwarded callback
        // arrives on the container's network interface — a `127.0.0.1`-only listener wouldn't
        // accept it. (`0.0.0.0` still accepts the plain-loopback case on a normal desktop.)
        progress(format!(
            "binding callback server on 0.0.0.0:{CALLBACK_PORT}…"
        ));
        let listener = TcpListener::bind(("0.0.0.0", CALLBACK_PORT))?;

        progress(format!(
            "requesting authorization for {ident} (resolving identity + PAR)…"
        ));
        let url = self
            .client
            .authorize(
                ident,
                AuthorizeOptions {
                    scopes: vec![
                        Scope::Known(KnownScope::Atproto),
                        Scope::Known(KnownScope::TransitionGeneric),
                    ],
                    ..Default::default()
                },
            )
            .await?;

        progress(format!(
            "authorize in your browser — if it didn't open, visit: {url}"
        ));
        // Detached so we don't block the runtime waiting on the opener process (some desktops,
        // notably Crostini, don't return promptly). If it fails, the URL was already surfaced.
        match open::that_detached(&url) {
            Ok(_) => progress(format!(
                "browser launched; waiting for the redirect to :{CALLBACK_PORT}…"
            )),
            Err(e) => progress(format!(
                "couldn't launch a browser ({e}); open the URL above manually…"
            )),
        }

        // Blocking accept on a worker-pool thread so it doesn't stall the runtime.
        let params = tokio::task::spawn_blocking(move || wait_for_callback(listener)).await??;
        progress("callback received; exchanging code for tokens…".into());
        let (session, _) = self.client.callback(params).await?;

        let did = session.did().await.ok_or("the OAuth session had no DID")?;
        let did = did.as_ref().to_string();
        // Display name: the handle the user typed (else the DID, when signed in by DID).
        let handle = if ident.starts_with("did:") {
            did.clone()
        } else {
            ident.trim().trim_start_matches('@').to_string()
        };
        let account = Account { did, handle };
        self.write_account(&account)?;
        Ok(account)
    }

    /// Validate the persisted session (atrium refreshes tokens if needed). `Ok(None)` means there
    /// is no stored session at all; `Ok(Some)` means it restored.
    ///
    /// A failed restore is **propagated, not swallowed** — and the session files are left intact.
    /// At this layer a transient network error (PDS/plc unreachable) is indistinguishable from a
    /// genuinely revoked token, and the old behaviour (delete the files on any error) silently
    /// signed the user out over a momentary blip. Keeping the files lets the next launch retry;
    /// [`Self::login`] overwrites and [`Self::logout`] clears when sign-out is actually intended.
    pub async fn restore(&self) -> AuthResult<Option<Account>> {
        let Some(account) = self.current_account() else {
            return Ok(None);
        };
        let did = Did::new(account.did.clone())?;
        self.client.restore(&did).await?;
        Ok(Some(account))
    }

    /// Revoke the session upstream (best-effort) and remove the local session/account files.
    pub async fn logout(&self) -> AuthResult<()> {
        if let Some(account) = self.current_account()
            && let Ok(did) = Did::new(account.did)
        {
            let _ = self.client.revoke(&did).await;
        }
        self.clear_files()
    }

    /// Create a `site.standard.graph.subscription` record in the user's repo; returns its rkey.
    pub async fn create_subscription(
        &self,
        did: &str,
        publication_uri: &str,
    ) -> AuthResult<String> {
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

    /// Delete the subscription record identified by `rkey` from the user's repo.
    pub async fn delete_subscription(&self, did: &str, rkey: &str) -> AuthResult<()> {
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

    fn write_account(&self, account: &Account) -> AuthResult<()> {
        if let Some(dir) = self.account_path.parent() {
            fs::create_dir_all(dir)?;
        }
        write_private(&self.account_path, &serde_json::to_vec_pretty(account)?)?;
        Ok(())
    }

    fn clear_files(&self) -> AuthResult<()> {
        for path in [&self.session_path, &self.account_path] {
            if let Err(e) = fs::remove_file(path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                return Err(e.into());
            }
        }
        Ok(())
    }
}

/// The subscription record body we write. `$type` + the publication AT-URI + a timestamp.
#[derive(Serialize)]
struct SubscriptionRecord<'a> {
    #[serde(rename = "$type")]
    r#type: &'a str,
    publication: &'a str,
    #[serde(rename = "createdAt")]
    created_at: String,
}

fn build_client(session_path: PathBuf) -> AuthResult<Client> {
    let http = RustlsHttpClient::new()?;
    let dns = DohDnsTxtResolver {
        client: http.client.clone(),
    };
    let resolver_http = Arc::new(http.clone());
    let resolver = OAuthResolverConfig {
        did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
            plc_directory_url: DEFAULT_PLC_DIRECTORY_URL.to_string(),
            http_client: Arc::clone(&resolver_http),
        }),
        handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
            dns_txt_resolver: dns,
            http_client: Arc::clone(&resolver_http),
        }),
        authorization_server_metadata: Default::default(),
        protected_resource_metadata: Default::default(),
    };
    let state_store = MemoryStateStore::default();
    let session_store = FileSessionStore { path: session_path };
    let scopes = vec![
        Scope::Known(KnownScope::Atproto),
        Scope::Known(KnownScope::TransitionGeneric),
    ];

    // Default to the hosted client (a real client_id + branded consent screen). The dev client
    // needs no hosting but shows a generic prompt — kept as `SR_OAUTH_LOCALHOST` for local work
    // or before the hosted metadata is deployed. `M` only flows into `OAuthClient::new`, so both
    // arms build the same `Client` type.
    if std::env::var_os("SR_OAUTH_LOCALHOST").is_some() {
        Ok(OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoLocalhostClientMetadata {
                redirect_uris: Some(vec![REDIRECT_URI.to_string()]),
                scopes: Some(scopes),
            },
            keys: None,
            resolver,
            state_store,
            session_store,
            http_client: http,
        })?)
    } else {
        Ok(OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoClientMetadata {
                client_id: HOSTED_CLIENT_ID.to_string(),
                client_uri: Some(CLIENT_URI.to_string()),
                redirect_uris: vec![REDIRECT_URI.to_string()],
                token_endpoint_auth_method: AuthMethod::None,
                grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                scopes,
                jwks_uri: None,
                token_endpoint_auth_signing_alg: None,
            },
            keys: None,
            resolver,
            state_store,
            session_store,
            http_client: http,
        })?)
    }
}

/// `at://<did>/<collection>/<rkey>` → `<rkey>` (the trailing path segment).
fn rkey_from_uri(uri: &str) -> String {
    uri.rsplit('/').next().unwrap_or_default().to_string()
}

// --- rustls HTTP client (atrium_xrpc::HttpClient over async reqwest) -------------

/// An [`HttpClient`] backed by reqwest's async client. With only `reqwest/rustls-tls` enabled
/// (no `default-tls`), this is pure-Rust TLS — no openssl. Cheap to `clone` (reqwest pools
/// internally), so the resolvers and the client can share one connection pool.
#[derive(Clone)]
struct RustlsHttpClient {
    client: reqwest::Client,
}

impl RustlsHttpClient {
    fn new() -> AuthResult<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("standard-reader/", env!("CARGO_PKG_VERSION")))
            // Bound auth/identity requests so a hung host can't stall sign-in or restore forever.
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { client })
    }
}

impl HttpClient for RustlsHttpClient {
    async fn send_http(
        &self,
        request: http::Request<Vec<u8>>,
    ) -> Result<http::Response<Vec<u8>>, Box<dyn Error + Send + Sync + 'static>> {
        let response = self.client.execute(request.try_into()?).await?;
        let mut builder = http::Response::builder().status(response.status());
        for (k, v) in response.headers() {
            builder = builder.header(k, v);
        }
        builder
            .body(response.bytes().await?.to_vec())
            .map_err(Into::into)
    }
}

/// Resolves `_atproto.<handle>` TXT records over **DNS-over-HTTPS** (Google's JSON endpoint),
/// so handle sign-in works for DNS-based handles (e.g. `pfrazee.com`) without pulling a DNS
/// stack — just an HTTPS GET on the same rustls reqwest client. Returns the raw TXT strings;
/// `AtprotoHandleResolver` picks out the `did=…` one (and falls back to HTTPS well-known if we
/// return nothing). Mirrors the core's `read::resolve_did` DoH fallback.
#[derive(Clone)]
struct DohDnsTxtResolver {
    client: reqwest::Client,
}

impl DnsTxtResolver for DohDnsTxtResolver {
    async fn resolve(
        &self,
        query: &str,
    ) -> Result<Vec<String>, Box<dyn Error + Send + Sync + 'static>> {
        let url = format!("https://dns.google/resolve?name={query}&type=TXT");
        let bytes = self.client.get(&url).send().await?.bytes().await?;
        let doc: serde_json::Value = serde_json::from_slice(&bytes)?;
        // `{ "Answer": [ { "data": "did=did:plc:…" }, … ] }` (data is sometimes quote-wrapped).
        let records = doc
            .get("Answer")
            .and_then(|a| a.as_array())
            .map(|answers| {
                answers
                    .iter()
                    .filter_map(|a| a.get("data").and_then(|d| d.as_str()))
                    .map(|s| s.trim_matches('"').to_string())
                    .collect()
            })
            .unwrap_or_default();
        Ok(records)
    }
}

// --- File-backed OAuth session store (0600) --------------------------------------

/// Persists the OAuth [`Session`] (DPoP key + tokens) to a `0600` JSON file keyed by DID.
/// Only one account is signed in at a time, but the on-disk shape is a DID→Session map to
/// match atrium's keying. No keyring (Crostini has no Secret Service daemon).
#[derive(Clone)]
struct FileSessionStore {
    path: PathBuf,
}

impl FileSessionStore {
    fn read_map(&self) -> Result<BTreeMap<String, Session>, SessionStoreError> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn write_map(&self, map: &BTreeMap<String, Session>) -> Result<(), SessionStoreError> {
        if map.is_empty() {
            // No sessions left → don't leave an empty file holding key material.
            if let Err(e) = fs::remove_file(&self.path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                return Err(e.into());
            }
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        write_private(&self.path, &serde_json::to_vec(map)?)?;
        Ok(())
    }
}

impl Store<Did, Session> for FileSessionStore {
    type Error = SessionStoreError;

    async fn get(&self, key: &Did) -> Result<Option<Session>, Self::Error> {
        Ok(self.read_map()?.get(key.as_ref()).cloned())
    }

    async fn set(&self, key: Did, value: Session) -> Result<(), Self::Error> {
        let mut map = self.read_map()?;
        map.insert(key.as_ref().to_string(), value);
        self.write_map(&map)
    }

    async fn del(&self, key: &Did) -> Result<(), Self::Error> {
        let mut map = self.read_map()?;
        map.remove(key.as_ref());
        self.write_map(&map)
    }

    async fn clear(&self) -> Result<(), Self::Error> {
        self.write_map(&BTreeMap::new())
    }
}

impl SessionStore for FileSessionStore {}

/// Write `bytes` to `path` with `0600` permissions (owner read/write only).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

// --- Loopback callback server ----------------------------------------------------

/// A small landing page shown in the browser after the redirect.
const CALLBACK_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>standard-reader</title></head>\
<body style=\"font-family:system-ui;text-align:center;padding-top:4rem;color:#222\">\
<h2>You're logged in to standard-reader.</h2>\
<p>You can close this tab and return to the terminal.</p></body></html>";

/// Accept a single connection on the bound loopback listener (polling until the deadline),
/// parse the OAuth redirect, reply with a landing page, and return the callback params.
fn wait_for_callback(listener: TcpListener) -> AuthResult<CallbackParams> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(CALLBACK_TIMEOUT_SECS);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return handle_callback_conn(stream),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("timed out waiting for the browser redirect".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn handle_callback_conn(mut stream: TcpStream) -> AuthResult<CallbackParams> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf)?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let params = parse_callback_request(&request)?;

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        CALLBACK_HTML.len(),
        CALLBACK_HTML
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    Ok(params)
}

/// Parse the `code`/`state`/`iss` from a raw HTTP request (the redirect `GET` line).
fn parse_callback_request(raw: &str) -> AuthResult<CallbackParams> {
    // First line: "GET /callback?code=...&state=...&iss=... HTTP/1.1"
    let target = raw
        .split_whitespace()
        .nth(1)
        .ok_or("malformed HTTP request line")?;
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");

    let (mut code, mut state, mut iss) = (None, None, None);
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let value = percent_decode(value);
        match key {
            "code" => code = Some(value),
            "state" => state = Some(value),
            "iss" => iss = Some(value),
            _ => {}
        }
    }
    Ok(CallbackParams {
        code: code.ok_or("callback redirect missing `code`")?,
        state,
        iss,
    })
}

/// Minimal `application/x-www-form-urlencoded` decode: `%XX` escapes and `+` → space.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_code_state_iss_from_get_line() {
        let raw = "GET /callback?code=abc123&state=xyz789&iss=https%3A%2F%2Fbsky.social HTTP/1.1\r\n\
                   Host: 127.0.0.1:4599\r\n\r\n";
        let params = parse_callback_request(raw).unwrap();
        assert_eq!(params.code, "abc123");
        assert_eq!(params.state.as_deref(), Some("xyz789"));
        assert_eq!(params.iss.as_deref(), Some("https://bsky.social"));
    }

    #[test]
    fn callback_without_code_is_an_error() {
        let raw = "GET /callback?state=only HTTP/1.1\r\n\r\n";
        assert!(parse_callback_request(raw).is_err());
    }

    #[test]
    fn callback_query_order_and_extras_are_tolerated() {
        let raw = "GET /callback?iss=x&extra=1&code=c&state=s HTTP/1.1\r\n";
        let params = parse_callback_request(raw).unwrap();
        assert_eq!(params.code, "c");
        assert_eq!(params.state.as_deref(), Some("s"));
        assert_eq!(params.iss.as_deref(), Some("x"));
    }

    #[test]
    fn percent_decode_handles_escapes_plus_and_trailing_percent() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("hello+world"), "hello world");
        assert_eq!(percent_decode("100%"), "100%"); // dangling % left verbatim
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn rkey_is_the_last_uri_segment() {
        assert_eq!(
            rkey_from_uri("at://did:plc:abc/site.standard.graph.subscription/3kxyz"),
            "3kxyz"
        );
    }
}
