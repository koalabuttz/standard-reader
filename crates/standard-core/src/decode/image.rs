//! Shared image construction: atproto blob refs and plain URLs → [`Image`].
//!
//! Block lexicons (Leaflet/Pckt/Offprint) embed only a blob *ref* (the CID); the
//! owning repo DID lives outside `content` and is supplied via [`DecodeCtx`]. Markdown
//! (GreenGale) instead emits already-resolved `getBlob` URLs.
//!
//! [`DecodeCtx`]: crate::decode::DecodeCtx

use serde_json::Value;

use crate::model::{Image, ImageSource};

/// Build a blob-backed [`Image`] from a `{ ref: { $link: <cid> }, … }` value, pairing
/// the CID with the owning `did`. Returns `None` if no CID is present.
pub fn blob_image(blob: &Value, did: &str, alt: &str) -> Option<Image> {
    let cid = blob
        .get("ref")
        .and_then(|r| r.get("$link"))
        .and_then(Value::as_str)?;
    Some(Image {
        alt: alt.to_string(),
        source: ImageSource::Blob {
            did: did.to_string(),
            cid: cid.to_string(),
        },
    })
}

/// Build a URL-backed [`Image`] (used by Markdown/HTML, which carry full URLs).
pub fn url_image(url: &str, alt: &str) -> Image {
    Image {
        alt: alt.to_string(),
        source: ImageSource::Url(url.to_string()),
    }
}
