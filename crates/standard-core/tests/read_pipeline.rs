//! The read pipeline driven offline by a `MockTransport` over **real** captured XRPC
//! responses (`tests/fixtures/xrpc/`). Deterministic, no network — yet it exercises the
//! full live shape: handle→DID→PDS resolution, the cross-repo subscription path, document
//! listing/decoding, the unhandled-type fallback, and the GreenGale two-phase `#contentRef`.

use std::collections::HashMap;
use std::fmt;

use serde_json::Value;

use standard_core::atp::{AtUri, Transport, xrpc};
use standard_core::decode::Registry;
use standard_core::model::{Block, ImageSource, Inline, PublishingPlatform};
use standard_core::read;

const DAVID_DID: &str = "did:plc:xn3l7ogsxym5ixxugidum5dw";
const DAVID_PDS: &str = "https://yapfest.club";
const HALF_DID: &str = "did:plc:xbtmt2zjwlrfegqvch7fboei";
const HALF_PDS: &str = "https://pds.zzstoatzz.io";

// --- MockTransport ---------------------------------------------------------------

#[derive(Debug)]
struct MockError(String);
impl fmt::Display for MockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for MockError {}

struct MockTransport {
    responses: HashMap<String, Vec<u8>>,
}

impl Transport for MockTransport {
    type Error = MockError;
    fn get(&self, url: &str) -> Result<Vec<u8>, MockError> {
        self.responses
            .get(url)
            .cloned()
            .ok_or_else(|| MockError(format!("no mock registered for GET {url}")))
    }
    fn post(&self, url: &str, _ct: &str, _body: &[u8]) -> Result<Vec<u8>, MockError> {
        Err(MockError(format!("unexpected POST {url}")))
    }
}

fn fixture(rel: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{rel}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|e| panic!("fixture {rel}: {e}"))
}

/// Build the transport with every URL the pipeline will request mapped to its captured
/// response. URLs are produced by the same `xrpc` builders the pipeline uses, so they
/// match by construction.
fn mock() -> MockTransport {
    let mut r: HashMap<String, Vec<u8>> = HashMap::new();

    // resolution
    r.insert(
        "https://david.yapfest.club/.well-known/atproto-did".into(),
        fixture("xrpc/wellknown_atproto_did.txt"),
    );
    r.insert(
        xrpc::plc_directory(DAVID_DID),
        fixture("xrpc/plc_david.json"),
    );
    r.insert(
        xrpc::plc_directory(HALF_DID),
        fixture("xrpc/plc_halfbaked.json"),
    );

    // David's subscriptions
    r.insert(
        xrpc::list_records(
            DAVID_PDS,
            DAVID_DID,
            "site.standard.graph.subscription",
            100,
            None,
        ),
        fixture("xrpc/list_subscriptions.json"),
    );
    // The captured first page carries a cursor; `list_subscriptions` paginates, so register the
    // next page (empty → ends the walk). Read the cursor from the fixture to stay in sync with it.
    let subs_first: Value =
        serde_json::from_slice(&fixture("xrpc/list_subscriptions.json")).unwrap();
    if let Some(cursor) = subs_first["cursor"].as_str() {
        r.insert(
            xrpc::list_records(
                DAVID_PDS,
                DAVID_DID,
                "site.standard.graph.subscription",
                100,
                Some(cursor),
            ),
            br#"{"records":[]}"#.to_vec(),
        );
    }

    // the subscribed "half baked" publication + its documents
    r.insert(
        xrpc::get_record(
            HALF_PDS,
            HALF_DID,
            "site.standard.publication",
            "3mbmm4qeiy2to",
        ),
        fixture("xrpc/getrecord_pub_halfbaked.json"),
    );
    r.insert(
        xrpc::list_records(HALF_PDS, HALF_DID, "site.standard.document", 50, None),
        fixture("xrpc/list_documents_halfbaked.json"),
    );
    // a getRecord for each listed doc (synthesized from the list envelope's records)
    let list: Value =
        serde_json::from_slice(&fixture("xrpc/list_documents_halfbaked.json")).unwrap();
    for rec in list["records"].as_array().unwrap() {
        let uri = AtUri::parse(rec["uri"].as_str().unwrap()).unwrap();
        r.insert(
            xrpc::get_record(HALF_PDS, &uri.did, &uri.collection, &uri.rkey),
            serde_json::to_vec(rec).unwrap(),
        );
    }

    // David's GreenGale doc for the two-phase #contentRef (both records)
    r.insert(
        xrpc::get_record(
            DAVID_PDS,
            DAVID_DID,
            "site.standard.document",
            "3mmozgypkle2s",
        ),
        fixture("greengale.contentref.json"),
    );
    r.insert(
        xrpc::get_record(
            DAVID_PDS,
            DAVID_DID,
            "app.greengale.document",
            "3mmozgypkle2s",
        ),
        fixture("greengale.document.json"),
    );

    // David's Pckt doc (contains a gallery block) + the referenced gallery record, for the
    // per-block two-phase resolution.
    r.insert(
        xrpc::get_record(
            DAVID_PDS,
            DAVID_DID,
            "site.standard.document",
            "3mmrd52hpdakk",
        ),
        fixture("pckt.json"),
    );
    r.insert(
        xrpc::get_record(DAVID_PDS, DAVID_DID, "blog.pckt.gallery", "3mmrn4i26e5al"),
        fixture("xrpc/getrecord_gallery_pckt.json"),
    );

    MockTransport { responses: r }
}

