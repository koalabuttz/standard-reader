//! Offprint — `app.offprint.content`: a flat `items` list of `app.offprint.block.*`.
//!
//! Validated against a live record (`mikestevens.link`). Text blocks carry byte-range
//! facets (the shared [`facets`](super::facets) shape); images embed a blob ref.

use serde_json::Value;

use super::facets::text_block_inlines;
use super::image::blob_image;
use super::{ContentDecoder, DecodeCtx};
use crate::model::{Block, RichDoc};

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

/// `{ color, emoji, plaintext(+facets) }` → a [`Block::Callout`], preserving the emoji
/// badge and the author's tint colour (the text path keeps any richtext facets).
fn callout(block: &Value) -> Block {
    let emoji = block
        .get("emoji")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let tint = block
        .get("color")
        .and_then(Value::as_str)
        .and_then(parse_css_rgb);
    Block::Callout {
        emoji,
        tint,
        content: text_block_inlines(block),
    }
}

/// Parse the RGB channels from a CSS colour like `rgb(168 85 247 / 0.2)` or
/// `rgb(168,85,247)` — the first three integers (alpha is ignored; the reader applies
/// its own subtle tint opacity).
fn parse_css_rgb(s: &str) -> Option<(u8, u8, u8)> {
    let nums: Vec<u8> = s
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| !t.is_empty())
        .take(3)
        .map(|t| t.parse::<u16>().unwrap_or(0).min(255) as u8)
        .collect();
    match nums[..] {
        [r, g, b] => Some((r, g, b)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_css_rgb;

    #[test]
    fn parses_css_rgb_with_alpha() {
        assert_eq!(parse_css_rgb("rgb(168 85 247 / 0.2)"), Some((168, 85, 247)));
        assert_eq!(parse_css_rgb("rgb(10,20,30)"), Some((10, 20, 30)));
        assert_eq!(parse_css_rgb("nope"), None);
    }
}
