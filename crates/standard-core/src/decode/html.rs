//! WordPress — `org.wordpress.html`: `{ html: "<rendered HTML>" }`.
//!
//! The simplest format: rendered `the_content` output, like RSS `content:encoded`.
//! We walk the DOM (`tl`, a lean pure-Rust parser) into the neutral [`RichDoc`]. Tags
//! with no neutral equivalent (tables, iframes, …) flatten to their children rather
//! than panicking — the decode contract.

use serde_json::Value;
use tl::{Node, NodeHandle, Parser};

use super::image::url_image;
use super::{ContentDecoder, DecodeCtx};
use crate::model::{Block, Image, Inline, PublishingPlatform, RichDoc};

pub struct Wordpress;

impl ContentDecoder for Wordpress {
    fn handles(&self, content: &Value) -> bool {
        content.get("$type").and_then(Value::as_str) == Some("org.wordpress.html")
    }

    fn decode(&self, content: &Value, _ctx: &DecodeCtx) -> Option<RichDoc> {
        let html = content.get("html")?.as_str()?;
        let dom = tl::parse(html, tl::ParserOptions::default()).ok()?;
        let parser = dom.parser();
        let mut blocks = Vec::new();
        children_to_blocks(dom.children(), parser, &mut blocks);
        Some(RichDoc { blocks })
    }

    fn publishing_platform(&self, _content: &Value) -> Option<PublishingPlatform> {
        Some(PublishingPlatform::Wordpress)
    }
}

/// Walk a run of sibling nodes, emitting block elements and gathering loose inline
/// content (bare text, `<strong>`, …) into paragraphs.
fn children_to_blocks(handles: &[NodeHandle], parser: &Parser, out: &mut Vec<Block>) {
    let mut inline_buf: Vec<Inline> = Vec::new();
    for h in handles {
        let Some(node) = h.get(parser) else { continue };
        match node {
            Node::Tag(tag) => {
                let name = tag.name().as_utf8_str();
                if is_block_tag(&name) {
                    flush_paragraph(&mut inline_buf, out);
                    block_from_tag(&name, h, parser, out);
                } else {
                    inline_from_tag(&name, h, parser, &mut inline_buf);
                }
            }
            Node::Raw(bytes) => {
                let text = decode_entities(&bytes.as_utf8_str());
                if !text.is_empty() {
                    inline_buf.push(Inline::Text(text));
                }
            }
            Node::Comment(_) => {}
        }
    }
    flush_paragraph(&mut inline_buf, out);
}

fn flush_paragraph(buf: &mut Vec<Inline>, out: &mut Vec<Block>) {
    if buf.iter().any(|i| !is_blank(i)) {
        out.push(Block::Paragraph(std::mem::take(buf)));
    } else {
        buf.clear();
    }
}

fn is_blank(inline: &Inline) -> bool {
    matches!(inline, Inline::Text(t) if t.trim().is_empty())
}

fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "blockquote"
            | "pre"
            | "hr"
            | "img"
            | "figure"
            | "div"
            | "section"
            | "article"
            | "main"
            | "table"
    )
}

fn block_from_tag(name: &str, handle: &NodeHandle, parser: &Parser, out: &mut Vec<Block>) {
    let Some(tag) = handle.get(parser).and_then(Node::as_tag) else {
        return;
    };
    let children = tag.children();
    let kids = children.top().as_slice();

    match name {
        "p" => {
            let inlines = inline_children(kids, parser);
            if inlines.iter().any(|i| !is_blank(i)) {
                out.push(Block::Paragraph(inlines));
            }
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = name[1..].parse().unwrap_or(1);
            out.push(Block::Heading {
                level,
                content: inline_children(kids, parser),
            });
        }
        "blockquote" => {
            let mut inner = Vec::new();
            children_to_blocks(kids, parser, &mut inner);
            out.push(Block::Quote(inner));
        }
        "ul" | "ol" => out.push(list(kids, parser, name == "ol")),
        "pre" => out.push(Block::Code {
            lang: code_lang(tag, parser),
            text: tag_text(tag, parser),
        }),
        "hr" => out.push(Block::Rule),
        "img" => {
            if let Some(img) = img_from_tag(tag) {
                out.push(Block::Image(img));
            }
        }
        // Transparent containers (and unmodeled blocks like <table>): recurse.
        _ => children_to_blocks(kids, parser, out),
    }
}

fn list(item_handles: &[NodeHandle], parser: &Parser, ordered: bool) -> Block {
    let mut items = Vec::new();
    for h in item_handles {
        let Some(tag) = h.get(parser).and_then(Node::as_tag) else {
            continue;
        };
        if tag.name().as_utf8_str() != "li" {
            continue;
        }
        let children = tag.children();
        let mut blocks = Vec::new();
        children_to_blocks(children.top().as_slice(), parser, &mut blocks);
        items.push(blocks);
    }
    Block::List { ordered, items }
}

fn inline_children(handles: &[NodeHandle], parser: &Parser) -> Vec<Inline> {
    let mut out = Vec::new();
    for h in handles {
        match h.get(parser) {
            Some(Node::Raw(bytes)) => {
                let text = decode_entities(&bytes.as_utf8_str());
                if !text.is_empty() {
                    out.push(Inline::Text(text));
                }
            }
            Some(Node::Tag(tag)) => inline_from_tag(&tag.name().as_utf8_str(), h, parser, &mut out),
            _ => {}
        }
    }
    out
}

