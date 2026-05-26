//! Pckt — `blog.pckt.content`: a (possibly nested) list of `blog.pckt.block.*`.
//!
//! Validated against a formatted live record: rich text uses the **shared byte-range
//! facets** (`blog.pckt.richtext.facet#bold|italic|strikethrough|underline|link`), *not*
//! the run-level `marks` the published survey described. Containers (lists, blockquote,
//! table) nest child blocks under a `content` array, so decoding is recursive; images
//! nest their blob under `attrs`.

use serde_json::Value;

use super::facets::text_block_inlines;
use super::image::blob_image;
use super::{ContentDecoder, DecodeCtx};
use crate::model::{Block, RichDoc};

pub struct Pckt;

impl ContentDecoder for Pckt {
    fn handles(&self, content: &Value) -> bool {
        content.get("$type").and_then(Value::as_str) == Some("blog.pckt.content")
    }

    fn decode(&self, content: &Value, ctx: &DecodeCtx) -> Option<RichDoc> {
        // Large content (>20 KB) is stored in an external blob with empty/absent
        // `items`. That needs a blob fetch we can't do here, so defer to the
        // `textContent` fallback by returning None.
        let items = content.get("items").and_then(Value::as_array)?;
        if items.is_empty() {
            return None;
        }
        Some(RichDoc {
            blocks: blocks(items, ctx),
        })
    }
}

fn blocks(items: &[Value], ctx: &DecodeCtx) -> Vec<Block> {
    items.iter().flat_map(|item| block(item, ctx)).collect()
}

/// Decode one block. Returns 0+ blocks: leaves yield one, degraded containers
/// (e.g. a table with no neutral equivalent) expand to their flattened text.
fn block(item: &Value, ctx: &DecodeCtx) -> Vec<Block> {
    let Some(ty) = item.get("$type").and_then(Value::as_str) else {
        return Vec::new();
    };
    // Match the suffix after `blog.pckt.block.` so minor naming drift is tolerated.
    match ty.rsplit('.').next().unwrap_or("") {
        "text" => vec![Block::Paragraph(text_block_inlines(item))],
        "heading" => {
            let level = item
                .get("level")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 6) as u8;
            vec![Block::Heading {
                level,
                content: text_block_inlines(item),
            }]
        }
        "blockquote" => vec![Block::Quote(child_blocks(item, ctx))],
        "codeBlock" => {
            let text = item
                .get("plaintext")
                .or_else(|| item.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let lang = item
                .get("language")
                .and_then(Value::as_str)
                .map(str::to_string);
            vec![Block::Code { lang, text }]
        }
        "bulletList" | "taskList" => vec![list(item, ctx, false)],
        "orderedList" => vec![list(item, ctx, true)],
        "image" => image(item, ctx).into_iter().collect(),
        "horizontalRule" => vec![Block::Rule],
        // `gallery` is a ref to a separate record (needs a fetch); `table` and other
        // unmodeled containers degrade by flattening their nested `content`.
        _ => child_blocks(item, ctx),
    }
}

/// Blocks under a container's `content` array.
fn child_blocks(item: &Value, ctx: &DecodeCtx) -> Vec<Block> {
    item.get("content")
        .and_then(Value::as_array)
        .map(|items| blocks(items, ctx))
        .unwrap_or_default()
}

/// A `{bullet,ordered,task}List` whose `content` is a list of items, each itself a
/// container of blocks. (Task-item checked state is dropped for v1.)
fn list(item: &Value, ctx: &DecodeCtx, ordered: bool) -> Block {
    let items = item
        .get("content")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| child_blocks(entry, ctx))
                .collect()
        })
        .unwrap_or_default();
    Block::List { ordered, items }
}

/// An image block — its blob and alt nest under `attrs`.
fn image(item: &Value, ctx: &DecodeCtx) -> Option<Block> {
    let attrs = item.get("attrs")?;
    let alt = attrs.get("alt").and_then(Value::as_str).unwrap_or("");
    blob_image(attrs.get("blob")?, ctx.repo_did, alt).map(Block::Image)
}
