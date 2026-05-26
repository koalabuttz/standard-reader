//! The internal, platform-neutral document model.
//!
//! Every publisher's `content` lexicon is decoded into the one [`RichDoc`] AST, and
//! every frontend renders *from* it. That is the seam: decoders never know about
//! rendering, renderers never know about lexicons.

use serde::{Deserialize, Serialize};

/// A fully decoded document body: an ordered list of block-level elements.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RichDoc {
    pub blocks: Vec<Block>,
}

/// Block-level content. The common denominator across Markdown and every block
/// lexicon we decode; anything richer degrades into the nearest of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Block {
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    Quote(Vec<Block>),
    List {
        ordered: bool,
        items: Vec<Vec<Block>>,
    },
    Code {
        lang: Option<String>,
        text: String,
    },
    Image(Image),
    /// A simple table: an optional header row of cells, then body rows. Each cell is inline
    /// content (block content inside a cell is flattened) — enough for the tables publishers emit.
    Table {
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
}

/// Inline (span-level) content. Leaflet/Bluesky "facets" (byte-range annotations)
/// decode into nested `Inline`s just like Markdown spans do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Inline {
    Text(String),
    Strong(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strike(Vec<Inline>),
    Underline(Vec<Inline>),
    Code(String),
    Link { href: String, content: Vec<Inline> },
    Image(Image),
    LineBreak,
}

/// An image reference — either a direct URL or an atproto blob. The frontend
/// resolves a [`ImageSource::Blob`] to a `com.atproto.sync.getBlob` request via
/// its `Transport`, then hands the bytes to its image renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub alt: String,
    pub source: ImageSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ImageSource {
    Url(String),
    Blob { did: String, cid: String },
}

/// Metadata for a `site.standard.document`, independent of its decoded body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// `at://<did>/site.standard.document/<rkey>`
    pub uri: String,
    pub title: String,
    pub description: Option<String>,
    /// AT-URI of the owning publication (the record's `site` field).
    pub publication: String,
    pub published_at: String,
    pub updated_at: Option<String>,
    pub cover_image: Option<Image>,
    /// Flat plaintext fallback / search source (spec: contains no formatting).
    pub text_content: Option<String>,
    pub tags: Vec<String>,
}

/// A `site.standard.publication` record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Publication {
    /// `at://<did>/site.standard.publication/<rkey>`
    pub uri: String,
    /// Base URL for the publication's web home.
    pub url: String,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<Image>,
}

/// A `site.standard.graph.subscription` record from the reader's own repo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub uri: String,
    /// AT-URI of the subscribed publication.
    pub publication: String,
}