fn inline_from_tag(name: &str, handle: &NodeHandle, parser: &Parser, out: &mut Vec<Inline>) {
    let Some(tag) = handle.get(parser).and_then(Node::as_tag) else {
        return;
    };
    let children = tag.children();
    let kids = children.top().as_slice();

    match name {
        "strong" | "b" => out.push(Inline::Strong(inline_children(kids, parser))),
        "em" | "i" => out.push(Inline::Emphasis(inline_children(kids, parser))),
        "del" | "s" | "strike" => out.push(Inline::Strike(inline_children(kids, parser))),
        "code" => out.push(Inline::Code(tag_text(tag, parser))),
        "a" => {
            let href = attr(tag, "href").unwrap_or_default();
            out.push(Inline::Link {
                href,
                content: inline_children(kids, parser),
            });
        }
        "br" => out.push(Inline::LineBreak),
        "img" => {
            if let Some(img) = img_from_tag(tag) {
                out.push(Inline::Image(img));
            }
        }
        // span / unknown inline wrappers: flatten to their children.
        _ => out.extend(inline_children(kids, parser)),
    }
}

fn img_from_tag(tag: &tl::HTMLTag) -> Option<Image> {
    let src = attr(tag, "src")?;
    let alt = attr(tag, "alt").unwrap_or_default();
    Some(url_image(&src, &alt))
}

/// `<pre><code class="language-rust">` → `Some("rust")`.
fn code_lang(pre: &tl::HTMLTag, parser: &Parser) -> Option<String> {
    let children = pre.children();
    for h in children.top().as_slice() {
        if let Some(code) = h.get(parser).and_then(Node::as_tag)
            && code.name().as_utf8_str() == "code"
        {
            let class = attr(code, "class")?;
            return class
                .split_whitespace()
                .find_map(|c| c.strip_prefix("language-"))
                .map(str::to_string);
        }
    }
    None
}

/// Entity-decoded inner text of a tag (for `<code>`/`<pre>`).
fn tag_text(tag: &tl::HTMLTag, parser: &Parser) -> String {
    decode_entities(&tag.inner_text(parser))
}

fn attr(tag: &tl::HTMLTag, key: &str) -> Option<String> {
    tag.attributes()
        .get(key)
        .flatten()
        .map(|b| b.as_utf8_str().to_string())
}

/// Decode the HTML entities WordPress actually emits (named common set + numeric).
/// Unrecognized `&…;` sequences are left as a literal `&`.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        if let Some(semi) = after[1..].find(';').map(|p| p + 1)
            && let Some(decoded) = resolve_entity(&after[1..semi])
        {
            out.push_str(&decoded);
            rest = &after[semi + 1..];
            continue;
        }
        out.push('&');
        rest = &after[1..];
    }
    out.push_str(rest);
    out
}

fn resolve_entity(entity: &str) -> Option<String> {
    if entity.is_empty() || entity.len() > 12 || entity.contains('&') {
        return None;
    }
    if let Some(num) = entity.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(code).map(|c| c.to_string());
    }
    let ch = match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "ldquo" => "“",
        "rdquo" => "”",
        "lsquo" => "‘",
        "rsquo" => "’",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        _ => return None,
    };
    Some(ch.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ImageSource;

    fn decode(html: &str) -> Vec<Block> {
        let content = serde_json::json!({ "$type": "org.wordpress.html", "html": html });
        Wordpress
            .decode(
                &content,
                &DecodeCtx {
                    repo_did: "did:plc:test",
                },
            )
            .unwrap()
            .blocks
    }

    #[test]
    fn paragraph_with_inline_styles_and_link() {
        let blocks =
            decode(r#"<p>Hello <strong>world</strong> &amp; <a href="https://x.test/">x</a>.</p>"#);
        assert_eq!(
            blocks,
            vec![Block::Paragraph(vec![
                Inline::Text("Hello ".into()),
                Inline::Strong(vec![Inline::Text("world".into())]),
                Inline::Text(" & ".into()),
                Inline::Link {
                    href: "https://x.test/".into(),
                    content: vec![Inline::Text("x".into())]
                },
                Inline::Text(".".into()),
            ])]
        );
    }

    #[test]
    fn headings_lists_and_rule() {
        let blocks = decode("<h2>Title</h2><ul><li>one</li><li>two</li></ul><hr>");
        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[0],
            Block::Heading {
                level: 2,
                content: vec![Inline::Text("Title".into())]
            }
        );
        assert!(matches!(&blocks[1], Block::List { ordered: false, items } if items.len() == 2));
        assert_eq!(blocks[2], Block::Rule);
    }

    #[test]
    fn img_becomes_url_image() {
        let blocks = decode(r#"<figure><img src="https://img.test/a.png" alt="a pic"></figure>"#);
        assert_eq!(
            blocks,
            vec![Block::Image(Image {
                alt: "a pic".into(),
                source: ImageSource::Url("https://img.test/a.png".into()),
            })]
        );
    }

    #[test]
    fn pre_code_with_language_class() {
        let blocks = decode(r#"<pre><code class="language-rust">fn main() {}</code></pre>"#);
        assert_eq!(
            blocks,
            vec![Block::Code {
                lang: Some("rust".into()),
                text: "fn main() {}".into()
            }]
        );
    }

    #[test]
    fn unknown_tags_flatten_not_panic() {
        let blocks = decode("<table><tr><td>cell text</td></tr></table>");
        // No Table block in the model: the text survives as a paragraph.
        assert_eq!(
            blocks,
            vec![Block::Paragraph(vec![Inline::Text("cell text".into())])]
        );
    }
}
