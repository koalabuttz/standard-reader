//! Offprint — `app.offprint.content`: a flat `items` list of `app.offprint.block.*`.
//!
//! Validated against a live record (`mikestevens.link`). Text blocks carry byte-range
//! facets (the shared [`facets`](super::facets) shape); images embed a blob ref.

use serde_json::Value;

use super::facets::{parse_css_rgb, text_block_inlines};
use super::image::blob_image;
use super::{ContentDecoder, DecodeCtx};
use crate::model::{Align, Block, Image, Inline, RichDoc};

pub struct Offprint;

impl ContentDecoder for Offprint {
    fn handles(&self, content: &Value) -> bool {
        content.get("$type").and_then(Value::as_str) == Some("app.offprint.content")
    }

    fn decode(&self, content: &Value, ctx: &DecodeCtx) -> Option<RichDoc> {
        let items = content.get("items")?.as_array()?;
        let mut blocks = Vec::new();
        for item in items {
            // Most block kinds carry a horizontal alignment; wrap their result when it's not the
            // default (left), so the reader can honor it without touching every other decoder.
            let align = read_align(item);
            // Match on the suffix after `app.offprint.block.`.
            match item
                .get("$type")
                .and_then(Value::as_str)
                .and_then(|t| t.rsplit('.').next())
            {
                Some("text") => blocks.push(wrap_align(
                    Block::Paragraph(text_block_inlines(item)),
                    align,
                )),
                Some("heading") => {
                    let level = item
                        .get("level")
                        .and_then(Value::as_u64)
                        .unwrap_or(2)
                        .clamp(1, 6) as u8;
                    blocks.push(wrap_align(
                        Block::Heading {
                            level,
                            content: text_block_inlines(item),
                        },
                        align,
                    ));
                }
                Some("image") => {
                    if let Some(img) = item
                        .get("image")
                        .and_then(|b| blob_image(b, ctx.repo_did, ""))
                    {
                        blocks.push(wrap_image_align(Block::Image(img), item));
                    }
                }
                Some("imageGrid") | Some("imageCarousel") => {
                    // A carousel is a sequential gallery; a TUI can't swipe, so both flatten to a
                    // grid of all their images.
                    let images = grid_images(item, ctx);
                    if !images.is_empty() {
                        blocks.push(wrap_image_align(Block::ImageGrid(images), item));
                    }
                }
                Some("imageDiff") => {
                    // Before/after pair: render both side by side, carrying the labels as alt text.
                    let images = diff_images(item, ctx);
                    if !images.is_empty() {
                        blocks.push(wrap_image_align(Block::ImageGrid(images), item));
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
                        blocks.push(wrap_align(b, align));
                    }
                }
                Some("webBookmark") => {
                    if let Some(b) = link_block(item, true) {
                        blocks.push(wrap_align(b, align));
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

/// A block's horizontal alignment, from `textAlign` (text/heading) or `alignment`
/// (image/embed). Anything but `center`/`right` (incl. absent) is the default left.
fn read_align(item: &Value) -> Align {
    match item
        .get("textAlign")
        .or_else(|| item.get("alignment"))
        .and_then(Value::as_str)
    {
        Some("center") => Align::Center,
        Some("right") => Align::Right,
        _ => Align::Left,
    }
}

/// Wrap `block` in [`Block::Aligned`] when it isn't left-aligned; left stays the bare block (so the
/// common case adds no wrapper and other decoders/fixtures are unaffected). For **text**, where the
/// reader's default is already left.
fn wrap_align(block: Block, align: Align) -> Block {
    match align {
        Align::Left => block,
        _ => Block::Aligned {
            align,
            content: Box::new(block),
        },
    }
}

/// Wrap an **image-like** block by its explicit `alignment` — *including* left, because the
/// reader's default for a bare image is *centered*, so honoring "left" requires an explicit wrapper
/// to override that. An absent `alignment` (e.g. a Markdown image) stays bare → centered.
fn wrap_image_align(block: Block, item: &Value) -> Block {
    let align = match item.get("alignment").and_then(Value::as_str) {
        Some("left") => Align::Left,
        Some("center") => Align::Center,
        Some("right") => Align::Right,
        _ => return block, // absent/unknown → bare (reader centers)
    };
    Block::Aligned {
        align,
        content: Box::new(block),
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
