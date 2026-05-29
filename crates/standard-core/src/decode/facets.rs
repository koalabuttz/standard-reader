//! Shared byte-range richtext facet engine.
//!
//! Leaflet, Offprint (and Bluesky) all annotate a `plaintext` string with byte-range
//! **facets**: `{ index: {byteStart, byteEnd}, features: [{ $type, … }] }`. The feature
//! `$type`s differ only by namespace and the `#suffix` (`#bold`/`#italic`/…), so one
//! suffix-keyed classifier serves every publisher. Slicing UTF-8 by byte offset is the
//! one genuinely fiddly bit — it lives here so each decoder stays a tiny `match`.

use serde_json::Value;

use crate::model::Inline;

/// A resolved inline style. `Code`/`Link` carry their payload; the rest are markers.
#[derive(Debug, Clone, PartialEq)]
pub enum FacetKind {
    Strong,
    Emphasis,
    Strike,
    Underline,
    /// Highlighted text, carrying the author's highlighter colour (parsed from a CSS `rgb(...)`).
    Highlight(Option<(u8, u8, u8)>),
    Code,
    Link(String),
}

/// One byte range plus the styles applied to it. A single atproto facet may list
/// several `features` (e.g. bold *and* italic); they nest, `kinds[0]` outermost.
#[derive(Debug, Clone, PartialEq)]
pub struct Facet {
    pub start: usize,
    pub end: usize,
    pub kinds: Vec<FacetKind>,
}

/// Classify a facet `feature` object by the `#suffix` of its `$type`. Shared across
/// publishers because the vocabulary is identical; unknown features return `None`
/// (the run stays plain text).
pub fn default_facet_kind(feature: &Value) -> Option<FacetKind> {
    let ty = feature.get("$type")?.as_str()?;
    Some(match ty.rsplit('#').next()? {
        "bold" => FacetKind::Strong,
        "italic" => FacetKind::Emphasis,
        "strikethrough" | "strike" => FacetKind::Strike,
        "underline" => FacetKind::Underline,
        "highlight" => FacetKind::Highlight(
            feature
                .get("color")
                .and_then(Value::as_str)
                .and_then(parse_css_rgb),
        ),
        "code" => FacetKind::Code,
        "link" => FacetKind::Link(feature.get("uri")?.as_str()?.to_string()),
        // A mention of an atproto identity (Offprint `#mention`, Leaflet `#didMention`) carries a
        // `did`; link it to the account's Bluesky profile. A `#webMention` carries a `uri` like a
        // plain link. Either way the mention's text stays readable; only the target is synthesized.
        "mention" | "didMention" => FacetKind::Link(format!(
            "https://bsky.app/profile/{}",
            feature.get("did")?.as_str()?
        )),
        "webMention" => FacetKind::Link(feature.get("uri")?.as_str()?.to_string()),
        _ => return None,
    })
}

/// Read the shared `facets` array off a text block, mapping each feature with
/// `classify`. Facets whose features are all unrecognized are dropped.
pub fn parse_facets(block: &Value, classify: impl Fn(&Value) -> Option<FacetKind>) -> Vec<Facet> {
    let Some(arr) = block.get("facets").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for f in arr {
        let idx = f.get("index");
        let (Some(start), Some(end)) = (
            idx.and_then(|i| i.get("byteStart")).and_then(Value::as_u64),
            idx.and_then(|i| i.get("byteEnd")).and_then(Value::as_u64),
        ) else {
            continue;
        };
        let kinds: Vec<FacetKind> = f
            .get("features")
            .and_then(Value::as_array)
            .map(|fs| fs.iter().filter_map(&classify).collect())
            .unwrap_or_default();
        if !kinds.is_empty() {
            out.push(Facet {
                start: start as usize,
                end: end as usize,
                kinds,
            });
        }
    }
    out
}

/// Convenience for the common block shape: a `plaintext` string with optional
/// shared-shape `facets`. Used by every byte-range block decoder.
pub fn text_block_inlines(block: &Value) -> Vec<Inline> {
    let text = block.get("plaintext").and_then(Value::as_str).unwrap_or("");
    apply_facets(text, &parse_facets(block, default_facet_kind))
}

