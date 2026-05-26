# Roadmap — standard-reader

A lean TUI reader for [standard.site](https://standard.site) (long-form on the AT Protocol). The engine (`standard-core`) is portable by design so it can grow new frontends — a desktop `ratatui` TUI now, a **PS Vita** frontend later — without a rewrite. See `CLAUDE.md` for architecture.

## Status — 2026-05-26: scaffold

Workspace builds; `cargo test -p standard-core` is green (6 tests).

**Done (real & tested):**
- [x] Cargo workspace + the portable core/frontend split
- [x] `model` — the `RichDoc` AST (+ Document / Publication / Subscription)
- [x] `decode` — `ContentDecoder` trait + `Registry` dispatch + `Plaintext` fallback
- [x] `atp` — `AtUri` parsing + the `Transport` trait + XRPC URL builders
- [x] `store` — the `Store` cache trait
- [x] `search` — inverted index over `textContent`
- [x] provisional OAuth `client_metadata.json`

**Stubbed (return `None` → graceful fallback):** the Markdown / Leaflet / Pckt decoders, and the entire `standard-tui` frontend (prints a banner).

## v0.1 — read the basics

- [ ] **Markdown decoder** (`pulldown-cmark` → `RichDoc`) — lights up GreenGale + Sequoia/markpub blogs. *Pure core; testable against the known-good GreenGale record. → recommended next.*
- [ ] `reqwest` `Transport` impl + `redb` `Store` impl
- [ ] OAuth loopback login (`atrium-oauth`) + `0600` token file
- [ ] Read flow: subscriptions → publications → documents (direct PDS reads)
- [ ] `ratatui` shell: subscription / document / reader panes, vim-style nav, async loading states
- [ ] Cover images (`ratatui-image`) + offline cache from `redb`
- [ ] Uniform render theme

## v0.2 — richer

- [ ] Author `basicTheme` toggle (uniform vs. author's styling)
- [ ] `Leaflet` (blocks + facets) and `Pckt` (blocks) decoders
- [ ] Inspect a live Offprint `site.standard.document`; write its decoder
- [ ] Inline images in the reading pane
- [ ] Search UI over the `textContent` index
- [ ] Background sync (incremental via `listRecords` cursors) + mark-read

## Later

- [ ] RSS feed support (the original "like an RSS reader" stretch)
- [ ] Recommends (`site.standard.graph.recommend`) as a discovery signal
- [ ] Show the Bluesky comment thread (`bskyPostRef`)
- [ ] `tantivy` search swap (only if ranked/fuzzy search is wanted)
- [ ] **PS Vita frontend** — new `Transport` + `Store` impls + framebuffer renderer, reusing all of `standard-core`

## Immediate next step

The **Markdown decoder** — pure, portable, and immediately demonstrable against the real GreenGale test record (`did:plc:xn3l7ogsxym5ixxugidum5dw`). Then the live pipeline (`reqwest`/`redb` + read flow), then the `ratatui` shell.
