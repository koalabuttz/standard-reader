# Roadmap — standard-reader

A lean TUI reader for [standard.site](https://standard.site) (long-form on the AT Protocol). The engine (`standard-core`) is portable by design so it can grow new frontends — a desktop `ratatui` TUI now, a **PS Vita** frontend later — without a rewrite. See `CLAUDE.md` for architecture.

## Status — 2026-05-26: an interactive reader

Workspace builds; the suite is green (46 tests across both crates: core unit + integration over real-record fixtures incl. an offline mock of the whole pipeline, the redb cache round-trip, and the TUI's renderer/state/`TestBackend` tests). `sr` launches a **`ratatui` reader** — add a blog by handle (a local follow-list persisted in redb), browse the sidebar → document list → reader, search, command palette, mouse, all over a worker thread with offline cache. Reading needs no auth. Inline images and OAuth are next.

**Done (real & tested):**
- [x] Cargo workspace + the portable core/frontend split
- [x] `model` — the `RichDoc` AST (+ Document / Publication / Subscription); inline quartet incl. `Underline`
- [x] `decode` — `ContentDecoder` trait (predicate `handles` + `DecodeCtx`), `Registry` dispatch, `Plaintext` fallback
- [x] **All five content decoders**, validated against live records:
  - [x] `Markdown` — bare string + the `at.markpub.markdown` wrapper (`pulldown-cmark`)
  - [x] `Leaflet` — `pub.leaflet.content` (`pages[].blocks[].block` + byte-range facets)
  - [x] `Pckt` — `blog.pckt.content` (recursive blocks + byte-range facets)
  - [x] `Offprint` — `app.offprint.content` (blocks + byte-range facets)
  - [x] `Wordpress` — `org.wordpress.html` (rendered HTML via `tl`)
- [x] Shared **byte-range facet engine** (`decode/facets.rs`) + blob/url image helper, reused by all three block formats
- [x] `content_ref()` — the GreenGale `#contentRef` two-phase seam (core returns the AT-URI to fetch)
- [x] `atp` — `AtUri` parsing + the `Transport` trait + XRPC URL builders
- [x] `store` — the `Store` cache trait
- [x] `search` — inverted index over `textContent`
- [x] provisional OAuth `client_metadata.json`

**Stubbed — the entire live path:** no `Transport` impl (nothing moves bytes), **no XRPC response parsers** (JSON → `Document`/`Publication`/`Subscription` doesn't exist), no read-flow orchestration, no `redb` `Store` impl, no OAuth, and `standard-tui` is an 11-line banner.

## v0.1 — read the basics

Sequenced so each step is runnable on top of the last:

- [x] **Read pipeline.** XRPC response parsers + orchestration in `core/read.rs` (`resolve_identity` → `list_subscriptions` → `get_publication` → `list_documents` → `get_document` → `decode`), generic over `Transport`, synchronous, mock-tested. Plus the `reqwest::blocking` `Transport` impl and the `sr fetch <handle|did>` binary — proven end-to-end on live data (public records, no auth).
- [x] GreenGale `#contentRef` two-phase fetch + `get_blob` for image blobs are wired into the pipeline. *Still deferred:* Pckt `gallery` ref (a record fetch like contentRef), and a decoder for **`at.unthread.content`** (a 6th content type seen live; currently degrades to the `Plaintext` fallback).
- [x] `redb` `Store` impl — offline cache (publications, documents+body, read-state, blobs, sync cursors, a persisted inverted index for `search`). `sr fetch` caches as it reads; `sr cached` renders offline. *Surfaced:* `listRecords` lists a whole **repo**; a doc belongs to the publication its `site` field names, so per-publication listing filters by `site` (the cache keys on it; the frontend filters for display).
- [x] **`ratatui` shell** — sidebar + reader layout, modern-dark theme, mouse + full keyboard (vim/arrows), command palette + `?` help, search across the cache. A **local follow-list** in redb (add a blog by handle/DID/URL, persists, no auth) is home. Render loop ↔ worker thread (worker owns Transport+Store+Registry, cache-first). `sr` with no args launches it; `fetch`/`cached` stay as debug paths.
- [x] Uniform render theme — the modern-dark theme (`ui::theme`), single source of truth for a future theme/accent picker.
- [ ] **Inline + cover images** (`ratatui-image` + `ThreadProtocol`, iTerm2 on hterm) → recommended next. The reader currently shows a `🖼 alt` placeholder.
- [ ] OAuth loopback login (`atrium-oauth`) + `0600` token file (no keyring on Crostini) — to *write/sync* the follow-list to atproto `site.standard.graph.subscription`. (Reading public feeds already needs no auth.)
- [ ] First-launch layout picker + alternate layouts (feed-first / three-pane) + theme/accent customization.

## v0.2 — richer

- [ ] Author `basicTheme` toggle (uniform vs. author's styling) — both render the same `RichDoc`, the mode only changes theming.
- [x] Search UI over the `textContent` index (in the shell; `/` searches the redb inverted index).
- [ ] Background sync (incremental via `listRecords` cursors). Mark-read exists (on open + `m`); unread badges in the list are still TODO.
- [ ] Model growth as needed: a `Block::Table` / `Block::Callout` (today they degrade — table→cell-text paragraphs, Offprint callout→quote).

## Later

- [ ] RSS feed support (the original "like an RSS reader" stretch).
- [ ] Recommends (`site.standard.graph.recommend`) as a discovery signal.
- [ ] Show the Bluesky comment thread (`bskyPostRef`).
- [ ] `tantivy` search swap (only if ranked/fuzzy search is wanted).
- [ ] **PS Vita frontend** — new `Transport` + `Store` impls + framebuffer renderer, reusing all of `standard-core`.

## Immediate next step

**Inline + cover images** — wire `ratatui-image` (`Picker` auto-detect + `ThreadProtocol` so encoding stays off the UI thread; iTerm2 protocol on this hterm box) and the `image` crate, fetching blob bytes via the worker's `get_blob` into the `redb` blob cache. Replaces the reader's `🖼 alt` placeholder — the biggest remaining "beautiful" lever. Then OAuth, then the layout picker.
