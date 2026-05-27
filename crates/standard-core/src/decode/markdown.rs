//! Markdown — two entry points onto one pipeline:
//!
//! - a **bare string** `content` (GreenGale, Sequoia, markpub static blogs — and the
//!   body of a GreenGale `app.greengale.document` resolved via a `#contentRef`), and
//! - `at.markpub.markdown`, a typed wrapper whose markdown lives at `text.markdown`.
//!
//! Both feed `pulldown-cmark`, whose flat event stream we fold into [`RichDoc`] with a
//! small container stack. Images carry full URLs (GreenGale emits resolved `getBlob`
//! URLs), so they map to [`ImageSource::Url`].

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag};
use serde_json::Value;

use super::image::url_image;
use super::{ContentDecoder, DecodeCtx};
use crate::model::{Block, Image, Inline, RichDoc};

pub struct Markdown;

impl ContentDecoder for Markdown {
    fn handles(&self, content: &Value) -> bool {
        content.is_string()
            || content.get("$type").and_then(Value::as_str) == Some("at.markpub.markdown")
    }

    fn decode(&self, content: &Value, _ctx: &DecodeCtx) -> Option<RichDoc> {
        let md = if let Some(s) = content.as_str() {
            s
        } else {
            // markpub wrapper: unwrap text.markdown.
            content
                .get("text")
                .and_then(|t| t.get("markdown"))
                .and_then(Value::as_str)?
        };
        Some(from_markdown(md))
    }
}

/// Parse a markdown string into a [`RichDoc`].
pub fn from_markdown(md: &str) -> RichDoc {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let mut builder = Builder::new();
    for event in Parser::new_ext(md, options) {
        builder.handle(event);
    }
    RichDoc {
        blocks: builder.finish(),
    }
}