// --- Tests -----------------------------------------------------------------------

#[test]
fn resolves_handle_to_identity() {
    let id = read::resolve_identity(&mock(), "david.yapfest.club").unwrap();
    assert_eq!(id.did, DAVID_DID);
    assert_eq!(id.pds, DAVID_PDS);
}

#[test]
fn full_cross_repo_subscription_flow() {
    let t = mock();
    let registry = Registry::with_defaults();

    // reader -> subscriptions -> a publication in ANOTHER repo
    let reader = read::resolve_identity(&t, DAVID_DID).unwrap();
    let subs = read::list_subscriptions(&t, &reader).unwrap();
    assert_eq!(subs.len(), 1);

    let pub_uri = AtUri::parse(&subs[0].publication).unwrap();
    assert_eq!(pub_uri.did, HALF_DID);

    // fetch that publication (cross-repo) + list its documents
    let (publication, repo) = read::get_publication(&t, &pub_uri).unwrap();
    assert_eq!(publication.name, "half baked");
    assert_eq!(repo.pds, HALF_PDS);

    let (docs, _cursor) = read::list_documents(&t, &repo, None).unwrap();
    assert_eq!(docs.len(), 5);

    // decode the first (a Pckt doc) end-to-end
    let pckt_uri = AtUri::parse(&docs[0].uri).unwrap();
    let (meta, body) = read::get_document(&t, &registry, &pckt_uri, &repo.pds).unwrap();
    assert_eq!(meta.publication, subs[0].publication);
    assert_eq!(meta.publishing_platform, Some(PublishingPlatform::Pckt));
    assert!(!body.blocks.is_empty(), "pckt body should decode to blocks");
}

#[test]
fn unthread_content_decodes_as_markdown() {
    let t = mock();
    let registry = Registry::with_defaults();
    // 3mj5rpb2gnc23 is `at.unthread.content` — a Markdown string in `content`. It must decode
    // through the Markdown pipeline (formatted), not the `*`-stripped textContent fallback.
    let uri = AtUri::parse(&format!(
        "at://{HALF_DID}/site.standard.document/3mj5rpb2gnc23"
    ))
    .unwrap();
    let (_, body) = read::get_document(&t, &registry, &uri, HALF_PDS).unwrap();
    assert!(!body.blocks.is_empty());
    // The record's body opens with `*…*`; decoding yields an Emphasis inline (the flat
    // textContent fallback would carry no inline formatting at all).
    let has_emphasis = body.blocks.iter().any(|b| {
        matches!(b, Block::Paragraph(inlines)
            if inlines.iter().any(|i| matches!(i, Inline::Emphasis(_))))
    });
    assert!(
        has_emphasis,
        "unthread markdown should produce formatted inlines"
    );
}

