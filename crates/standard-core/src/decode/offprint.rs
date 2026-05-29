//! Offprint — `app.offprint.content`: a flat `items` list of `app.offprint.block.*`.
//!
//! Validated against a live record (`mikestevens.link`). Text blocks carry byte-range
//! facets (the shared [`facets`](super::facets) shape); images embed a blob ref.

use serde_json::Value;

use super::facets::text_block_inlines;
use super::image::blob_image;
use super::{ContentDecoder, DecodeCtx};
use crate::model::{Block, Image, Inline, RichDoc};

pub struct Offprint;

impl ContentDecoder for Offprint {
    fn handles(&self, content: &Value) -> bool {
        content.get("$type").and_then(Value::as_str) == Some("app.offprint.content")
    }

    fn decode(&self, content: &Value, ctx: &DecodeCtx) -> Option<RichDoc> {
        let items = content.get("items")?.as_array()?;
        let mut blocks = Vec::new();
        for item in items {
            // Match on the suffix after `app.offprint.block.`.
            match item
                .get("$type")
                .and_then(Value::as_str)
                .and_then(|t| t.rsplit('.').next())
            {
                Some("text") => blocks.push(Block::Paragraph(text_block_inlines(item))),
                Some("heading") => {
                    let level = item
                        .get("level")
                        .and_then(Value::as_u64)
                        .unwrap_or(2)
                        .clamp(1, 6) as u8;
                    blocks.push(Block::Heading {
                        level,
                        content: text_block_inlines(item),
                    });
                }
                Some("image") => {
                    if let Some(img) = item
                        .get("image")
                        .and_then(|b| blob_image(b, ctx.repo_did, ""))
                    {
                        blocks.push(Block::Image(img));
                    }
                }
                Some("imageGrid") | Some("imageCarousel") => {
                    // A carousel is a sequential gallery; a TUI can't swipe, so both flatten to a
                    // grid of all their images.
                    let images = grid_images(item, ctx);
                    if !images.is_empty() {
                        blocks.push(Block::ImageGrid(images));
                    }
                }
                Some("imageDiff") => {
                    // Before/after pair: render both side by side, carrying the labels as alt text.
                    let images = diff_images(item, ctx);
                    if !images.is_empty() {
                        blocks.push(Block::ImageGrid(images));
                    }
                }
                Some("bulletList") => blocks.push(Block::List {
                    ordered: false,
                    items: list_items(item),
                }),
                Some("orderedList") => blocks.push(Block::List {
                    ordered: true,
                    items: list_items(item),
                }),
                Some("taskList") => blocks.push(Block::List {
                    ordered: false,
                    items: task_items(item),
                }),
                Some("blockquote") => blocks.push(blockquote(item)),
                Some("codeBlock") => blocks.push(code_block(item)),
                // Web embeds/bookmarks: a TUI can't host an iframe, so degrade to a clickable link
                // (reusing the link machinery), keeping a bookmark's description as a trailing line.
                Some("webEmbed") => {
                    if let Some(b) = link_block(item, false) {
                        blocks.push(b);
                    }
                }
                Some("webBookmark") => {
                    if let Some(b) = link_block(item, true) {
                        blocks.push(b);
                    }
                }
                Some("callout") => blocks.push(callout(item)),
                Some("horizontalRule") => blocks.push(Block::Rule),
                // Unknown block: degrade to its text if it has any, rather than dropping it.
                _ => {
                    if item
                        .get("plaintext")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty())
                    {
                        blocks.push(Block::Paragraph(text_block_inlines(item)));
                    }
                }
            }
        }
        Some(RichDoc { blocks })
    }
}

/// `{ children: [{ content: <text block> }] }` → list items (one paragraph each). Shared by the
/// bullet and ordered lists, which differ only by the `ordered` flag.
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

/// `{ children: [{ checked, content: <text block> }] }` → list items, each prefixed with a
/// checkbox glyph (the model has no checked-state field, so the state is carried in the text).
fn task_items(block: &Value) -> Vec<Vec<Block>> {
    block
        .get("children")
        .and_then(Value::as_array)
        .map(|children| {
            children
                .iter()
                .map(|c| {
                    let mark = if c.get("checked").and_then(Value::as_bool).unwrap_or(false) {
                        "☑ "
                    } else {
                        "☐ "
                    };
                    let mut inlines = vec![Inline::Text(mark.into())];
                    if let Some(content) = c.get("content") {
                        inlines.extend(text_block_inlines(content));
                    }
                    vec![Block::Paragraph(inlines)]
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `{ content: [<text block>, …] }` → a [`Block::Quote`] of paragraphs.
fn blockquote(block: &Value) -> Block {
    let inner = block
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|it| Block::Paragraph(text_block_inlines(it)))
                .collect()
        })
        .unwrap_or_default();
    Block::Quote(inner)
}

/// `{ code, language }` → a [`Block::Code`].
fn code_block(block: &Value) -> Block {
    let text = block
        .get("code")
        .or_else(|| block.get("plaintext"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let lang = block
        .get("language")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Block::Code { lang, text }
}

/// A `webEmbed`/`webBookmark` (`{ href, title, description }`) → a paragraph holding a clickable
/// link labelled with the title (falling back to the URL). For a bookmark (`with_desc`), the
/// description follows on its own line. `None` if there's no `href`.
fn link_block(block: &Value, with_desc: bool) -> Option<Block> {
    let href = block.get("href").and_then(Value::as_str)?;
    let title = block.get("title").and_then(Value::as_str).unwrap_or("");
    let label = if title.is_empty() {
        format!("🔗 {href}")
    } else {
        format!("🔗 {title}")
    };
    let mut inlines = vec![Inline::Link {
        href: href.to_string(),
        content: vec![Inline::Text(label)],
    }];
    if with_desc
        && let Some(desc) = block
            .get("description")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    {
        inlines.push(Inline::LineBreak);
        inlines.push(Inline::Text(desc.to_string()));
    }
    Some(Block::Paragraph(inlines))
}

/// `{ images: [{ image: <blob> }], labels: [..] }` → the diff's images, each labelled (alt) by
/// its position's entry in `labels` (e.g. "Before"/"After").
fn diff_images(block: &Value, ctx: &DecodeCtx) -> Vec<Image> {
    let labels = block.get("labels").and_then(Value::as_array);
    block
        .get("images")
        .and_then(Value::as_array)
        .map(|images| {
            images
                .iter()
                .enumerate()
                .filter_map(|(i, entry)| {
                    let mut img = entry
                        .get("image")
                        .and_then(|b| blob_image(b, ctx.repo_did, ""))?;
                    if let Some(label) = labels
                        .and_then(|l| l.get(i))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        img.alt = label.to_string();
                    }
                    Some(img)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `{ images: [{ image: <blob>, aspectRatio }] }` → the grid's images.
fn grid_images(block: &Value, ctx: &DecodeCtx) -> Vec<Image> {
    block
        .get("images")
        .and_then(Value::as_array)
        .map(|images| {
            images
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("image")
                        .and_then(|b| blob_image(b, ctx.repo_did, ""))
                })
                .collect()
        })
        .unwrap_or_default()
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
