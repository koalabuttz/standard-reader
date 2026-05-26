//! Offprint — `app.offprint.content`: a flat `items` list of `app.offprint.block.*`.
//!
//! Validated against a live record (`mikestevens.link`). Text blocks carry byte-range
//! facets (the shared [`facets`](super::facets) shape); images embed a blob ref.

use serde_json::Value;

use super::facets::text_block_inlines;
use super::image::blob_image;
use super::{ContentDecoder, DecodeCtx};
use crate::model::{Block, Inline, RichDoc};

pub struct Offprint;

impl ContentDecoder for Offprint {
    fn handles(&self, content: &Value) -> bool {
        content.get("$type").and_then(Value::as_str) == Some("app.offprint.content")
    }

    fn decode(&self, content: &Value, ctx: &DecodeCtx) -> Option<RichDoc> {
        let items = content.get("items")?.as_array()?;
        let mut blocks = Vec::new();
        for item in items {
            match item.get("$type").and_then(Value::as_str) {
                Some("app.offprint.block.text") => {
                    blocks.push(Block::Paragraph(text_block_inlines(item)));
                }
                Some("app.offprint.block.image") => {
                    if let Some(img) = item
                        .get("image")
                        .and_then(|b| blob_image(b, ctx.repo_did, ""))
                    {
                        blocks.push(Block::Image(img));
                    }
                }
                Some("app.offprint.block.bulletList") => blocks.push(bullet_list(item)),
                Some("app.offprint.block.callout") => blocks.push(callout(item)),
                // Unknown block: skip it; the rest of the document still renders.
                _ => {}
            }
        }
        Some(RichDoc { blocks })
    }
}

/// `{ children: [{ content: <text block> }] }` → an unordered [`Block::List`].
fn bullet_list(block: &Value) -> Block {
    let items = block
        .get("children")
        .and_then(Value::as_array)
        .map(|children| {
            children
                .iter()
                .filter_map(|c| c.get("content"))
                .map(|content| vec![Block::Paragraph(text_block_inlines(content))])
                .collect()
        })
        .unwrap_or_default();
    Block::List {
        ordered: false,
        items,
    }
}

/// No neutral callout block yet, so degrade to a quote (prepend the emoji; drop color).
fn callout(block: &Value) -> Block {
    let emoji = block.get("emoji").and_then(Value::as_str).unwrap_or("");
    let text = block.get("plaintext").and_then(Value::as_str).unwrap_or("");
    let line = if emoji.is_empty() {
        text.to_string()
    } else {
        format!("{emoji} {text}")
    };
    Block::Quote(vec![Block::Paragraph(vec![Inline::Text(line)])])
}
