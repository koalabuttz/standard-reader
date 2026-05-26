//! Content decoding: every publisher's `content` representation → one [`RichDoc`].
//!
//! `site.standard.document.content` is an **open union** — each platform embeds its
//! own lexicon (confirmed from real records):
//!
//! | `content.$type`        | shape                                  | decoder      |
//! |------------------------|----------------------------------------|--------------|
//! | *(bare string)*        | Markdown (GreenGale, Sequoia, markpub) | [`Markdown`] |
//! | `pub.leaflet.*`        | blocks + facets                        | [`Leaflet`]  |
//! | `blog.pckt.content`    | `items: [blog.pckt.block.*]`           | [`Pckt`]     |
//! | *(unknown / absent)*   | fall back to `textContent`             | [`Plaintext`]|
//!
//! Adding a platform = one new [`ContentDecoder`]; nothing else changes.

use serde_json::Value;

use crate::model::{Block, Inline, RichDoc};

/// Decodes one publisher's `content` value into the neutral [`RichDoc`].
///
/// Implementations are **pure**: no I/O, no platform assumptions.
pub trait ContentDecoder {
    /// The `content.$type` NSID this decoder claims (e.g. `"blog.pckt.content"`),
    /// or `None` if it handles bare-string (Markdown) content.
    fn handles(&self) -> Option<&'static str>;

    /// Decode, or return `None` to defer to the next decoder / the fallback.
    fn decode(&self, content: &Value) -> Option<RichDoc>;
}

/// Ordered set of decoders plus the always-on `textContent` fallback.
pub struct Registry {
    decoders: Vec<Box<dyn ContentDecoder + Send + Sync>>,
}

impl Registry {
    /// The default decoder set. (Markdown/Leaflet/Pckt are stubs for now and fall
    /// through to the plaintext fallback until implemented.)
    pub fn with_defaults() -> Self {
        Self {
            decoders: vec![Box::new(Markdown), Box::new(Leaflet), Box::new(Pckt)],
        }
    }

    /// Decode `content` (the union value, if present) into a [`RichDoc`], falling
    /// back to a typeset rendering of `text_content` when no decoder applies.
    pub fn decode(&self, content: Option<&Value>, text_content: Option<&str>) -> RichDoc {
        if let Some(value) = content {
            let ty = value.get("$type").and_then(Value::as_str);
            for d in &self.decoders {
                let matches = match (d.handles(), ty) {
                    (Some(h), Some(t)) => h == t,
                    (None, None) => value.is_string(), // bare-string Markdown
                    _ => false,
                };
                if matches {
                    if let Some(doc) = d.decode(value) {
                        return doc;
                    }
                }
            }
        }
        Plaintext.from_text(text_content.unwrap_or_default())
    }
}

/// Fallback: split flat `textContent` into paragraphs on blank lines. Always works.
pub struct Plaintext;

impl Plaintext {
    pub fn from_text(&self, text: &str) -> RichDoc {
        let blocks = text
            .split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| Block::Paragraph(vec![Inline::Text(p.to_string())]))
            .collect();
        RichDoc { blocks }
    }
}

// --- Stubs: implemented next, fall through to the fallback until then. ---

/// Markdown string content (GreenGale, Sequoia static blogs, markpub.at).
pub struct Markdown;
impl ContentDecoder for Markdown {
    fn handles(&self) -> Option<&'static str> {
        None
    }
    fn decode(&self, _content: &Value) -> Option<RichDoc> {
        None // TODO: pulldown-cmark → RichDoc
    }
}

/// Leaflet's `pub.leaflet.*` blocks + facets.
pub struct Leaflet;
impl ContentDecoder for Leaflet {
    fn handles(&self) -> Option<&'static str> {
        Some("pub.leaflet.pages.linearDocument")
    }
    fn decode(&self, _content: &Value) -> Option<RichDoc> {
        None // TODO: blocks + byte-range facets → RichDoc
    }
}

/// Pckt's `blog.pckt.content` → `items: [blog.pckt.block.*]`.
pub struct Pckt;
impl ContentDecoder for Pckt {
    fn handles(&self) -> Option<&'static str> {
        Some("blog.pckt.content")
    }
    fn decode(&self, _content: &Value) -> Option<RichDoc> {
        None // TODO: block items → RichDoc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_fallback_splits_paragraphs() {
        let doc = Registry::with_defaults().decode(None, Some("hello world\n\nsecond para"));
        assert_eq!(doc.blocks.len(), 2);
    }

    #[test]
    fn pckt_content_falls_back_to_text_until_decoder_lands() {
        let content = serde_json::json!({
            "$type": "blog.pckt.content",
            "items": [{ "$type": "blog.pckt.block.text", "plaintext": "test" }]
        });
        let doc = Registry::with_defaults().decode(Some(&content), Some("test"));
        assert_eq!(doc.blocks, vec![Block::Paragraph(vec![Inline::Text("test".into())])]);
    }
}
