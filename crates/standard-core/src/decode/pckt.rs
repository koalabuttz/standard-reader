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
use crate::model::{Block, Inline, RichDoc};

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
        "table" => vec![table(item)],
        // `gallery` carries only a `ref` to a separate `blog.pckt.gallery` record. The decoder
        // is pure (no I/O), so emit a placeholder; `read::get_document` fetches the record and
        // resolves it to an `ImageGrid` (the same two-phase pattern as `#contentRef`).
        "gallery" => item
            .get("ref")
            .and_then(Value::as_str)
            .map(|uri| Block::GalleryRef { uri: uri.to_string() })
            .into_iter()
            .collect(),
        // Other unmodeled containers degrade by flattening their nested `content`.
        _ => child_blocks(item, ctx),
    }
}

/// Decode a fetched `blog.pckt.gallery` record's `images` into a flat list of [`Image`]s
/// (for an [`Block::ImageGrid`]). Each entry holds a top-level `blob` (CID under `ref.$link`);
/// `did` is the gallery record's repo. Pure — the fetch happens in `read::get_document`.
pub(crate) fn gallery_images(record: &Value, did: &str) -> Vec<crate::model::Image> {
    record
        .get("images")
        .and_then(Value::as_array)
        .map(|images| {
            images
                .iter()
                .filter_map(|img| blob_image(img.get("blob")?, did, ""))
                .collect()
        })
        .unwrap_or_default()
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

/// A `table` whose `content` is rows (`tableRow`), each a list of cells
/// (`tableHeader`/`tableCell`) whose `content` is blocks. The first all-header row becomes
/// the header; each cell's blocks are flattened to inline text.
fn table(item: &Value) -> Block {
    let mut head: Vec<Vec<Inline>> = Vec::new();
    let mut rows: Vec<Vec<Vec<Inline>>> = Vec::new();

    let Some(row_vals) = item.get("content").and_then(Value::as_array) else {
        return Block::Table { head, rows };
    };
    for (i, row) in row_vals.iter().enumerate() {
        let Some(cell_vals) = row.get("content").and_then(Value::as_array) else {
            continue;
        };
        let mut all_header = !cell_vals.is_empty();
        let mut cells = Vec::with_capacity(cell_vals.len());
        for cell in cell_vals {
            if !cell
                .get("$type")
                .and_then(Value::as_str)
                .is_some_and(|t| t.ends_with("tableHeader"))
            {
                all_header = false;
            }
            cells.push(cell_inlines(cell));
        }
        if i == 0 && all_header {
            head = cells;
        } else {
            rows.push(cells);
        }
    }
    Block::Table { head, rows }
}

/// Flatten a table cell's content blocks to a single line of inlines.
fn cell_inlines(cell: &Value) -> Vec<Inline> {
    let mut out = Vec::new();
    if let Some(blocks) = cell.get("content").and_then(Value::as_array) {
        for block in blocks {
            if !out.is_empty() {
                out.push(Inline::Text(" ".into()));
            }
            out.extend(text_block_inlines(block));
        }
    }
    out
}

/// An image block — its blob and alt nest under `attrs`.
fn image(item: &Value, ctx: &DecodeCtx) -> Option<Block> {
    let attrs = item.get("attrs")?;
    let alt = attrs.get("alt").and_then(Value::as_str).unwrap_or("");
    blob_image(attrs.get("blob")?, ctx.repo_did, alt).map(Block::Image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ImageSource;

    const CTX: DecodeCtx = DecodeCtx {
        repo_did: "did:plc:test",
    };

    #[test]
    fn gallery_block_becomes_a_ref_placeholder() {
        let content = serde_json::json!({
            "$type": "blog.pckt.content",
            "items": [{
                "$type": "blog.pckt.block.gallery",
                "ref": "at://did:plc:abc/blog.pckt.gallery/3rk"
            }]
        });
        let blocks = Pckt.decode(&content, &CTX).unwrap().blocks;
        assert_eq!(
            blocks,
            vec![Block::GalleryRef {
                uri: "at://did:plc:abc/blog.pckt.gallery/3rk".into()
            }]
        );
    }

    #[test]
    fn gallery_block_without_ref_is_skipped() {
        let content = serde_json::json!({
            "$type": "blog.pckt.content",
            "items": [{ "$type": "blog.pckt.block.gallery" }]
        });
        assert!(Pckt.decode(&content, &CTX).unwrap().blocks.is_empty());
    }

    #[test]
    fn gallery_images_maps_blobs_to_blob_sources() {
        // The shape of a real `blog.pckt.gallery` record (two image entries).
        let record = serde_json::json!({
            "$type": "blog.pckt.gallery",
            "images": [
                { "src": "blob:bafa", "blob": { "$type": "blob", "ref": { "$link": "bafa" }, "mimeType": "image/png", "size": 1 } },
                { "src": "blob:bafb", "blob": { "$type": "blob", "ref": { "$link": "bafb" }, "mimeType": "image/jpeg", "size": 2 } }
            ],
            "layout": "grid"
        });
        let images = gallery_images(&record, "did:plc:owner");
        assert_eq!(images.len(), 2);
        assert!(matches!(
            &images[0].source,
            ImageSource::Blob { did, cid } if did == "did:plc:owner" && cid == "bafa"
        ));
        assert!(matches!(
            &images[1].source,
            ImageSource::Blob { did, cid } if did == "did:plc:owner" && cid == "bafb"
        ));
    }
}
