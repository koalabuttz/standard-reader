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
use crate::decode::{DecodeCtx, Registry, content_ref, external_content_cid, gallery_images};
use crate::model::{Block, Document, Image, Inline, Publication, RichDoc, Subscription};

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
        publishing_platform: None,
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

/// Resolve a handle to a DID. Tries both atproto methods: the HTTPS well-known file
/// (`https://<handle>/.well-known/atproto-did`), then the `_atproto.<handle>` DNS TXT record
/// via DNS-over-HTTPS. Handles on a custom domain commonly use only one or the other (e.g.
/// `pfrazee.com` serves no well-known file — that path redirects — and publishes via DNS).
pub fn resolve_did<T: Transport>(t: &T, handle: &str) -> Result<String, ReadError> {
    // 1. HTTPS well-known. A non-2xx (or a redirect to a non-DID page) just falls through.
    if let Ok(bytes) = get(t, &format!("https://{handle}/.well-known/atproto-did")) {
        let did = String::from_utf8_lossy(&bytes).trim().to_string();
        if did.starts_with("did:") {
            return Ok(did);
        }
    }
    // 2. DNS TXT, queried over HTTPS so the core needs no DNS stack (stays pure-HTTP + sync).
    if let Some(did) = resolve_did_via_dns(t, handle)? {
        return Ok(did);
    }
    Err(ReadError::Resolve(format!(
        "could not resolve handle {handle} (no well-known file and no _atproto DNS record)"
    )))
}

/// Look up the `_atproto.<handle>` TXT record through Google's DNS-over-HTTPS JSON endpoint and
/// extract the `did=` value. Returns `None` if the query fails or no `did=` record is present.
fn resolve_did_via_dns<T: Transport>(t: &T, handle: &str) -> Result<Option<String>, ReadError> {
    let url = format!("https://dns.google/resolve?name=_atproto.{handle}&type=TXT");
    let Ok(bytes) = get(t, &url) else {
        return Ok(None);
    };
    let doc = parse_json(&bytes)?;
    // `{ "Answer": [ { "data": "did=did:plc:…" }, … ] }` (data is sometimes quote-wrapped).
    let did = doc
        .get("Answer")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| a.get("data").and_then(Value::as_str))
        .find_map(|data| {
            data.trim_matches('"')
                .strip_prefix("did=")
                .map(str::to_string)
        })
        .filter(|d| d.starts_with("did:"));
    Ok(did)
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

/// Discover a publication's AT-URI from its **web page**. Every standard.site page advertises
/// its record with a discovery `<link>`:
///
/// ```html
/// <link rel="site.standard.publication" href="at://did:plc:…/site.standard.publication/…"/>
/// ```
///
/// This is the resolution path for a publisher **URL whose host is not an atproto handle** —
/// e.g. a `*.leaflet.pub` subdomain, which serves no `.well-known/atproto-did` and has no
/// `_atproto` DNS record, so [`resolve_did`] can't reach it. From the AT-URI the caller takes
/// the DID and resolves the repo directly (no aggregator). Returns `None` if the page carries
/// no such tag (not a standard.site publication).
pub fn discover_publication_uri<T: Transport>(
    t: &T,
    url: &str,
) -> Result<Option<String>, ReadError> {
    let html = String::from_utf8_lossy(&get(t, url)?).into_owned();
    Ok(extract_publication_link(&html))
}

/// Pull the publication AT-URI out of a page's `<link>` tags: the first `<link>` whose `href`
/// is an `at://…/site.standard.publication/…` URI. Tolerant of attribute order and quote
/// style; a targeted scan beats pulling in an HTML parser for one well-formed discovery tag.
fn extract_publication_link(html: &str) -> Option<String> {
    const COLLECTION: &str = "site.standard.publication";
    html.split("<link")
        .skip(1)
        .filter_map(|frag| {
            let tag = &frag[..frag.find('>').unwrap_or(frag.len())];
            tag_attr(tag, "href")
        })
        .find(|href| href.starts_with("at://") && href.contains(COLLECTION))
        .map(str::to_string)
}

/// Read a `name="value"` (or `name='value'`) attribute out of a tag's inner text.
fn tag_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=");
    let after = &tag[tag.find(&key)? + key.len()..];
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &after[1..];
    rest.find(quote).map(|end| &rest[..end])
}

