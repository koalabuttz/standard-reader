# Roadmap — standard-reader

A lean TUI reader for [standard.site](https://standard.site) (long-form on the AT Protocol). The engine (`standard-core`) is portable by design so it can grow new frontends — a desktop `ratatui` TUI now, a **PS Vita** frontend later — without a rewrite. See `CLAUDE.md` for architecture.

## Status — 2026-05-26: reads + offline cache work (CLI)

Workspace builds; the suite is green (39 tests across both crates: core unit + integration over real-record fixtures incl. an offline mock of the whole pipeline, plus the redb cache round-trip). The engine reaches live data and persists it: `sr fetch <handle|did>` resolves a repo, walks subscriptions → publications → documents, decodes them off real PDSes, and writes them into a `redb` cache; `sr cached` re-renders that cache with no network. No UI yet — that's next.

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
- [ ] OAuth loopback login (`atrium-oauth`) + `0600` token file (no keyring on Crostini) — unlocks the reader's *own* subscriptions.
- [ ] `ratatui` shell: subscription / document / reader panes, vim-style nav, async loading states.
- [ ] Cover + inline images (`ratatui-image`, iTerm2 protocol on hterm) from the `redb` cache.
- [ ] Uniform render theme (the reader's own consistent styling).

## v0.2 — richer

- [ ] Author `basicTheme` toggle (uniform vs. author's styling) — both render the same `RichDoc`, the mode only changes theming.
- [ ] Search UI over the `textContent` index.
- [ ] Background sync (incremental via `listRecords` cursors) + mark-read.
- [ ] Model growth as needed: a `Block::Table` / `Block::Callout` (today they degrade — table→cell-text paragraphs, Offprint callout→quote).

## Later

- [ ] RSS feed support (the original "like an RSS reader" stretch).
- [ ] Recommends (`site.standard.graph.recommend`) as a discovery signal.
- [ ] Show the Bluesky comment thread (`bskyPostRef`).
- [ ] `tantivy` search swap (only if ranked/fuzzy search is wanted).
- [ ] **PS Vita frontend** — new `Transport` + `Store` impls + framebuffer renderer, reusing all of `standard-core`.

## Immediate next step

The **`ratatui` shell** — an interactive reader over the pieces that already work: subscription / document / reader panes, vim-style nav, fetch on a worker thread into the `redb` cache, cover/inline images. Reading public subscriptions needs no auth, so OAuth (writing subs, private repos) can come after the UI is usable.
