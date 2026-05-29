//! Integration tests over **real records** pulled from live PDSes (saved under
//! `tests/fixtures/`). Passing here means the decoders match production data, not just
//! hand-written samples.

use serde_json::Value;

use standard_core::atp::AtUri;
use standard_core::decode::{DecodeCtx, Registry, content_ref};
use standard_core::model::{Block, ImageSource, Inline, RichDoc};

/// Load a saved `getRecord` response and decode its `content`, using the record's own
/// repo DID for blob refs.
fn decode_fixture(name: &str) -> RichDoc {
    let raw = std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture exists");
    let record: Value = serde_json::from_str(&raw).unwrap();
    let value = &record["value"];
    let did = AtUri::parse(record["uri"].as_str().unwrap()).unwrap().did;

    Registry::with_defaults().decode(
        value.get("content"),
        value.get("textContent").and_then(Value::as_str),
        &DecodeCtx { repo_did: &did },
    )
}

#[test]
fn pckt_real_record() {
    let doc = decode_fixture("pckt.json");

    // Plain text, then a bold facet over the whole run (byte-range, not run marks).
    assert_eq!(
        doc.blocks[0],
        Block::Paragraph(vec![Inline::Text("test".into())])
    );
    assert_eq!(
        doc.blocks[1],
        Block::Paragraph(vec![Inline::Strong(vec![Inline::Text("bold".into())])])
    );
    // Stacked features nest, first feature outermost.
    assert_eq!(
        doc.blocks[2],
        Block::Paragraph(vec![Inline::Strong(vec![Inline::Emphasis(vec![
            Inline::Text("italic and bold".into())
        ])])])
    );

    // Heading, nested list, blob image, rule, and an underline all decode.
    assert!(doc.blocks.iter().any(|b| matches!(
        b,
        Block::Heading { level: 1, content } if content == &[Inline::Text("heading one".into())]
    )));
    assert!(
        doc.blocks
            .iter()
            .any(|b| matches!(b, Block::List { ordered: false, .. }))
    );
    assert!(doc.blocks.iter().any(|b| matches!(
        b,
        Block::Image(img) if matches!(&img.source, ImageSource::Blob { .. })
    )));
    assert!(doc.blocks.iter().any(|b| matches!(b, Block::Rule)));
    assert!(
        has_underline(&doc.blocks),
        "underline facet should decode to Inline::Underline"
    );
    // the table decodes to a Block::Table with a 3-column header and body rows.
    assert!(
        doc.blocks.iter().any(|b| matches!(
            b,
            Block::Table { head, rows } if head.len() == 3 && !rows.is_empty()
        )),
        "pckt table should decode to a Block::Table"
    );
}

/// Recursively search decoded blocks for an `Inline::Underline`.
fn has_underline(blocks: &[Block]) -> bool {
    fn in_inlines(inlines: &[Inline]) -> bool {
        inlines.iter().any(|i| match i {
            Inline::Underline(_) => true,
            Inline::Strong(c) | Inline::Emphasis(c) | Inline::Strike(c) => in_inlines(c),
            Inline::Link { content, .. } => in_inlines(content),
            _ => false,
        })
    }
    blocks.iter().any(|b| match b {
        Block::Paragraph(c) | Block::Heading { content: c, .. } => in_inlines(c),
        Block::Quote(b) => has_underline(b),
        Block::List { items, .. } => items.iter().any(|it| has_underline(it)),
        _ => false,
    })
}

#[test]
fn leaflet_real_record() {
    let doc = decode_fixture("leaflet.json");
    assert_eq!(doc.blocks.len(), 4);
    assert_eq!(
        doc.blocks[0],
        Block::Paragraph(vec![Inline::Text("Test".into())])
    );
    // Strikethrough facet over the whole word.
    assert_eq!(
        doc.blocks[1],
        Block::Paragraph(vec![Inline::Strike(vec![Inline::Text("test".into())])])
    );
    assert_eq!(
        doc.blocks[3],
        Block::Heading {
            level: 3,
            content: vec![Inline::Text("testtest".into())]
        }
    );
}