// --- Orchestration ---------------------------------------------------------------

/// Follow `listRecords` cursors to fetch **every** record of a collection (newest-first per the
/// PDS). Bounded against a misbehaving PDS: the walk ends as soon as a page returns no cursor *or*
/// no records, so it can never loop forever.
fn paginate<T: Transport>(
    t: &T,
    pds: &str,
    did: &str,
    collection: &str,
    page_limit: u32,
) -> Result<Vec<Record>, ReadError> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = xrpc::list_records(pds, did, collection, page_limit, cursor.as_deref());
        let (records, next) = parse_list(&get(t, &url)?)?;
        let got = records.len();
        out.extend(records);
        match next {
            Some(c) if got > 0 => cursor = Some(c),
            _ => break,
        }
    }
    Ok(out)
}

/// A reader's own subscriptions (each points at a publication AT-URI, often cross-repo).
pub fn list_subscriptions<T: Transport>(
    t: &T,
    reader: &Identity,
) -> Result<Vec<Subscription>, ReadError> {
    let records = paginate(
        t,
        &reader.pds,
        &reader.did,
        "site.standard.graph.subscription",
        100,
    )?;
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
    let records = paginate(t, &repo.pds, &repo.did, "site.standard.publication", 100)?;
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

/// **Every** document of a publication's repo (metadata only), newest-first — paginated to
/// completion. Used for the initial backfill so a feed isn't capped at its newest page;
/// [`list_documents`] fetches a single page for the cheap incremental refresh walk.
pub fn list_all_documents<T: Transport>(
    t: &T,
    repo: &Identity,
) -> Result<Vec<Document>, ReadError> {
    let records = paginate(t, &repo.pds, &repo.did, "site.standard.document", 50)?;
    Ok(records
        .iter()
        .filter_map(|r| parse_document(&r.value, &r.uri))
        .collect())
}

/// A **bounded window** of a repo's documents (metadata only), newest-first, starting from
/// `cursor` (`None` = the newest). Walks at most `max_pages` pages and returns the docs plus the
/// cursor to resume from for *older* records — `None` when the repo is exhausted within the window.
/// The building block for the bounded initial fetch and for "load older" (pass the stored resume
/// cursor). Like [`paginate`], it stops as soon as a page returns no cursor or no records.
pub fn list_documents_window<T: Transport>(
    t: &T,
    repo: &Identity,
    cursor: Option<&str>,
    max_pages: u32,
) -> Result<(Vec<Document>, Option<String>), ReadError> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = cursor.map(str::to_string);
    for _ in 0..max_pages.max(1) {
        let (docs, next) = list_documents(t, repo, cursor.as_deref())?;
        let got = docs.len();
        out.extend(docs);
        match next {
            Some(c) if got > 0 => cursor = Some(c),
            _ => return Ok((out, None)), // exhausted within the window
        }
    }
    Ok((out, cursor)) // hit the page cap; `cursor` resumes the older walk
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
    let mut meta = parse_document(&record.value, &doc_uri.to_string())
        .ok_or_else(|| ReadError::Parse(format!("not a document: {doc_uri}")))?;

    let ctx = DecodeCtx {
        repo_did: &doc_uri.did,
    };
    let raw_content = record.value.get("content");

    // Pckt large-document shape: the `[blog.pckt.block.*]` array is externalized to a
    // `text/plain` blob with empty `items`. Fetch it and splice the array into `items` so the
    // normal decode path runs (the decoder is pure). On failure we fall through with the empty
    // content, which degrades to the `textContent` fallback — same as before.
    let spliced: Option<Value> = match raw_content.and_then(external_content_cid) {
        Some(cid) => {
            let blob_url = xrpc::get_blob(pds, &doc_uri.did, cid);
            let items = parse_json(&get(t, &blob_url)?)?;
            let mut c = raw_content.cloned().unwrap_or(Value::Null);
            if let Some(obj) = c.as_object_mut() {
                obj.insert("items".to_string(), items);
            }
            Some(c)
        }
        None => None,
    };
    let content = spliced.as_ref().or(raw_content);
    meta.publishing_platform = registry.publishing_platform(content);

    let body = match content.and_then(content_ref) {
        // Two-phase: the content points at another record; fetch and decode that.
        Some(ref_uri) => {
            if ref_uri.collection == "app.greengale.document" {
                meta.publishing_platform = Some(crate::model::PublishingPlatform::GreenGale);
            }
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

    // Second-phase, like `#contentRef` but per block: a Pckt `gallery` decodes to a
    // `GalleryRef` placeholder; fetch each referenced record and splice in its images.
    let mut body = body;
    resolve_gallery_refs(t, &mut body.blocks, &doc_uri.did, pds);
    let body = or_description(body, meta.description.as_deref());
    Ok((meta, body))
}

/// Fall back to the document's `description` blurb when the decoded body is empty. Some
/// publications keep the full article on the web and publish only a metadata stub to atproto
/// (e.g. `atproto.com/blog`): no `content`, no `textContent` — which would otherwise render as a
/// blank post. The blurb at least gives the reader something; the full text is at the doc's web
/// `path`. A non-empty body is returned unchanged.
fn or_description(mut body: RichDoc, description: Option<&str>) -> RichDoc {
    if body.blocks.is_empty()
        && let Some(desc) = description
        && !desc.trim().is_empty()
    {
        body.blocks
            .push(Block::Paragraph(vec![Inline::Text(desc.to_string())]));
    }
    body
}

/// Replace each [`Block::GalleryRef`] (in `blocks`, recursing into `Quote`/`List` containers)
/// with a resolved [`Block::ImageGrid`]. Best-effort: a ref that fails to fetch/parse is dropped
/// rather than aborting the document, so no `GalleryRef` ever reaches a frontend.
fn resolve_gallery_refs<T: Transport>(
    t: &T,
    blocks: &mut Vec<Block>,
    doc_did: &str,
    doc_pds: &str,
) {
    for block in blocks.iter_mut() {
        match block {
            Block::GalleryRef { uri } => {
                if let Some(images) = fetch_gallery(t, uri, doc_did, doc_pds) {
                    *block = Block::ImageGrid(images);
                }
            }
            Block::Quote(children) => resolve_gallery_refs(t, children, doc_did, doc_pds),
            Block::List { items, .. } => {
                for item in items.iter_mut() {
                    resolve_gallery_refs(t, item, doc_did, doc_pds);
                }
            }
            _ => {}
        }
    }
    // Drop any placeholder that didn't resolve (left a `GalleryRef`).
    blocks.retain(|b| !matches!(b, Block::GalleryRef { .. }));
}

/// Fetch a `blog.pckt.gallery` record and decode its images. Returns `None` (→ the placeholder
/// is dropped) on any failure or an empty gallery. PDS is reused from the document when the
/// referenced record is in the same repo, else resolved — mirroring the `#contentRef` branch.
fn fetch_gallery<T: Transport>(
    t: &T,
    uri: &str,
    doc_did: &str,
    doc_pds: &str,
) -> Option<Vec<Image>> {
    let at = AtUri::parse(uri)?;
    let pds = if at.did == doc_did {
        doc_pds.to_string()
    } else {
        resolve_pds(t, &at.did).ok()?
    };
    let url = xrpc::get_record(&pds, &at.did, &at.collection, &at.rkey);
    let record = parse_get(&get(t, &url).ok()?).ok()?;
    let images = gallery_images(&record.value, &at.did);
    (!images.is_empty()).then_some(images)
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
    fn empty_body_falls_back_to_the_description_blurb() {
        let para = |s: &str| Block::Paragraph(vec![Inline::Text(s.into())]);

        // Metadata-only doc (no content/textContent → empty body): render the description.
        let body = or_description(RichDoc::default(), Some("A short blurb."));
        assert_eq!(body.blocks, vec![para("A short blurb.")]);

        // A real body is left untouched.
        let real = RichDoc {
            blocks: vec![para("the actual post")],
        };
        assert_eq!(or_description(real.clone(), Some("blurb")), real);

        // No (or blank) description → still empty, not a stray empty paragraph.
        assert!(or_description(RichDoc::default(), None).blocks.is_empty());
        assert!(
            or_description(RichDoc::default(), Some("   "))
                .blocks
                .is_empty()
        );
    }

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

    /// A tiny canned-response `Transport`: a URL not in the map "404"s (an `Err`).
    struct MockTransport(std::collections::HashMap<String, Vec<u8>>);

    impl crate::atp::Transport for MockTransport {
        type Error = std::io::Error;
        fn get(&self, url: &str) -> Result<Vec<u8>, Self::Error> {
            self.0
                .get(url)
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, url.to_string()))
        }
        fn post(&self, _: &str, _: &str, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
            Err(std::io::Error::other("no post in mock"))
        }
    }

    #[test]
    fn resolve_did_falls_back_to_dns_when_well_known_is_missing() {
        // Only the DNS-over-HTTPS route answers; the well-known fetch 404s (like pfrazee.com).
        let mut routes = std::collections::HashMap::new();
        routes.insert(
            "https://dns.google/resolve?name=_atproto.pfrazee.com&type=TXT".to_string(),
            br#"{"Answer":[{"name":"_atproto.pfrazee.com.","data":"did=did:plc:ragtjsm2j2vknwkz3zp4oxrd"}]}"#.to_vec(),
        );
        let did = resolve_did(&MockTransport(routes), "pfrazee.com").unwrap();
        assert_eq!(did, "did:plc:ragtjsm2j2vknwkz3zp4oxrd");
    }

    #[test]
    fn resolve_did_prefers_well_known_and_skips_dns() {
        let mut routes = std::collections::HashMap::new();
        routes.insert(
            "https://alice.test/.well-known/atproto-did".to_string(),
            b"did:plc:fromwellknown\n".to_vec(),
        );
        // No DNS route registered → if it reached DNS it would 404; it must not.
        let did = resolve_did(&MockTransport(routes), "alice.test").unwrap();
        assert_eq!(did, "did:plc:fromwellknown");
    }

    #[test]
    fn resolve_did_errors_when_neither_method_resolves() {
        let empty = MockTransport(std::collections::HashMap::new());
        assert!(resolve_did(&empty, "nobody.test").is_err());
    }

    fn doc_record(rkey: &str, title: &str) -> String {
        format!(
            r#"{{"uri":"at://did:plc:abc/site.standard.document/{rkey}","value":{{"title":"{title}","site":"at://did:plc:abc/site.standard.publication/p","publishedAt":"2026-01-01T00:00:00Z"}}}}"#
        )
    }

    #[test]
    fn list_all_documents_pages_through_the_cursor() {
        let repo = Identity {
            did: "did:plc:abc".into(),
            pds: "https://pds.test".into(),
        };
        let page1 = "https://pds.test/xrpc/com.atproto.repo.listRecords?repo=did:plc:abc&collection=site.standard.document&limit=50";
        let page2 = format!("{page1}&cursor=pg2");
        let mut routes = std::collections::HashMap::new();
        routes.insert(
            page1.to_string(),
            format!(
                r#"{{"records":[{}],"cursor":"pg2"}}"#,
                doc_record("1", "One")
            )
            .into_bytes(),
        );
        // Last page: records but no cursor → the walk ends here.
        routes.insert(
            page2,
            format!(r#"{{"records":[{}]}}"#, doc_record("2", "Two")).into_bytes(),
        );
        let docs = list_all_documents(&MockTransport(routes), &repo).unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].title, "One");
        assert_eq!(docs[1].title, "Two");
    }

    #[test]
    fn paginate_stops_on_an_empty_page_even_with_a_cursor() {
        let repo = Identity {
            did: "did:plc:abc".into(),
            pds: "https://pds.test".into(),
        };
        let page1 = "https://pds.test/xrpc/com.atproto.repo.listRecords?repo=did:plc:abc&collection=site.standard.document&limit=50";
        let page2 = format!("{page1}&cursor=pg2");
        let mut routes = std::collections::HashMap::new();
        routes.insert(
            page1.to_string(),
            format!(
                r#"{{"records":[{}],"cursor":"pg2"}}"#,
                doc_record("1", "One")
            )
            .into_bytes(),
        );
        // Empty page that still advertises a cursor: the walk must stop here and never request
        // "pg3" (which isn't routed, so a continued walk would error and fail this test).
        routes.insert(page2, br#"{"records":[],"cursor":"pg3"}"#.to_vec());
        let docs = list_all_documents(&MockTransport(routes), &repo).unwrap();
        assert_eq!(docs.len(), 1);
    }

    /// Build a 3-page repo (1→pg2→pg3→end) once, reused by the window test below.
    fn three_page_routes() -> (Identity, std::collections::HashMap<String, Vec<u8>>) {
        let repo = Identity {
            did: "did:plc:abc".into(),
            pds: "https://pds.test".into(),
        };
        let page1 = "https://pds.test/xrpc/com.atproto.repo.listRecords?repo=did:plc:abc&collection=site.standard.document&limit=50";
        let page2 = format!("{page1}&cursor=pg2");
        let page3 = format!("{page1}&cursor=pg3");
        let mut routes = std::collections::HashMap::new();
        routes.insert(
            page1.to_string(),
            format!(
                r#"{{"records":[{}],"cursor":"pg2"}}"#,
                doc_record("1", "One")
            )
            .into_bytes(),
        );
        routes.insert(
            page2,
            format!(
                r#"{{"records":[{}],"cursor":"pg3"}}"#,
                doc_record("2", "Two")
            )
            .into_bytes(),
        );
        // Last page: records but no cursor → the repo is exhausted here.
        routes.insert(
            page3,
            format!(r#"{{"records":[{}]}}"#, doc_record("3", "Three")).into_bytes(),
        );
        (repo, routes)
    }

    #[test]
    fn window_stops_at_the_page_cap_and_returns_a_resume_cursor() {
        let (repo, routes) = three_page_routes();
        // Cap at 2 pages: we get the first two docs and a cursor to resume the older walk —
        // page 3 is never requested.
        let (docs, resume) = list_documents_window(&MockTransport(routes), &repo, None, 2).unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].title, "One");
        assert_eq!(docs[1].title, "Two");
        assert_eq!(resume.as_deref(), Some("pg3"));
    }

    #[test]
    fn window_returns_none_when_the_repo_is_exhausted_within_the_cap() {
        let (repo, routes) = three_page_routes();
        // A generous cap walks all three pages; the last has no cursor → resume is None (done).
        let (docs, resume) = list_documents_window(&MockTransport(routes), &repo, None, 5).unwrap();
        assert_eq!(docs.len(), 3);
        assert_eq!(resume, None);
    }

    #[test]
    fn window_resumes_from_a_given_cursor() {
        let (repo, routes) = three_page_routes();
        // "Load older" starting at pg3 (the stored resume cursor): just the last page, exhausted.
        let (docs, resume) =
            list_documents_window(&MockTransport(routes), &repo, Some("pg3"), 3).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title, "Three");
        assert_eq!(resume, None);
    }

    #[test]
    fn extracts_publication_uri_from_a_discovery_link() {
        // The shape Leaflet (and other standard.site publishers) serve in <head>.
        let html = r#"<head>
            <link rel="alternate" type="application/rss+xml" href="https://x.leaflet.pub/rss"/>
            <link rel="alternate" href="at://did:plc:rnpkyqnmsw4ipey6eotbdnnf/site.standard.publication/3lyiajoe55c2b"/>
            <link rel="site.standard.publication" href="at://did:plc:rnpkyqnmsw4ipey6eotbdnnf/site.standard.publication/3lyiajoe55c2b"/>
        </head>"#;
        assert_eq!(
            extract_publication_link(html).as_deref(),
            Some("at://did:plc:rnpkyqnmsw4ipey6eotbdnnf/site.standard.publication/3lyiajoe55c2b")
        );
    }

    #[test]
    fn extract_publication_link_ignores_non_publication_links() {
        // Only RSS/web alternates, no atproto publication link → nothing to discover.
        let html = r#"<link rel="alternate" type="application/json" href="https://x.leaflet.pub/json"/>
            <link rel="icon" href="/favicon.ico"/>"#;
        assert_eq!(extract_publication_link(html), None);
    }

    #[test]
    fn discover_publication_uri_fetches_then_extracts() {
        let mut routes = std::collections::HashMap::new();
        routes.insert(
            "https://retrobailey.leaflet.pub/".to_string(),
            br#"<html><head><link rel="site.standard.publication" href="at://did:plc:rnpkyqnmsw4ipey6eotbdnnf/site.standard.publication/3lyiajoe55c2b"/></head></html>"#.to_vec(),
        );
        let got =
            discover_publication_uri(&MockTransport(routes), "https://retrobailey.leaflet.pub/")
                .unwrap();
        assert_eq!(
            got.as_deref(),
            Some("at://did:plc:rnpkyqnmsw4ipey6eotbdnnf/site.standard.publication/3lyiajoe55c2b")
        );
    }
}
