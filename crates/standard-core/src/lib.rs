//! `standard-core` — the UI- and I/O-agnostic engine for reading
//! [standard.site](https://standard.site) (long-form content on the AT Protocol).
//!
//! Everything platform-specific — HTTP, storage, the terminal, OAuth — is injected
//! through the [`atp::Transport`] and [`store::Store`] traits. The core itself is
//! **synchronous and dependency-light** so that the same brain can drive the
//! `ratatui` TUI today and, one day, a PS Vita frontend: a new platform only has to
//! supply a `Transport`, a `Store`, and a renderer for the [`model::RichDoc`] AST.
//!
//! The pipeline:
//!
//! 1. [`atp`] builds XRPC requests (handle→DID→PDS resolution, `listRecords`,
//!    `getRecord`, `getBlob`) and parses responses — over an injected `Transport`.
//! 2. [`decode`] turns each publisher's `content` lexicon (Leaflet/Pckt blocks,
//!    GreenGale Markdown, …) into one neutral [`model::RichDoc`].
//! 3. [`store`] caches records, read-state, and blobs for offline reading.
//! 4. [`search`] indexes the flat `textContent` for lookups.

pub mod atp;
pub mod decode;
pub mod model;
pub mod search;
pub mod store;