#[test]
fn offprint_real_record() {
    let doc = decode_fixture("offprint.json");
    // 12 text + 2 image + 1 bulletList + 1 callout, all mapped, in order.
    assert_eq!(doc.blocks.len(), 16);

    match &doc.blocks[0] {
        Block::Paragraph(inlines) => match &inlines[0] {
            Inline::Text(t) => assert!(t.starts_with("This'll be probably the 30th blog")),
            other => panic!("expected leading text, got {other:?}"),
        },
        other => panic!("expected paragraph first, got {other:?}"),
    }

    // Image blocks resolve to blob refs against this record's DID.
    assert!(matches!(
        &doc.blocks[3],
        Block::Image(img) if matches!(&img.source, ImageSource::Blob { did, .. }
            if did == "did:plc:gj55urnejshc53jzje5afyk2")
    ));
    assert!(matches!(&doc.blocks[8], Block::List { ordered: false, .. }));
    // Callout decodes to a real Block::Callout, keeping its emoji + tint colour.
    assert!(matches!(
        &doc.blocks[11],
        Block::Callout {
            emoji: Some(_),
            tint: Some(_),
            ..
        }
    ));
}

#[test]
fn offprint_heading_imagegrid_and_rule_are_not_dropped() {
    // The "Galaxy Buds" review uses heading / imageGrid / horizontalRule blocks that the
    // decoder previously dropped (the "I absolutely love the new design." line vanished).
    let doc = decode_fixture("offprint_galaxybuds.json");

    assert!(
        doc.blocks.iter().any(|b| matches!(
            b,
            Block::Heading { level: 3, content }
                if content.iter().any(|i| matches!(i, Inline::Text(t) if t.contains("absolutely love the new design")))
        )),
        "offprint heading should render (not be dropped)"
    );
    // imageGrid groups several blob images; horizontalRule → a Rule.
    assert!(
        doc.blocks
            .iter()
            .any(|b| matches!(b, Block::ImageGrid(imgs) if imgs.len() >= 2)),
        "imageGrid should decode to a Block::ImageGrid of several images"
    );
    assert!(doc.blocks.iter().any(|b| matches!(b, Block::Rule)));
}

