# Roadmap — standard-reader

A lean TUI reader for [standard.site](https://standard.site) (long-form on the AT Protocol). The engine (`standard-core`) is portable by design so it can grow new frontends — a desktop `ratatui` TUI now, a **PS Vita** frontend later — without a rewrite. See `CLAUDE.md` for architecture.

## Status — 2026-05-28: an interactive reader, with images and sign-in

Workspace builds; the suite is green (89 tests across both crates: core unit + integration over real-record fixtures incl. an offline mock of the whole pipeline, the redb cache round-trip, and the TUI's renderer/state/`TestBackend` tests, the OAuth loopback parser, and the subscription sync-diff). `sr` launches a **`ratatui` reader** — add a blog by handle (a local follow-list persisted in redb), browse the sidebar → document list → reader, search, command palette, mouse. The reader is a **block-flow** that renders real inline + cover images (`ratatui-image`, iTerm2 graphics where supported), all over a worker thread with an offline cache. Reading needs no auth; **`L` signs in via OAuth** to mirror the follow-list to atproto subscriptions. A first-launch layout picker is next.

**Done (real & tested):**
- [x] Cargo workspace + the portable core/frontend split
- [x] `model` — the `RichDoc` AST (+ Document / Publication / Subscription); inline quartet incl. `Underline`
- [x] `decode` — `ContentDecoder` trait (predicate `handles` + `DecodeCtx`), `Registry` dispatch, `Plaintext` fallback
- [x] **All six content decoders**, validated against live records:
  - [x] `Markdown` — bare string + the `at.markpub.markdown` wrapper (`pulldown-cmark`)
  - [x] `Leaflet` — `pub.leaflet.content` (`pages[].blocks[].block` + byte-range facets)
  - [x] `Pckt` — `blog.pckt.content` (recursive blocks + byte-range facets)
  - [x] `Offprint` — `app.offprint.content` (blocks + byte-range facets)
  - [x] `Wordpress` — `org.wordpress.html` (rendered HTML via `tl`)
  - [x] `Unthread` — `at.unthread.content` (a Markdown string, reusing the Markdown pipeline)
- [x] Shared **byte-range facet engine** (`decode/facets.rs`) + blob/url image helper, reused by all three block formats
- [x] `content_ref()` — the GreenGale `#contentRef` two-phase seam (core returns the AT-URI to fetch)
- [x] `atp` — `AtUri` parsing + the `Transport` trait + XRPC URL builders
- [x] `store` — the `Store` cache trait
- [x] `search` — inverted index over `textContent`
- [x] OAuth `client_metadata.json` — now **hosted & live** (`www.davidlewis.xyz`; `client_uri` → the project page)

## v0.1 — read the basics

Sequenced so each step is runnable on top of the last:

- [x] **Read pipeline.** XRPC response parsers + orchestration in `core/read.rs` (`resolve_identity` → `list_subscriptions` → `get_publication` → `list_documents` → `get_document` → `decode`), generic over `Transport`, synchronous, mock-tested. Plus the `reqwest::blocking` `Transport` impl and the `sr fetch <handle|did>` binary — proven end-to-end on live data (public records, no auth).
- [x] GreenGale `#contentRef` two-phase fetch + `get_blob` for image blobs are wired into the pipeline. **`at.unthread.content`** (Unthread — a Markdown string in `content`) has its own decoder, reusing the Markdown pipeline. **Pckt `gallery`** (a `blog.pckt.gallery` record ref) is resolved per-block in `get_document` — the decoder emits a `GalleryRef` placeholder, the read layer fetches the record and splices a resolved `ImageGrid`. No deferred content types remain — every `site.standard.document` content shape decodes.
- [x] `redb` `Store` impl — offline cache (publications, documents+body, read-state, blobs, sync cursors, a persisted inverted index for `search`). `sr fetch` caches as it reads; `sr cached` renders offline. *Surfaced:* `listRecords` lists a whole **repo**; a doc belongs to the publication its `site` field names, so per-publication listing filters by `site` (the cache keys on it; the frontend filters for display).
- [x] **`ratatui` shell** — sidebar + reader layout, modern-dark theme, mouse + full keyboard (vim/arrows), command palette + `?` help, search across the cache. A **local follow-list** in redb (add a blog by handle/DID/URL, persists, no auth) is home. Render loop ↔ worker thread (worker owns Transport+Store+Registry, cache-first). `sr` with no args launches it; `fetch`/`cached` stay as debug paths.
- [x] Uniform render theme — the modern-dark theme (`ui::theme`), single source of truth for a future theme/accent picker.
- [x] **Inline + cover images** — the reader is now a **block-flow** (text runs + image segments, row-scrolled). Blob bytes fetched by the worker, cached in redb (offline), decoded (`image`) and encoded for the terminal via `ratatui-image` (iTerm2 graphics protocol, halfblocks fallback). Cover renders atop; images scroll smoothly with the text via a row-sliced protocol (`SlicedProtocol` + `SignedPosition`), encoded once per display size. Markdown images (GreenGale/Unthread/markpub `![](…)`) render wherever `pulldown-cmark` puts them: a top-level paragraph image is hoisted to a block image, while an image **nested in a blockquote** (CommonMark lazy continuation) or list renders **in place, framed by the quote bar / list indent** — matching how the source platform shows it. Formats the lean `image` build can't decode (notably **AVIF**, which GreenGale emits) fall back to the bsky CDN's transcode-to-JPEG — only undecodable blobs touch the CDN; JPEG/PNG/WebP stay direct-from-PDS, and the result is cached offline. *Deferred:* `ThreadProtocol` to move the one-time encode off the UI thread; native (pure-Rust) AVIF decode if a viable decoder appears.
- [x] **OAuth loopback login** (`atrium-oauth`, DPoP/PKCE/PAR) + a `0600` session file under XDG config (no system keyring required). `L` (or palette) signs in by handle/DID → browser → a hand-rolled one-shot loopback server on `127.0.0.1:4599` → session persisted + restored on launch. The async/tokio surface is confined to a worker-owned runtime (`auth.rs`); the core stays synchronous. Pure-Rust TLS throughout — a custom rustls `HttpClient` replaces atrium's openssl `default-client`, and a stub DNS resolver forces HTTPS well-known handle resolution (no DNS stack). The follow-list now **mirrors to atproto** `site.standard.graph.subscription` (rkey stored in `FOLLOWS`): on sign-in, remote subscriptions import; local-only follows are reconciled via a **Subscribe / Remove** modal (no silent deletes); follow/unfollow then write upstream. (Reading public feeds still needs no auth.)
- [ ] First-launch layout picker + alternate layouts (feed-first / three-pane) + theme/accent customization.

## v0.2 — richer

- [ ] Author `basicTheme` toggle (uniform vs. author's styling) — both render the same `RichDoc`, the mode only changes theming.
- [x] Search UI over the `textContent` index (in the shell; `/` searches the redb inverted index).
- [x] **Incremental refresh** — a feed backfills its full history on first fetch (`listRecords` paginated to exhaustion, no page cap), then refreshes walk newest-first and stop at already-cached records via the per-publication `sync_cursor` high-water mark. Plus live-path hardening: connect/request timeouts on both HTTP clients, and a transient restore failure no longer wipes the session.
- [ ] *Automatic* background sync (periodic / on a timer, not just on `r`/open) + unread badges in the list (mark-read exists, on open + `m`).
- [x] `Block::Table` (Pckt tables → box-drawing grid) and `Block::Callout` (Offprint callouts → a tinted box with the author's colour + emoji badge), both first-class in the model and reader.
- [x] **Reader rendering polish** — interactive in-post hyperlinks (keyboard `n`/`N` to cycle + `Enter`, plus mouse click), code blocks framed with a left gutter + language label, and display-width-correct tables / full-width rules (`unicode-width`). Link rects are read back from a temp-buffer render of each line, so click targets match ratatui's actual word-wrap even when a link wraps across rows.

## Later

- [ ] RSS feed support (the original "like an RSS reader" stretch).
- [ ] Recommends (`site.standard.graph.recommend`) as a discovery signal.
- [ ] Show the Bluesky comment thread (`bskyPostRef`).
- [ ] `tantivy` search swap (only if ranked/fuzzy search is wanted).
- [ ] **PS Vita frontend** — new `Transport` + `Store` impls + framebuffer renderer, reusing all of `standard-core`.

## Immediate next step

**First-launch layout picker** + alternate layouts (feed-first / three-pane) and theme/accent customization — the one v0.1 item still open. Then v0.2: the author-`basicTheme` toggle, and *automatic* background sync with unread badges in the list (incremental refresh itself now landed). Deferred polish: moving the one-time image encode off the UI thread (`ThreadProtocol`).
