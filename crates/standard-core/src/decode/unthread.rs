//! Unthread (`unthread.at`) — `at.unthread.content`, a typed wrapper whose `content` field is
//! a Markdown string. Same shape idea as the markpub wrapper, just a different namespace and
//! field name, so it rides the shared `pulldown-cmark` pipeline ([`from_markdown`]). Unthread
//! posts are short-form; the `site` is a plain URL (`https://unthread.at`) rather than an
//! at-URI publication, but that's the read model's concern — the decoder only sees `content`.

use serde_json::Value;

use super::markdown::from_markdown;
use super::{ContentDecoder, DecodeCtx};
use crate::model::{PublishingPlatform, RichDoc};

pub struct Unthread;

impl ContentDecoder for Unthread {
    fn handles(&self, content: &Value) -> bool {
        content.get("$type").and_then(Value::as_str) == Some("at.unthread.content")
    }

    fn decode(&self, content: &Value, _ctx: &DecodeCtx) -> Option<RichDoc> {
        // `{ "$type": "at.unthread.content", "content": "<markdown>" }`. A missing/non-string
        // body defers (→ next decoder → typeset `textContent`) rather than yielding an empty doc.
        let md = content.get("content").and_then(Value::as_str)?;
        Some(from_markdown(md))
    }

    fn publishing_platform(&self, _content: &Value) -> Option<PublishingPlatform> {
        Some(PublishingPlatform::Unthread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, Inline};

    const CTX: DecodeCtx = DecodeCtx {
        repo_did: "did:plc:test",
    };

    #[test]
    fn decodes_markdown_body() {
        let content = serde_json::json!({
            "$type": "at.unthread.content",
            "content": "*argues* and **firmly**"
        });
        assert!(Unthread.handles(&content));
        let blocks = Unthread.decode(&content, &CTX).unwrap().blocks;
        assert_eq!(
            blocks,
            vec![Block::Paragraph(vec![
                Inline::Emphasis(vec![Inline::Text("argues".into())]),
                Inline::Text(" and ".into()),
                Inline::Strong(vec![Inline::Text("firmly".into())]),
            ])]
        );
    }

    #[test]
    fn defers_when_content_is_absent_or_wrong_type() {
        // Wrong $type → not handled.
        assert!(!Unthread.handles(&serde_json::json!({ "$type": "at.markpub.markdown" })));
        // Right $type but no string body → None (defers to the fallback).
        let no_body = serde_json::json!({ "$type": "at.unthread.content" });
        assert!(Unthread.decode(&no_body, &CTX).is_none());
    }
}
