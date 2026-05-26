//! Pckt — `blog.pckt.content`: a flat `items` list of `blog.pckt.block.*`.
//!
//! The live record I validated against is trivial (a single unstyled text block), and
//! the published survey disagrees with it on field names (`text` vs the real `plaintext`)
//! and on the rich-text model (run-level `marks` vs byte-range `facets`). So the text
//! path here accepts **either** mechanism: byte-range `facets` (shared shape) if present,
//! else run-level `marks` applied across the whole run. Confirm against a richly
//! formatted record when one turns up.

use serde_json::Value;

use super::facets::{Facet, FacetKind, apply_facets, default_facet_kind, parse_facets};
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

        let mut blocks = Vec::new();
        for item in items {
            let Some(ty) = item.get("$type").and_then(Value::as_str) else {
                continue;
            };
            // Match on the suffix after `blog.pckt.block.` so minor naming drift is tolerated.
            match ty.rsplit('.').next() {
                Some("text") => blocks.push(Block::Paragraph(text_inlines(item))),
                Some("heading") => {
                    let level = item
                        .get("level")
                        .and_then(Value::as_u64)
                        .unwrap_or(1)
                        .clamp(1, 6) as u8;
                    blocks.push(Block::Heading {
                        level,
                        content: text_inlines(item),
                    });
                }
                Some("blockquote") => {
                    blocks.push(Block::Quote(vec![Block::Paragraph(text_inlines(item))]))
                }
                Some("codeBlock") | Some("code") => {
                    let text = field_str(item, &["plaintext", "text", "code"]).to_string();
                    let lang = item
                        .get("language")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    blocks.push(Block::Code { lang, text });
                }
                Some("image") => {
                    let blob = item.get("image").or_else(|| item.get("blob"));
                    if let Some(img) = blob.and_then(|b| blob_image(b, ctx.repo_did, "")) {
                        blocks.push(Block::Image(img));
                    }
                }
                Some("horizontalRule") | Some("hr") => blocks.push(Block::Rule),
                // Galleries, tables, task lists, embeds: degrade to nothing for v1.
                _ => {}
            }
        }
        Some(RichDoc { blocks })
    }
}

/// A Pckt text block — supports either byte-range `facets` or run-level `marks`.
fn text_inlines(block: &Value) -> Vec<Inline> {
    let text = field_str(block, &["plaintext", "text"]);

    if block.get("facets").is_some() {
        return apply_facets(text, &parse_facets(block, default_facet_kind));
    }
    if let Some(marks) = block.get("marks").and_then(Value::as_array) {
        let kinds: Vec<FacetKind> = marks.iter().filter_map(default_facet_kind).collect();
        if !kinds.is_empty() {
            let whole = Facet {
                start: 0,
                end: text.len(),
                kinds,
            };
            return apply_facets(text, &[whole]);
        }
    }
    if text.is_empty() {
        Vec::new()
    } else {
        vec![Inline::Text(text.to_string())]
    }
}

/// First present, string-valued key among `keys` (tolerates field-name drift).
fn field_str<'a>(value: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|k| value.get(*k).and_then(Value::as_str))
        .unwrap_or("")
}