/// Split `text` into inlines, wrapping each facet range in its styles. Robust by
/// construction: out-of-range or non-char-boundary offsets are skipped, overlapping
/// facets after the first are ignored — it never panics (the decode contract).
pub fn apply_facets(text: &str, facets: &[Facet]) -> Vec<Inline> {
    let mut ordered: Vec<&Facet> = facets.iter().collect();
    ordered.sort_by_key(|f| f.start);

    let mut out = Vec::new();
    let mut cursor = 0usize;
    for f in ordered {
        if f.start < cursor || f.start > f.end || f.end > text.len() {
            continue; // overlapping, inverted, or out of range
        }
        if !text.is_char_boundary(f.start) || !text.is_char_boundary(f.end) {
            continue; // offset lands mid-codepoint
        }
        if f.start > cursor {
            push_text(&mut out, &text[cursor..f.start]);
        }
        out.push(wrap(text[f.start..f.end].to_string(), &f.kinds));
        cursor = f.end;
    }
    if cursor < text.len() {
        push_text(&mut out, &text[cursor..]);
    }
    if out.is_empty() && !text.is_empty() {
        out.push(Inline::Text(text.to_string()));
    }
    out
}

fn push_text(out: &mut Vec<Inline>, s: &str) {
    if !s.is_empty() {
        out.push(Inline::Text(s.to_string()));
    }
}