#[test]
fn greengale_two_phase_contentref_then_markdown() {
    // Phase 1: site.standard.document.content is a reference, not inline content.
    let raw = std::fs::read_to_string(format!(
        "{}/tests/fixtures/greengale.contentref.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let record: Value = serde_json::from_str(&raw).unwrap();
    let uri = content_ref(&record["value"]["content"]).expect("greengale content is a ref");
    assert_eq!(uri.collection, "app.greengale.document");

    // Phase 2: decode the referenced record's bare-markdown body.
    let doc = decode_fixture("greengale.document.json");
    assert!(!doc.blocks.is_empty());

    // The markdown's `# BLAH BLAH` heading survives.
    assert!(doc.blocks.iter().any(|b| matches!(
        b,
        Block::Heading { level: 1, content } if content == &[Inline::Text("BLAH BLAH".into())]
    )));
    // A fenced code block and a getBlob image URL both decode.
    assert!(doc.blocks.iter().any(|b| matches!(b, Block::Code { .. })));
    assert!(contains_image_url(&doc.blocks, "com.atproto.sync.getBlob"));
}

/// The greengale body's content is a bare string (no `$type`), so it must route to the
/// Markdown decoder — i.e. it is NOT a contentRef itself.
#[test]
fn greengale_body_is_not_itself_a_ref() {
    let raw = std::fs::read_to_string(format!(
        "{}/tests/fixtures/greengale.document.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let record: Value = serde_json::from_str(&raw).unwrap();
    assert!(content_ref(&record["value"]["content"]).is_none());
}

fn contains_image_url(blocks: &[Block], needle: &str) -> bool {
    fn inline_has(inlines: &[Inline], needle: &str) -> bool {
        inlines.iter().any(|i| match i {
            Inline::Image(img) => matches!(&img.source, ImageSource::Url(u) if u.contains(needle)),
            Inline::Strong(c) | Inline::Emphasis(c) | Inline::Strike(c) => inline_has(c, needle),
            Inline::Link { content, .. } => inline_has(content, needle),
            _ => false,
        })
    }
    blocks.iter().any(|b| match b {
        Block::Paragraph(c) | Block::Heading { content: c, .. } => inline_has(c, needle),
        Block::Image(img) => matches!(&img.source, ImageSource::Url(u) if u.contains(needle)),
        Block::Quote(b) => contains_image_url(b, needle),
        Block::List { items, .. } => items.iter().any(|it| contains_image_url(it, needle)),
        _ => false,
    })
}

/// Every link href anywhere in the blocks (recursing through styled spans + containers).
fn link_hrefs(blocks: &[Block]) -> Vec<String> {
    fn from_inlines(inlines: &[Inline], out: &mut Vec<String>) {
        for i in inlines {
            match i {
                Inline::Link { href, content } => {
                    out.push(href.clone());
                    from_inlines(content, out);
                }
                Inline::Strong(c)
                | Inline::Emphasis(c)
                | Inline::Strike(c)
                | Inline::Underline(c)
                | Inline::Highlight(c) => from_inlines(c, out),
                _ => {}
            }
        }
    }
    fn walk(blocks: &[Block], out: &mut Vec<String>) {
        for b in blocks {
            match b {
                Block::Paragraph(c) | Block::Heading { content: c, .. } => from_inlines(c, out),
                Block::Callout { content, .. } => from_inlines(content, out),
                Block::Quote(bs) => walk(bs, out),
                Block::List { items, .. } => items.iter().for_each(|it| walk(it, out)),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(blocks, &mut out);
    out
}

/// Whether any inline anywhere is an `Inline::Highlight`.
fn has_highlight(blocks: &[Block]) -> bool {
    fn in_inlines(inlines: &[Inline]) -> bool {
        inlines.iter().any(|i| match i {
            Inline::Highlight(_) => true,
            Inline::Strong(c)
            | Inline::Emphasis(c)
            | Inline::Strike(c)
            | Inline::Underline(c)
            | Inline::Link { content: c, .. } => in_inlines(c),
            _ => false,
        })
    }
    fn walk(blocks: &[Block]) -> bool {
        blocks.iter().any(|b| match b {
            Block::Paragraph(c) | Block::Heading { content: c, .. } => in_inlines(c),
            Block::Callout { content, .. } => in_inlines(content),
            Block::Quote(bs) => walk(bs),
            Block::List { items, .. } => items.iter().any(|it| walk(it)),
            _ => false,
        })
    }
    walk(blocks)
}

/// The Offprint "all available blocks" reference doc: every block + facet type must survive,
/// not just the 7/15 we handled before. (Validated live against blog.aka.dad.)
#[test]
fn offprint_all_blocks_and_facets_are_covered() {
    let doc = decode_fixture("offprint_allblocks.json");
    let b = &doc.blocks;

    assert!(b.iter().any(|x| matches!(x, Block::Quote(_))), "blockquote");
    assert!(
        b.iter().any(|x| matches!(x, Block::Code { .. })),
        "codeBlock"
    );
    assert!(
        b.iter()
            .any(|x| matches!(x, Block::List { ordered: true, .. })),
        "orderedList"
    );
    // Task-list items carry a checkbox glyph (the model has no checked field).
    assert!(
        b.iter().any(|x| matches!(x, Block::List { items, .. }
            if items.iter().flatten().any(|blk| matches!(blk, Block::Paragraph(c)
                if c.iter().any(|i| matches!(i, Inline::Text(t) if t.starts_with("☐ ") || t.starts_with("☑ "))))))),
        "taskList checkboxes"
    );
    // imageGrid + imageCarousel + imageDiff all collapse to ImageGrid (≥3 across the doc).
    assert!(
        b.iter()
            .filter(|x| matches!(x, Block::ImageGrid(_)))
            .count()
            >= 3,
        "carousel/grid/diff → ImageGrid"
    );

    let hrefs = link_hrefs(b);
    assert!(
        hrefs.iter().any(|h| h.contains("example.com")),
        "webEmbed/webMention → link"
    );
    assert!(
        hrefs.iter().any(|h| h.contains("raycast.com")),
        "webBookmark → link"
    );
    assert!(
        hrefs
            .iter()
            .any(|h| h.starts_with("https://bsky.app/profile/")),
        "mention facet → bsky profile link"
    );
    assert!(has_highlight(b), "highlight facet → Inline::Highlight");
}

/// Leaflet's catch-all used to drop lists and embeds; now they decode (validated against a live
/// retrobailey.leaflet.pub record covering unorderedList + bskyPost + button).
#[test]
fn leaflet_lists_and_embeds_are_not_dropped() {
    let doc = decode_fixture("leaflet_embeds.json");
    let b = &doc.blocks;

    assert!(b.iter().any(|x| matches!(x, Block::List { .. })), "list");
    let hrefs = link_hrefs(b);
    assert!(
        hrefs
            .iter()
            .any(|h| h.starts_with("https://bsky.app/profile/")),
        "bskyPost → bsky.app link"
    );
    assert!(
        hrefs
            .iter()
            .any(|h| !h.starts_with("https://bsky.app/profile/")),
        "button/website → link"
    );
}
