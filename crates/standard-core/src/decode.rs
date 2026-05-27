//! Content decoding: every publisher's `content` representation → one [`RichDoc`].
//!
//! `site.standard.document.content` is an **open union** — each platform embeds its own
//! lexicon. Shapes below were validated against live records (the published survey had
//! several wrong field names):
//!
//! | `content.$type`                    | shape                                   | decoder       |
//! |------------------------------------|-----------------------------------------|---------------|
//! | *(bare string)* / `at.markpub.markdown` | Markdown (GreenGale body, Sequoia, markpub) | [`Markdown`]  |
//! | `pub.leaflet.content`              | `pages[].blocks[].block` + facets       | [`Leaflet`]   |
//! | `blog.pckt.content`                | `items: [blog.pckt.block.*]`            | [`Pckt`]      |
//! | `app.offprint.content`             | `items: [app.offprint.block.*]` + facets | [`Offprint`] |
//! | `org.wordpress.html`               | `{ html }` (rendered HTML)              | [`Wordpress`] |
//! | `at.unthread.content`              | `{ content }` — a Markdown string       | [`Unthread`]  |
//! | `*#contentRef`                     | reference to another record (GreenGale) | [`content_ref`] (two-phase) |
//! | *(unknown / absent)*               | fall back to `textContent`              | [`Plaintext`] |
//!
//! Adding a platform = one new [`ContentDecoder`] in its own `decode/<name>.rs` plus one
//! line in [`Registry::with_defaults`]; the three block formats share [`facets`]. A Pckt
//! `gallery` block is itself a *reference* to a separate record — the decoder emits a
//! [`crate::model::Block::GalleryRef`] placeholder that [`crate::read::get_document`] resolves
//! to an [`crate::model::Block::ImageGrid`] (the `#contentRef` pattern, at block granularity).

use serde_json::Value;

use crate::atp::AtUri;
use crate::model::{Block, Inline, RichDoc};

pub mod facets;
pub mod image;

mod html;
mod leaflet;
mod markdown;
mod offprint;
mod pckt;
mod unthread;

pub use html::Wordpress;
pub use leaflet::Leaflet;
pub use markdown::Markdown;
pub use offprint::Offprint;
pub use pckt::Pckt;
pub use unthread::Unthread;

// Resolving a Pckt `gallery` block's referenced record into images is decoder-specific but
// driven from the read layer (which does the fetch); expose just that helper, not the module.
pub(crate) use pckt::gallery_images;

/// Context a decoder needs beyond the `content` value itself.
pub struct DecodeCtx<'a> {
    /// DID of the repo that owns the record. Needed to build [`ImageSource::Blob`] refs,
    /// since block lexicons embed only the blob CID, not the owning DID.
    ///
    /// [`ImageSource::Blob`]: crate::model::ImageSource::Blob
    pub repo_did: &'a str,
}

/// Decodes one publisher's `content` value into the neutral [`RichDoc`].
///
/// Implementations are **pure**: no I/O, no platform assumptions. Decode returns `None`
/// to defer (to the next decoder, then the `textContent` fallback) and must never panic
/// on partial or unexpected input.
pub trait ContentDecoder {
    /// Whether this decoder claims `content` (typically a `$type` check).
    fn handles(&self, content: &Value) -> bool;

    /// Decode, or return `None` to defer.
    fn decode(&self, content: &Value, ctx: &DecodeCtx) -> Option<RichDoc>;
}

/// Ordered set of decoders plus the always-on `textContent` fallback.
pub struct Registry {
    decoders: Vec<Box<dyn ContentDecoder + Send + Sync>>,
}

impl Registry {
    /// The default decoder set, in dispatch order.
    pub fn with_defaults() -> Self {
        Self {
            decoders: vec![
                Box::new(Markdown),
                Box::new(Leaflet),
                Box::new(Pckt),
                Box::new(Offprint),
                Box::new(Wordpress),
                Box::new(Unthread),
            ],
        }
    }

