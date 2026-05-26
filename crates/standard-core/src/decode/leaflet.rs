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
use crate::model::{Block, RichDoc};

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
        // Lists, embeds, button, math, poll: degrade to nothing for v1.
        _ => None,
    }
}
