//! Leaflet — `pub.leaflet.content`: `pages[].blocks[].block`, where each block is a
//! `pub.leaflet.blocks.*` value. Text blocks carry the shared byte-range facets.
//!
//! Validated against a live record: the content `$type` is `pub.leaflet.content` (the
//! old stub matched the nested *page* type `pub.leaflet.pages.linearDocument`, which
//! never fired), and block NSIDs are `pub.leaflet.blocks.*` (plural).

use serde_json::Value;

use super::facets::text_block_inlines;
use super::image::blob_image;
use super::{ContentDecoder, DecodeCtx};
use crate::model::{Block, Inline, RichDoc};

pub struct Leaflet;

impl ContentDecoder for Leaflet {
    fn handles(&self, content: &Value) -> bool {
        content.get("$type").and_then(Value::as_str) == Some("pub.leaflet.content")
    }

    fn decode(&self, content: &Value, ctx: &DecodeCtx) -> Option<RichDoc> {
        let pages = content.get("pages")?.as_array()?;
        let mut blocks = Vec::new();
        for page in pages {
            let Some(entries) = page.get("blocks").and_then(Value::as_array) else {
                continue;
            };
            for entry in entries {
                // Each entry wraps the real block under `block` (alignment lives here too).
                if let Some(block) = entry.get("block").and_then(|b| leaflet_block(b, ctx)) {
                    blocks.push(block);
                }
            }
        }
        Some(RichDoc { blocks })
    }
}

fn leaflet_block(block: &Value, ctx: &DecodeCtx) -> Option<Block> {
    match block.get("$type").and_then(Value::as_str)? {
        "pub.leaflet.blocks.text" => Some(Block::Paragraph(text_block_inlines(block))),
        "pub.leaflet.blocks.header" => {
            let level = block
                .get("level")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 6) as u8;
            Some(Block::Heading {
                level,
                content: text_block_inlines(block),
            })
        }
        "pub.leaflet.blocks.blockquote" => Some(Block::Quote(vec![Block::Paragraph(
            text_block_inlines(block),
        )])),
        "pub.leaflet.blocks.code" => {
            let text = block
                .get("plaintext")
                .or_else(|| block.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let lang = block
                .get("language")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(Block::Code { lang, text })
        }
        "pub.leaflet.blocks.image" => block
            .get("image")
            .and_then(|b| blob_image(b, ctx.repo_did, ""))
            .map(Block::Image),
        "pub.leaflet.blocks.horizontalRule" => Some(Block::Rule),
        "pub.leaflet.blocks.unorderedList" => Some(Block::List {
            ordered: false,
            items: list_items(block),
        }),
        "pub.leaflet.blocks.orderedList" => Some(Block::List {
            ordered: true,
            items: list_items(block),
        }),
        // Embeds a TUI can't host degrade to a clickable link (reusing the link machinery).
        "pub.leaflet.blocks.website" => block
            .get("src")
            .and_then(Value::as_str)
            .map(|src| link_paragraph(src, src)),
        "pub.leaflet.blocks.button" => {
            let url = block.get("url").and_then(Value::as_str)?;
            let text = block
                .get("text")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(url);
            Some(link_paragraph(url, text))
        }
        "pub.leaflet.blocks.bskyPost" => {
            let uri = block
                .get("postRef")
                .and_then(|p| p.get("uri"))
                .and_then(Value::as_str)?;
            let href = bsky_post_url(uri).unwrap_or_else(|| uri.to_string());
            Some(link_paragraph(&href, "View on Bluesky"))
        }
        // A reference to another standard.site post: we can't resolve its web URL here, so show
        // the AT-URI as a marker rather than dropping the reference.
        "pub.leaflet.blocks.standardSitePost" => block
            .get("uri")
            .and_then(Value::as_str)
            .map(|uri| Block::Paragraph(vec![Inline::Text(format!("↪ {uri}"))])),
        // Poll/math/embeds we don't model: a small text marker beats silent loss.
        "pub.leaflet.blocks.poll" => Some(Block::Paragraph(vec![Inline::Text("📊 [poll]".into())])),
        _ => None,
    }
}

/// `{ children: [{ content: <text block> }] }` → list items (one paragraph each). Leaflet wraps
/// each item in a `…#listItem` whose `content` is a single text block.
fn list_items(block: &Value) -> Vec<Vec<Block>> {
    block
        .get("children")
        .and_then(Value::as_array)
        .map(|children| {
            children
                .iter()
                .filter_map(|c| c.get("content"))
                .map(|content| vec![Block::Paragraph(text_block_inlines(content))])
                .collect()
        })
        .unwrap_or_default()
}

/// A paragraph holding one clickable link, labelled (🔗) by `label`.
fn link_paragraph(href: &str, label: &str) -> Block {
    Block::Paragraph(vec![Inline::Link {
        href: href.to_string(),
        content: vec![Inline::Text(format!("🔗 {label}"))],
    }])
}

/// `at://<did>/app.bsky.feed.post/<rkey>` → the post's `bsky.app` web URL, so the embed becomes a
/// link a browser can open. `None` if the AT-URI isn't the expected shape.
fn bsky_post_url(uri: &str) -> Option<String> {
    let mut parts = uri.strip_prefix("at://")?.splitn(3, '/');
    let did = parts.next()?;
    let _collection = parts.next()?;
    let rkey = parts.next()?;
    (!did.is_empty() && !rkey.is_empty())
        .then(|| format!("https://bsky.app/profile/{did}/post/{rkey}"))
}