/// An in-progress container. Block containers collect `Block`s; inline containers
/// collect `Inline`s; leaves accumulate a string.
enum Frame {
    Root(Vec<Block>),
    Quote(Vec<Block>),
    Item(Vec<Block>),
    List {
        ordered: bool,
        items: Vec<Vec<Block>>,
    },
    Paragraph(Vec<Inline>),
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Code {
        lang: Option<String>,
        text: String,
    },
    Strong(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strike(Vec<Inline>),
    Link {
        href: String,
        content: Vec<Inline>,
    },
    Image {
        dest: String,
        alt: String,
    },
}

struct Builder {
    stack: Vec<Frame>,
}

impl Builder {
    fn new() -> Self {
        Self {
            stack: vec![Frame::Root(Vec::new())],
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(_) => self.end(),
            Event::Text(t) => self.text(&t),
            Event::Code(t) => self.push_inline(Inline::Code(t.into_string())),
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.push_inline(Inline::LineBreak),
            Event::Rule => self.push_block(Block::Rule),
            // Raw HTML, footnotes, task markers, math: ignored for v1.
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        let frame = match tag {
            Tag::Paragraph => Frame::Paragraph(Vec::new()),
            Tag::Heading { level, .. } => Frame::Heading {
                level: heading_level(level),
                content: Vec::new(),
            },
            Tag::BlockQuote(_) => Frame::Quote(Vec::new()),
            Tag::CodeBlock(kind) => Frame::Code {
                lang: code_lang(kind),
                text: String::new(),
            },
            Tag::List(start) => Frame::List {
                ordered: start.is_some(),
                items: Vec::new(),
            },
            Tag::Item => Frame::Item(Vec::new()),
            Tag::Strong => Frame::Strong(Vec::new()),
            Tag::Emphasis => Frame::Emphasis(Vec::new()),
            Tag::Strikethrough => Frame::Strike(Vec::new()),
            Tag::Link { dest_url, .. } => Frame::Link {
                href: dest_url.into_string(),
                content: Vec::new(),
            },
            Tag::Image { dest_url, .. } => Frame::Image {
                dest: dest_url.into_string(),
                alt: String::new(),
            },
            // Tables/other (not enabled) would land here; treat as transparent.
            _ => Frame::Paragraph(Vec::new()),
        };
        self.stack.push(frame);
    }

    fn end(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        match frame {
            Frame::Paragraph(content) => self.push_block(paragraph_block(content)),
            Frame::Heading { level, content } => self.push_block(Block::Heading { level, content }),
            Frame::Quote(blocks) => self.push_block(Block::Quote(blocks)),
            Frame::Code { lang, text } => {
                let text = text.strip_suffix('\n').unwrap_or(&text).to_string();
                self.push_block(Block::Code { lang, text });
            }
            Frame::List { ordered, items } => self.push_block(Block::List { ordered, items }),
            Frame::Item(blocks) => self.push_item(blocks),
            Frame::Strong(content) => self.push_inline(Inline::Strong(content)),
            Frame::Emphasis(content) => self.push_inline(Inline::Emphasis(content)),
            Frame::Strike(content) => self.push_inline(Inline::Strike(content)),
            Frame::Link { href, content } => self.push_inline(Inline::Link { href, content }),
            Frame::Image { dest, alt } => self.push_inline(Inline::Image(url_image(&dest, &alt))),
            // The root should never be popped by a stray End.
            Frame::Root(blocks) => self.stack.push(Frame::Root(blocks)),
        }
    }

    fn text(&mut self, s: &str) {
        match self.stack.last_mut() {
            Some(Frame::Code { text, .. }) => text.push_str(s),
            Some(Frame::Image { alt, .. }) => alt.push_str(s),
            _ => self.push_inline(Inline::Text(s.to_string())),
        }
    }

    fn push_inline(&mut self, inline: Inline) {
        match self.stack.last_mut() {
            Some(
                Frame::Paragraph(c)
                | Frame::Heading { content: c, .. }
                | Frame::Strong(c)
                | Frame::Emphasis(c)
                | Frame::Strike(c)
                | Frame::Link { content: c, .. },
            ) => c.push(inline),
            // Inline arriving in a block context (loose text): wrap in a paragraph.
            _ => self.push_block(Block::Paragraph(vec![inline])),
        }
    }

    fn push_block(&mut self, block: Block) {
        if let Some(Frame::Root(b) | Frame::Quote(b) | Frame::Item(b)) = self.stack.last_mut() {
            b.push(block)
        }
    }

    fn push_item(&mut self, blocks: Vec<Block>) {
        if let Some(Frame::List { items, .. }) = self.stack.last_mut() {
            items.push(blocks);
        }
    }

    fn finish(mut self) -> Vec<Block> {
        // Close any frames left open by malformed input.
        while self.stack.len() > 1 {
            self.end();
        }
        match self.stack.pop() {
            Some(Frame::Root(blocks)) => blocks,
            _ => Vec::new(),
        }
    }
}

/// Build a paragraph block — but promote an **image-only** paragraph (Markdown `![](…)`, which
/// `pulldown-cmark` emits as an inline image inside a paragraph) to a block-level image so the
/// frontend fetches and renders it. One image → [`Block::Image`]; several on adjacent lines →
/// [`Block::ImageGrid`]. Anything with real text stays a paragraph (the inline image degrades
/// to its alt text — true intra-paragraph image rendering is out of scope).
fn paragraph_block(content: Vec<Inline>) -> Block {
    let is_filler = |i: &Inline| {
        matches!(i, Inline::LineBreak) || matches!(i, Inline::Text(t) if t.trim().is_empty())
    };
    let images: Vec<Image> = content
        .iter()
        .filter_map(|i| match i {
            Inline::Image(img) => Some(img.clone()),
            _ => None,
        })
        .collect();
    let only_images = !images.is_empty()
        && content
            .iter()
            .all(|i| matches!(i, Inline::Image(_)) || is_filler(i));
    match (only_images, images.len()) {
        (false, _) => Block::Paragraph(content),
        (true, 1) => Block::Image(images.into_iter().next().unwrap()),
        (true, _) => Block::ImageGrid(images),
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn code_lang(kind: CodeBlockKind) -> Option<String> {
    match kind {
        CodeBlockKind::Fenced(info) => {
            let lang = info.split_whitespace().next().unwrap_or("");
            (!lang.is_empty()).then(|| lang.to_string())
        }
        CodeBlockKind::Indented => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_str(md: &str) -> Vec<Block> {
        let content = Value::String(md.to_string());
        Markdown
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
    fn headings_emphasis_and_strike() {
        let blocks = decode_str("# Title\n\nsome **bold** and _it_ and ~~no~~");
        assert_eq!(
            blocks[0],
            Block::Heading {
                level: 1,
                content: vec![Inline::Text("Title".into())]
            }
        );
        assert_eq!(
            blocks[1],
            Block::Paragraph(vec![
                Inline::Text("some ".into()),
                Inline::Strong(vec![Inline::Text("bold".into())]),
                Inline::Text(" and ".into()),
                Inline::Emphasis(vec![Inline::Text("it".into())]),
                Inline::Text(" and ".into()),
                Inline::Strike(vec![Inline::Text("no".into())]),
            ])
        );
    }

    #[test]
    fn lists_quote_code_and_image() {
        let blocks = decode_str(
            "- one\n- two\n\n> quoted\n\n```rust\nfn x() {}\n```\n\n![cap](https://i.test/a.png)",
        );
        assert!(matches!(&blocks[0], Block::List { ordered: false, items } if items.len() == 2));
        assert_eq!(
            blocks[1],
            Block::Quote(vec![Block::Paragraph(vec![Inline::Text("quoted".into())])])
        );
        assert_eq!(
            blocks[2],
            Block::Code {
                lang: Some("rust".into()),
                text: "fn x() {}".into()
            }
        );
        // An image-only paragraph is promoted to a block image (so the frontend renders it).
        assert_eq!(
            blocks[3],
            Block::Image(crate::model::Image {
                alt: "cap".into(),
                source: crate::model::ImageSource::Url("https://i.test/a.png".into()),
            })
        );
    }

    #[test]
    fn image_only_paragraphs_become_block_images() {
        // Standalone image → Block::Image.
        assert!(matches!(
            decode_str("![a](https://i.test/a.png)").as_slice(),
            [Block::Image(_)]
        ));
        // Two images on adjacent lines (one paragraph) → an ImageGrid.
        assert!(matches!(
            decode_str("![a](https://i.test/a.png)\n![b](https://i.test/b.png)").as_slice(),
            [Block::ImageGrid(imgs)] if imgs.len() == 2
        ));
        // An image with real text stays a paragraph (degrades to alt text inline).
        assert!(matches!(
            decode_str("see ![a](https://i.test/a.png) here").as_slice(),
            [Block::Paragraph(_)]
        ));
    }

    #[test]
    fn markpub_wrapper_is_unwrapped() {
        let content = serde_json::json!({
            "$type": "at.markpub.markdown",
            "text": { "$type": "at.markpub.text", "markdown": "## Hi" },
            "flavor": "gfm"
        });
        let doc = Markdown
            .decode(
                &content,
                &DecodeCtx {
                    repo_did: "did:plc:test",
                },
            )
            .unwrap();
        assert_eq!(
            doc.blocks,
            vec![Block::Heading {
                level: 2,
                content: vec![Inline::Text("Hi".into())]
            }]
        );
    }
}
