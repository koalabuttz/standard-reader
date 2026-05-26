//! AT Protocol plumbing — *without* a network stack.
//!
//! The core builds request URLs and parses responses; the frontend's [`Transport`]
//! actually moves the bytes (and attaches auth). This is what keeps `reqwest`/`tokio`
//! out of the core and a Vita port in reach.

use std::fmt;

/// A parsed `at://<did>/<collection>/<rkey>` URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtUri {
    pub did: String,
    pub collection: String,
    pub rkey: String,
}

impl AtUri {
    /// Parse an AT-URI. Returns `None` if it isn't a well-formed record URI.
    pub fn parse(s: &str) -> Option<Self> {
        let rest = s.strip_prefix("at://")?;
        let mut parts = rest.splitn(3, '/');
        let did = parts.next()?;
        let collection = parts.next()?;
        let rkey = parts.next()?;
        if did.is_empty() || collection.is_empty() || rkey.is_empty() {
            return None;
        }
        Some(Self {
            did: did.to_string(),
            collection: collection.to_string(),
            rkey: rkey.to_string(),
        })
    }
}

impl fmt::Display for AtUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at://{}/{}/{}", self.did, self.collection, self.rkey)
    }
}

/// Whatever a frontend needs to perform an XRPC request, abstracted to bytes.
///
/// Synchronous on purpose: the desktop frontend runs this on a worker thread; a
/// Vita frontend can call it inline. Implementations attach auth (DPoP / bearer).
pub trait Transport {
    type Error: std::error::Error + Send + Sync + 'static;

    fn get(&self, url: &str) -> Result<Vec<u8>, Self::Error>;
    fn post(&self, url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>, Self::Error>;
}

/// URL builders for the XRPC methods this reader uses. Query values are simple
/// (DIDs, NSIDs, CIDs, rkeys) and need no percent-encoding beyond `:` in DIDs,
/// which servers accept literally; callers pass already-safe identifiers.
pub mod xrpc {
    /// `com.atproto.identity.resolveHandle` against an entryway/PDS.
    pub fn resolve_handle(service: &str, handle: &str) -> String {
        format!("{service}/xrpc/com.atproto.identity.resolveHandle?handle={handle}")
    }

    /// The PLC directory entry for a DID (to discover its PDS endpoint).
    pub fn plc_directory(did: &str) -> String {
        format!("https://plc.directory/{did}")
    }

    /// `com.atproto.repo.listRecords`.
    pub fn list_records(pds: &str, repo: &str, collection: &str, limit: u32, cursor: Option<&str>) -> String {
        let mut url = format!(
            "{pds}/xrpc/com.atproto.repo.listRecords?repo={repo}&collection={collection}&limit={limit}"
        );
        if let Some(c) = cursor {
            url.push_str("&cursor=");
            url.push_str(c);
        }
        url
    }

    /// `com.atproto.repo.getRecord`.
    pub fn get_record(pds: &str, repo: &str, collection: &str, rkey: &str) -> String {
        format!("{pds}/xrpc/com.atproto.repo.getRecord?repo={repo}&collection={collection}&rkey={rkey}")
    }

    /// `com.atproto.sync.getBlob` — fetch an image/asset blob by CID.
    pub fn get_blob(pds: &str, did: &str, cid: &str) -> String {
        format!("{pds}/xrpc/com.atproto.sync.getBlob?did={did}&cid={cid}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_record_uri() {
        let u = AtUri::parse("at://did:plc:xn3l7ogsxym5ixxugidum5dw/site.standard.publication/3mmnuz5454lm7").unwrap();
        assert_eq!(u.collection, "site.standard.publication");
        assert_eq!(u.rkey, "3mmnuz5454lm7");
        assert_eq!(u.to_string(), "at://did:plc:xn3l7ogsxym5ixxugidum5dw/site.standard.publication/3mmnuz5454lm7");
    }

    #[test]
    fn rejects_garbage() {
        assert!(AtUri::parse("https://example.com").is_none());
        assert!(AtUri::parse("at://did:plc:abc/onlytwo").is_none());
    }

    #[test]
    fn builds_blob_url() {
        let url = xrpc::get_blob("https://yapfest.club", "did:plc:abc", "bafkreixyz");
        assert_eq!(url, "https://yapfest.club/xrpc/com.atproto.sync.getBlob?did=did:plc:abc&cid=bafkreixyz");
    }
}
