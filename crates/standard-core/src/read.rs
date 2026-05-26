//! The read pipeline: live records → decoded documents.
//!
//! This is the orchestration layer — identity resolution, `listRecords`/`getRecord`
//! fetches, response parsing, and decode dispatch — written as free functions generic
//! over [`atp::Transport`]. It stays in the **core** (synchronous, no I/O of its own:
//! the `Transport` moves the bytes) so a future Vita frontend reuses every step.
//!
//! It does NOT touch [`store::Store`]: the pipeline returns model types and lets the
//! caller decide what to cache.
//!
//! [`store::Store`]: crate::store::Store

use std::error::Error;
use std::fmt;

use serde_json::Value;

use crate::atp::{AtUri, Transport, xrpc};
use crate::decode::image::blob_image;
use crate::decode::{DecodeCtx, Registry, content_ref};
use crate::model::{Document, Publication, RichDoc, Subscription};

/// A resolved repo: its DID and the PDS that hosts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub did: String,
    pub pds: String,
}

/// Anything that can go wrong while reading.
#[derive(Debug)]
pub enum ReadError {
    /// The frontend's `Transport` failed (network, TLS, HTTP status).
    Transport(Box<dyn Error + Send + Sync>),
    /// Identity resolution failed (bad handle, missing PDS service, unknown DID method).
    Resolve(String),
    /// A response wasn't the JSON shape we expected.
    Parse(String),
    /// An AT-URI didn't parse.
    BadUri(String),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Transport(e) => write!(f, "transport error: {e}"),
            ReadError::Resolve(m) => write!(f, "resolve error: {m}"),
            ReadError::Parse(m) => write!(f, "parse error: {m}"),
            ReadError::BadUri(u) => write!(f, "bad AT-URI: {u}"),
        }
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ReadError::Transport(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

/// A record as it comes back from `getRecord` / inside `listRecords`.
struct Record {
    uri: String,
    value: Value,
}

// --- HTTP helper -----------------------------------------------------------------

/// GET `url` through the transport, boxing its error into [`ReadError::Transport`].
fn get<T: Transport>(t: &T, url: &str) -> Result<Vec<u8>, ReadError> {
    t.get(url).map_err(|e| ReadError::Transport(Box::new(e)))
}

fn parse_json(bytes: &[u8]) -> Result<Value, ReadError> {
    serde_json::from_slice(bytes).map_err(|e| ReadError::Parse(e.to_string()))
}

// --- Response parsers (pure) -----------------------------------------------------

/// Parse a `getRecord` envelope: `{ uri, cid, value }`.
fn parse_get(bytes: &[u8]) -> Result<Record, ReadError> {
    let v = parse_json(bytes)?;
    let uri = v
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| ReadError::Parse("getRecord: missing uri".into()))?
        .to_string();
    let value = v
        .get("value")
        .cloned()
        .ok_or_else(|| ReadError::Parse("getRecord: missing value".into()))?;
    Ok(Record { uri, value })
}

/// Parse a `listRecords` envelope: `{ records: [{uri, value}], cursor? }`.
fn parse_list(bytes: &[u8]) -> Result<(Vec<Record>, Option<String>), ReadError> {
    let v = parse_json(bytes)?;
    let records = v
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| ReadError::Parse("listRecords: missing records".into()))?
        .iter()
        .filter_map(|r| {
            Some(Record {
                uri: r.get("uri")?.as_str()?.to_string(),
                value: r.get("value")?.clone(),
            })
        })
        .collect();
    let cursor = v.get("cursor").and_then(Value::as_str).map(str::to_string);
    Ok((records, cursor))
}

/// `at://<did>/...` → the owning DID.
fn did_of(uri: &str) -> Option<&str> {
    uri.strip_prefix("at://")?.split('/').next()
}