#[test]
fn pckt_gallery_ref_resolves_to_image_grid() {
    let t = mock();
    let registry = Registry::with_defaults();
    // David's Pckt doc has a `gallery` block referencing `blog.pckt.gallery/3mmrn4i26e5al`.
    // get_document must fetch that record and splice in a resolved ImageGrid (two-phase, per
    // block) — and leave no GalleryRef placeholder behind.
    let uri = AtUri::parse(&format!(
        "at://{DAVID_DID}/site.standard.document/3mmrd52hpdakk"
    ))
    .unwrap();
    let (meta, body) = read::get_document(&t, &registry, &uri, DAVID_PDS).unwrap();
    assert_eq!(meta.publishing_platform, Some(PublishingPlatform::Pckt));

    let grid = body
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::ImageGrid(images) => Some(images),
            _ => None,
        })
        .expect("the gallery should resolve to a Block::ImageGrid");
    assert_eq!(grid.len(), 2, "the fixture gallery has two images");
    assert!(
        grid.iter()
            .all(|i| matches!(&i.source, ImageSource::Blob { .. })),
        "gallery images are atproto blobs"
    );
    assert!(
        !body
            .blocks
            .iter()
            .any(|b| matches!(b, Block::GalleryRef { .. })),
        "no unresolved GalleryRef should remain"
    );
}

#[test]
fn pckt_externalized_content_blob_is_fetched_and_decoded() {
    // Pckt's large-document shape (the "bee" script, ~114 KB): `items` is empty and the real
    // `[blog.pckt.block.*]` array lives in a text/plain blob. get_document must fetch that blob,
    // splice the array in, and decode it — NOT fall back to flat `textContent` (whose `\n`s
    // render as one run-on block).
    let did = "did:plc:beepub";
    let pds = "https://margin.cafe";
    let rkey = "3mbnc7czrc2gr";
    let cid = "bafblockarray";

    let doc_record = serde_json::json!({
        "uri": format!("at://{did}/site.standard.document/{rkey}"),
        "value": {
            "$type": "site.standard.document",
            "title": "bee",
            "site": format!("at://{did}/site.standard.publication/3pub"),
            "publishedAt": "2026-01-05T01:33:08Z",
            // The fallback the bug exposed — present, but must NOT be used now.
            "textContent": "bee\nline two\nline three",
            "content": {
                "$type": "blog.pckt.content",
                "items": [],
                "blob": { "$type": "blob", "ref": { "$link": cid }, "mimeType": "text/plain", "size": 114000 },
                "references": []
            }
        }
    });
    // What the content blob holds: the externalized block array.
    let block_array = serde_json::json!([
        { "$type": "blog.pckt.block.text", "plaintext": "bee" },
        { "$type": "blog.pckt.block.text", "plaintext": "line two" },
        { "$type": "blog.pckt.block.text", "plaintext": "line three" }
    ]);

    let mut routes: HashMap<String, Vec<u8>> = HashMap::new();
    routes.insert(
        xrpc::get_record(pds, did, "site.standard.document", rkey),
        serde_json::to_vec(&doc_record).unwrap(),
    );
    routes.insert(
        xrpc::get_blob(pds, did, cid),
        serde_json::to_vec(&block_array).unwrap(),
    );
    let t = MockTransport { responses: routes };

    let registry = Registry::with_defaults();
    let uri = AtUri::parse(&format!("at://{did}/site.standard.document/{rkey}")).unwrap();
    let (meta, body) = read::get_document(&t, &registry, &uri, pds).unwrap();

    assert_eq!(meta.title, "bee");
    let paras: Vec<&Block> = body
        .blocks
        .iter()
        .filter(|b| matches!(b, Block::Paragraph(_)))
        .collect();
    assert_eq!(
        paras.len(),
        3,
        "each externalized text block becomes its own paragraph (not one run-on plaintext block)"
    );
    assert!(
        matches!(&body.blocks[0], Block::Paragraph(c) if c == &[Inline::Text("bee".into())]),
        "first block is the decoded first line, proving the blob was fetched + decoded"
    );
}

#[test]
fn greengale_two_phase_contentref_fetches_referenced_record() {
    let t = mock();
    let registry = Registry::with_defaults();
    // The site.standard.document content is a #contentRef; get_document must fetch the
    // referenced app.greengale.document and decode ITS bare-markdown body.
    let uri = AtUri::parse(&format!(
        "at://{DAVID_DID}/site.standard.document/3mmozgypkle2s"
    ))
    .unwrap();
    let (meta, body) = read::get_document(&t, &registry, &uri, DAVID_PDS).unwrap();
    assert_eq!(
        meta.publishing_platform,
        Some(PublishingPlatform::GreenGale)
    );
    assert!(body.blocks.iter().any(|b| matches!(
        b,
        Block::Heading { level: 1, content } if content == &[standard_core::model::Inline::Text("BLAH BLAH".into())]
    )));
}