    /// Decode `content` into a [`RichDoc`], falling back to a typeset rendering of
    /// `text_content` when no decoder applies.
    pub fn decode(
        &self,
        content: Option<&Value>,
        text_content: Option<&str>,
        ctx: &DecodeCtx,
    ) -> RichDoc {
        if let Some(value) = content {
            for decoder in &self.decoders {
                if decoder.handles(value)
                    && let Some(doc) = decoder.decode(value, ctx)
                {
                    return doc;
                }
            }
        }
        Plaintext.from_text(text_content.unwrap_or_default())
    }
}

/// If `content` is a *reference* to another record rather than inline content (e.g.
/// GreenGale's `app.greengale.document#contentRef`), return the AT-URI to fetch. The
/// frontend resolves it via its `Transport`, then calls [`Registry::decode`] on the
/// fetched record's own `content`. The core decides *what* to fetch; the frontend does
/// the I/O — the same split as the rest of the engine.
pub fn content_ref(content: &Value) -> Option<AtUri> {
    let ty = content.get("$type")?.as_str()?;
    if !ty.ends_with("#contentRef") {
        return None;
    }
    AtUri::parse(content.get("uri")?.as_str()?)
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

#[cfg(test)]
mod tests {
    use super::*;

    const CTX: DecodeCtx = DecodeCtx {
        repo_did: "did:plc:test",
    };

    #[test]
    fn plaintext_fallback_splits_paragraphs() {
        let doc = Registry::with_defaults().decode(None, Some("hello world\n\nsecond para"), &CTX);
        assert_eq!(doc.blocks.len(), 2);
    }

    #[test]
    fn unknown_content_type_falls_back_to_textcontent() {
        // A `$type` no decoder claims must defer to the typeset `textContent`, not error.
        let content = serde_json::json!({ "$type": "com.example.nope", "whatever": 1 });
        let doc = Registry::with_defaults().decode(Some(&content), Some("a\n\nb"), &CTX);
        assert_eq!(doc.blocks.len(), 2);
        assert!(doc.blocks.iter().all(|b| matches!(b, Block::Paragraph(_))));
    }

    #[test]
    fn unthread_content_decodes_as_markdown() {
        let content = serde_json::json!({
            "$type": "at.unthread.content",
            "content": "plain then *emphasis*"
        });
        let doc = Registry::with_defaults().decode(Some(&content), Some("plain then emphasis"), &CTX);
        assert_eq!(
            doc.blocks,
            vec![Block::Paragraph(vec![
                Inline::Text("plain then ".into()),
                Inline::Emphasis(vec![Inline::Text("emphasis".into())]),
            ])]
        );
    }

    #[test]
    fn pckt_content_decodes_to_paragraphs() {
        let content = serde_json::json!({
            "$type": "blog.pckt.content",
            "items": [{ "$type": "blog.pckt.block.text", "plaintext": "test" }]
        });
        let doc = Registry::with_defaults().decode(Some(&content), Some("test"), &CTX);
        assert_eq!(
            doc.blocks,
            vec![Block::Paragraph(vec![Inline::Text("test".into())])]
        );
    }

    #[test]
    fn greengale_contentref_is_recognized_then_body_decodes() {
        // Phase 1: site.standard.document.content is a reference.
        let content = serde_json::json!({
            "$type": "app.greengale.document#contentRef",
            "uri": "at://did:plc:abc/app.greengale.document/3xyz"
        });
        let uri = content_ref(&content).expect("should detect contentRef");
        assert_eq!(uri.collection, "app.greengale.document");

        // Phase 2: the frontend fetched that record; its `content` is bare markdown.
        let body = Value::String("# Heading\n\nbody".into());
        let doc = Registry::with_defaults().decode(Some(&body), None, &CTX);
        assert_eq!(
            doc.blocks[0],
            Block::Heading {
                level: 1,
                content: vec![Inline::Text("Heading".into())]
            }
        );
    }

    #[test]
    fn non_ref_content_has_no_content_ref() {
        let content = serde_json::json!({ "$type": "app.offprint.content", "items": [] });
        assert!(content_ref(&content).is_none());
    }
}