fn parse_document(value: &Value, uri: &str) -> Option<Document> {
    let did = did_of(uri)?;
    Some(Document {
        uri: uri.to_string(),
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        // `site` is the AT-URI of the owning publication — the defining link.
        publication: value.get("site").and_then(Value::as_str)?.to_string(),
        published_at: value
            .get("publishedAt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        updated_at: value
            .get("updatedAt")
            .and_then(Value::as_str)
            .map(str::to_string),
        cover_image: value.get("coverImage").and_then(|b| blob_image(b, did, "")),
        text_content: value
            .get("textContent")
            .and_then(Value::as_str)
            .map(str::to_string),
        tags: value
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        path: value
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn parse_publication(value: &Value, uri: &str) -> Option<Publication> {
    let did = did_of(uri)?;
    Some(Publication {
        uri: uri.to_string(),
        url: value
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        icon: value.get("icon").and_then(|b| blob_image(b, did, "")),
    })
}

fn parse_subscription(value: &Value, uri: &str) -> Option<Subscription> {
    Some(Subscription {
        uri: uri.to_string(),
        publication: value
            .get("publication")
            .and_then(Value::as_str)?
            .to_string(),
    })
}

// --- Identity resolution ---------------------------------------------------------

/// Resolve a handle to a DID via the HTTPS well-known method
/// (`https://<handle>/.well-known/atproto-did`). DNS-TXT fallback is deferred.
pub fn resolve_did<T: Transport>(t: &T, handle: &str) -> Result<String, ReadError> {
    let bytes = get(t, &format!("https://{handle}/.well-known/atproto-did"))?;
    let did = String::from_utf8_lossy(&bytes).trim().to_string();
    if did.starts_with("did:") {
        Ok(did)
    } else {
        Err(ReadError::Resolve(format!(
            "no atproto-did for handle {handle}"
        )))
    }
}

/// Resolve a DID to its PDS endpoint (`did:plc` via plc.directory, `did:web` via its
/// `did.json`).
pub fn resolve_pds<T: Transport>(t: &T, did: &str) -> Result<String, ReadError> {
    let doc = if did.starts_with("did:plc:") {
        parse_json(&get(t, &xrpc::plc_directory(did))?)?
    } else if let Some(rest) = did.strip_prefix("did:web:") {
        // did:web:host(:path) → https://host/(path/)did.json
        let path = rest.replace(':', "/");
        parse_json(&get(t, &format!("https://{path}/.well-known/did.json"))?)?
    } else {
        return Err(ReadError::Resolve(format!("unsupported DID method: {did}")));
    };

    doc.get("service")
        .and_then(Value::as_array)
        .and_then(|services| {
            services.iter().find(|s| {
                s.get("type").and_then(Value::as_str) == Some("AtprotoPersonalDataServer")
                    || s.get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id.ends_with("#atproto_pds"))
            })
        })
        .and_then(|s| s.get("serviceEndpoint").and_then(Value::as_str))
        .map(|e| e.trim_end_matches('/').to_string())
        .ok_or_else(|| ReadError::Resolve(format!("no PDS service for {did}")))
}

/// Resolve a handle *or* DID to its [`Identity`]. A `did:`-prefixed input skips the
/// handle step (handy when a handle only resolves via DNS).
pub fn resolve_identity<T: Transport>(t: &T, handle_or_did: &str) -> Result<Identity, ReadError> {
    let did = if handle_or_did.starts_with("did:") {
        handle_or_did.to_string()
    } else {
        resolve_did(t, handle_or_did)?
    };
    let pds = resolve_pds(t, &did)?;
    Ok(Identity { did, pds })
}

// --- Orchestration ---------------------------------------------------------------

/// A reader's own subscriptions (each points at a publication AT-URI, often cross-repo).
pub fn list_subscriptions<T: Transport>(
    t: &T,
    reader: &Identity,
) -> Result<Vec<Subscription>, ReadError> {
    let url = xrpc::list_records(
        &reader.pds,
        &reader.did,
        "site.standard.graph.subscription",
        100,
        None,
    );
    let (records, _) = parse_list(&get(t, &url)?)?;
    Ok(records
        .iter()
        .filter_map(|r| parse_subscription(&r.value, &r.uri))
        .collect())
}

/// The publications a repo publishes itself (the fallback when it has no subscriptions).
pub fn list_publications<T: Transport>(
    t: &T,
    repo: &Identity,
) -> Result<Vec<Publication>, ReadError> {
    let url = xrpc::list_records(&repo.pds, &repo.did, "site.standard.publication", 100, None);
    let (records, _) = parse_list(&get(t, &url)?)?;
    Ok(records
        .iter()
        .filter_map(|r| parse_publication(&r.value, &r.uri))
        .collect())
}

/// Fetch a publication by AT-URI, resolving its repo (it may live in another DID).
/// Returns the publication and the [`Identity`] of its repo, for listing its documents.
pub fn get_publication<T: Transport>(
    t: &T,
    pub_uri: &AtUri,
) -> Result<(Publication, Identity), ReadError> {
    let pds = resolve_pds(t, &pub_uri.did)?;
    let url = xrpc::get_record(&pds, &pub_uri.did, &pub_uri.collection, &pub_uri.rkey);
    let record = parse_get(&get(t, &url)?)?;
    let publication = parse_publication(&record.value, &record.uri)
        .ok_or_else(|| ReadError::Parse(format!("not a publication: {pub_uri}")))?;
    let identity = Identity {
        did: pub_uri.did.clone(),
        pds,
    };
    Ok((publication, identity))
}

/// A page of a publication's documents (metadata only), newest-first per the PDS, plus
/// the cursor for the next page.
pub fn list_documents<T: Transport>(
    t: &T,
    repo: &Identity,
    cursor: Option<&str>,
) -> Result<(Vec<Document>, Option<String>), ReadError> {
    let url = xrpc::list_records(&repo.pds, &repo.did, "site.standard.document", 50, cursor);
    let (records, next) = parse_list(&get(t, &url)?)?;
    let docs = records
        .iter()
        .filter_map(|r| parse_document(&r.value, &r.uri))
        .collect();
    Ok((docs, next))
}

/// Fetch one document and decode its body. Honors the `#contentRef` seam: when the
/// `content` is a reference (GreenGale), the referenced record is fetched and *its*
/// content decoded.
pub fn get_document<T: Transport>(
    t: &T,
    registry: &Registry,
    doc_uri: &AtUri,
    pds: &str,
) -> Result<(Document, RichDoc), ReadError> {
    let url = xrpc::get_record(pds, &doc_uri.did, &doc_uri.collection, &doc_uri.rkey);
    let record = parse_get(&get(t, &url)?)?;
    let meta = parse_document(&record.value, &doc_uri.to_string())
        .ok_or_else(|| ReadError::Parse(format!("not a document: {doc_uri}")))?;

    let ctx = DecodeCtx {
        repo_did: &doc_uri.did,
    };
    let content = record.value.get("content");

    let body = match content.and_then(content_ref) {
        // Two-phase: the content points at another record; fetch and decode that.
        Some(ref_uri) => {
            let ref_pds = if ref_uri.did == doc_uri.did {
                pds.to_string()
            } else {
                resolve_pds(t, &ref_uri.did)?
            };
            let ref_url =
                xrpc::get_record(&ref_pds, &ref_uri.did, &ref_uri.collection, &ref_uri.rkey);
            let referenced = parse_get(&get(t, &ref_url)?)?;
            registry.decode(
                referenced.value.get("content"),
                referenced.value.get("textContent").and_then(Value::as_str),
                &ctx,
            )
        }
        None => registry.decode(
            content,
            record.value.get("textContent").and_then(Value::as_str),
            &ctx,
        ),
    };
    Ok((meta, body))
}

/// Fetch an image/asset blob by CID.
pub fn get_blob<T: Transport>(
    t: &T,
    pds: &str,
    did: &str,
    cid: &str,
) -> Result<Vec<u8>, ReadError> {
    get(t, &xrpc::get_blob(pds, did, cid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_subscription_to_cross_repo_publication() {
        let value = serde_json::json!({
            "$type": "site.standard.graph.subscription",
            "publication": "at://did:plc:other/site.standard.publication/3abc"
        });
        let sub = parse_subscription(
            &value,
            "at://did:plc:me/site.standard.graph.subscription/3xyz",
        )
        .unwrap();
        assert_eq!(
            sub.publication,
            "at://did:plc:other/site.standard.publication/3abc"
        );
    }

    #[test]
    fn document_requires_site_and_resolves_cover_blob() {
        let uri = "at://did:plc:abc/site.standard.document/3doc";
        let value = serde_json::json!({
            "title": "Hello",
            "site": "at://did:plc:abc/site.standard.publication/3pub",
            "publishedAt": "2026-05-26T00:00:00Z",
            "textContent": "body",
            "coverImage": { "$type": "blob", "ref": { "$link": "bafcover" }, "mimeType": "image/png", "size": 1 }
        });
        let doc = parse_document(&value, uri).unwrap();
        assert_eq!(
            doc.publication,
            "at://did:plc:abc/site.standard.publication/3pub"
        );
        assert!(matches!(
            doc.cover_image.unwrap().source,
            crate::model::ImageSource::Blob { ref did, ref cid } if did == "did:plc:abc" && cid == "bafcover"
        ));

        // No `site` → not a valid standard document.
        let bad = serde_json::json!({ "title": "x" });
        assert!(parse_document(&bad, uri).is_none());
    }

    #[test]
    fn list_envelope_keeps_cursor() {
        let bytes = br#"{"records":[{"uri":"at://d/c/1","value":{"publication":"at://x/site.standard.publication/1"}}],"cursor":"next123"}"#;
        let (records, cursor) = parse_list(bytes).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(cursor.as_deref(), Some("next123"));
    }
}