/// Parse the RGB channels from a CSS colour like `rgb(168 85 247 / 0.2)` or `rgb(168,85,247)` —
/// the first three integers (any alpha is ignored; the model carries solid RGB and the frontend
/// applies its own opacity). Shared by the highlight facet and Offprint's callout tint.
pub fn parse_css_rgb(s: &str) -> Option<(u8, u8, u8)> {
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

/// Nest `text` inside its styles, `kinds[0]` outermost. `kinds` is non-empty.
fn wrap(text: String, kinds: &[FacetKind]) -> Inline {
    let mut it = kinds.iter().rev();
    let mut node = apply_kind(it.next().unwrap(), vec![Inline::Text(text)]);
    for k in it {
        node = apply_kind(k, vec![node]);
    }
    node
}

fn apply_kind(kind: &FacetKind, content: Vec<Inline>) -> Inline {
    match kind {
        FacetKind::Strong => Inline::Strong(content),
        FacetKind::Emphasis => Inline::Emphasis(content),
        FacetKind::Strike => Inline::Strike(content),
        FacetKind::Underline => Inline::Underline(content),
        FacetKind::Highlight(color) => Inline::Highlight {
            color: *color,
            content,
        },
        FacetKind::Code => Inline::Code(flatten_text(&content)),
        FacetKind::Link(href) => Inline::Link {
            href: href.clone(),
            content,
        },
    }
}

/// `Inline::Code` is a plain string, so a code facet flattens its span to text.
fn flatten_text(inlines: &[Inline]) -> String {
    let mut s = String::new();
    collect_text(inlines, &mut s);
    s
}

fn collect_text(inlines: &[Inline], out: &mut String) {
    for i in inlines {
        match i {
            Inline::Text(t) | Inline::Code(t) => out.push_str(t),
            Inline::Strong(c) | Inline::Emphasis(c) | Inline::Strike(c) | Inline::Underline(c) => {
                collect_text(c, out)
            }
            Inline::Highlight { content, .. } | Inline::Link { content, .. } => {
                collect_text(content, out)
            }
            Inline::LineBreak => out.push('\n'),
            Inline::Image(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bold(start: u64, end: u64) -> Value {
        serde_json::json!({
            "index": { "byteStart": start, "byteEnd": end },
            "features": [{ "$type": "app.offprint.richtext.facet#bold" }]
        })
    }

    #[test]
    fn splits_into_styled_runs() {
        let block = serde_json::json!({ "plaintext": "hello world", "facets": [bold(0, 5)] });
        assert_eq!(
            text_block_inlines(&block),
            vec![
                Inline::Strong(vec![Inline::Text("hello".into())]),
                Inline::Text(" world".into()),
            ]
        );
    }

    #[test]
    fn link_facet_keeps_uri() {
        let block = serde_json::json!({
            "plaintext": "see offprint",
            "facets": [{
                "index": { "byteStart": 4, "byteEnd": 12 },
                "features": [{ "$type": "pub.leaflet.richtext.facet#link", "uri": "https://offprint.app/" }]
            }]
        });
        assert_eq!(
            text_block_inlines(&block),
            vec![
                Inline::Text("see ".into()),
                Inline::Link {
                    href: "https://offprint.app/".into(),
                    content: vec![Inline::Text("offprint".into())]
                },
            ]
        );
    }

    #[test]
    fn multibyte_offsets_are_byte_accurate() {
        // "café" is 5 bytes (é = 2); bolding bytes 0..5 covers the whole word.
        let block = serde_json::json!({ "plaintext": "café!", "facets": [bold(0, 5)] });
        assert_eq!(
            text_block_inlines(&block),
            vec![
                Inline::Strong(vec![Inline::Text("café".into())]),
                Inline::Text("!".into())
            ]
        );
    }

    #[test]
    fn non_char_boundary_offset_is_skipped_not_panicked() {
        // byteEnd 1 lands inside the 3-byte '€' — must be dropped, not panic.
        let block = serde_json::json!({ "plaintext": "€", "facets": [bold(0, 1)] });
        assert_eq!(text_block_inlines(&block), vec![Inline::Text("€".into())]);
    }

    #[test]
    fn highlight_facet_keeps_its_authored_colour() {
        let block = serde_json::json!({
            "plaintext": "look here",
            "facets": [{
                "index": { "byteStart": 5, "byteEnd": 9 },
                "features": [{ "$type": "app.offprint.richtext.facet#highlight", "color": "rgb(168 85 247 / 0.5)" }]
            }]
        });
        assert_eq!(
            text_block_inlines(&block),
            vec![
                Inline::Text("look ".into()),
                Inline::Highlight {
                    color: Some((168, 85, 247)),
                    content: vec![Inline::Text("here".into())]
                },
            ]
        );
    }

    #[test]
    fn parses_css_rgb_first_three_ints() {
        assert_eq!(parse_css_rgb("rgb(168 85 247 / 0.2)"), Some((168, 85, 247)));
        assert_eq!(parse_css_rgb("rgb(10,20,30)"), Some((10, 20, 30)));
        assert_eq!(parse_css_rgb("nope"), None);
    }

    #[test]
    fn mention_links_to_the_bsky_profile_and_webmention_to_its_uri() {
        // A `#mention`/`#didMention` carries a `did` → a Bluesky profile link.
        let block = serde_json::json!({
            "plaintext": "hi @x",
            "facets": [{
                "index": { "byteStart": 3, "byteEnd": 5 },
                "features": [{ "$type": "app.offprint.richtext.facet#mention", "did": "did:plc:abc", "handle": "x" }]
            }]
        });
        assert_eq!(
            text_block_inlines(&block),
            vec![
                Inline::Text("hi ".into()),
                Inline::Link {
                    href: "https://bsky.app/profile/did:plc:abc".into(),
                    content: vec![Inline::Text("@x".into())]
                },
            ]
        );
        // A `#webMention` carries a `uri` → a plain link.
        let block = serde_json::json!({
            "plaintext": "see it",
            "facets": [{
                "index": { "byteStart": 4, "byteEnd": 6 },
                "features": [{ "$type": "app.offprint.richtext.facet#webMention", "uri": "https://example.com/" }]
            }]
        });
        assert_eq!(
            text_block_inlines(&block),
            vec![
                Inline::Text("see ".into()),
                Inline::Link {
                    href: "https://example.com/".into(),
                    content: vec![Inline::Text("it".into())]
                },
            ]
        );
    }

    #[test]
    fn nests_multiple_features_on_one_range() {
        let block = serde_json::json!({
            "plaintext": "x",
            "facets": [{
                "index": { "byteStart": 0, "byteEnd": 1 },
                "features": [
                    { "$type": "a#bold" },
                    { "$type": "a#italic" }
                ]
            }]
        });
        assert_eq!(
            text_block_inlines(&block),
            vec![Inline::Strong(vec![Inline::Emphasis(vec![Inline::Text(
                "x".into()
            )])])]
        );
    }
}
