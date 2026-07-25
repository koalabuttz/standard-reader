# Roadmap — standard-reader

A reader for [standard.site](https://standard.site) (long-form on the AT Protocol). The engine (`standard-core`) plus a platform-agnostic frontend (`standard-frontend`: App + UI + worker + seam traits) are portable by design: the desktop `ratatui` TUI and **browser/WASM** shell share them now, and a **PS Vita** frontend can follow without a rewrite. See `CLAUDE.md` for architecture.

## Status — 2026-07-24: `sr` 1.2.0 release candidate; browser M3 complete (engine `standard-core` 0.3.0)

Workspace builds; the suite is green (188 tests across the workspace: core unit + integration over real-record fixtures incl. an offline mock of the whole pipeline, full Offprint/Leaflet block+facet coverage + alignment, the redb and OPFS-cache round trips, browser input/persistence/OAuth transaction tests, and the frontend/TUI renderer-state tests, OAuth scope/loopback parsing, subscription sync-diff, unread/load-older plumbing, and customization persistence). `sr` launches a **`ratatui` reader** — add a blog by handle (a local follow-list persisted in redb), browse the sidebar → document list → reader, search, command palette, mouse. The reader is a **block-flow** that renders real inline + cover images (`ratatui-image`, iTerm2 graphics where supported), all over a worker thread with an offline cache. Reading needs no auth; **`L` signs in via OAuth** to mirror the follow-list to atproto subscriptions. **Fully customizable**: cycle layouts (`\`), resize panes independently (`< >`), pick/edit a global colour theme (`t`), and give each blog its own layout, preset, or independent custom palette (`b`) — persisted to `prefs.toml`. Feeds load **lazily** — adding a handle that publishes several blogs shows a **pick-which-to-follow** checklist, and opening a feed pulls a bounded recent window (older posts on demand via `↓`) rather than backfilling everything up front.

The **browser/WASM shell is complete through M3**: the shared frontend renders through ratzilla,
fetches directly from PDSes on a Web Worker, paints native browser image overlays, persists the
complete offline reading cache plus appearance preferences in OPFS, and signs in directly through
atproto browser OAuth to mirror subscriptions. Public reading and local follows remain available
without an account.

**Done (real & tested):**
- [x] Cargo workspace + the portable split: `standard-core` (engine) → `standard-frontend` (App/UI/worker) → `standard-tui` (desktop shell)
- [x] **`standard-frontend` extracted** — the App/UI/worker + the `FrontendStore` / `AuthProvider` / `ImageSink` seam traits, generic over the platform, with no platform deps (`ratatui` + `image` only) — groundwork for the web & Vita shells
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
- [x] **Inline + cover images** — the reader is now a **block-flow** (text runs + image segments, row-scrolled). Blob bytes fetched by the worker, cached in redb (offline), decoded (`image`) and encoded for the terminal via `ratatui-image` (iTerm2 graphics protocol, halfblocks fallback). Cover renders atop; images scroll smoothly with the text via a row-sliced protocol (`SlicedProtocol` + `SignedPosition`), encoded once per display size. Markdown images (GreenGale/Unthread/markpub `![](…)`) render wherever `pulldown-cmark` puts them: a top-level paragraph image is hoisted to a block image, while an image **nested in a blockquote** (CommonMark lazy continuation) or list renders **in place, framed by the quote bar / list indent** — matching how the source platform shows it. Formats the default-features `image` build can't decode (notably **AVIF**, which GreenGale emits) fall back to the bsky CDN's transcode-to-JPEG — only undecodable blobs touch the CDN; JPEG/PNG/WebP stay direct-from-PDS, and the result is cached offline. *Deferred:* `ThreadProtocol` to move the one-time encode off the UI thread; native (pure-Rust) AVIF decode if a viable decoder appears.
- [x] **OAuth loopback login** (`atrium-oauth`, DPoP/PKCE/PAR) + a `0600` session file under XDG config (no system keyring required). `L` (or palette) signs in by handle/DID → browser → a hand-rolled one-shot loopback server on `127.0.0.1:4599` → session persisted + restored on launch. The async/tokio surface is confined to a worker-owned runtime (`auth.rs`); the core stays synchronous. Pure-Rust TLS throughout — a custom rustls `HttpClient` replaces atrium's openssl `default-client`, and a stub DNS resolver forces HTTPS well-known handle resolution (no DNS stack). The follow-list now **mirrors to atproto** `site.standard.graph.subscription` (rkey stored in `FOLLOWS`): on sign-in, remote subscriptions import; local-only follows are reconciled via a **Subscribe / Remove** modal (no silent deletes); follow/unfollow then write upstream. (Reading public feeds still needs no auth.)
- [x] **Customization** — a first-launch picker + in-app controls: cycleable layouts (one / two / three-pane + a drill-down) with independently resizable panes; colour themes (built-in presets + an in-app RGB editor over a human-editable `prefs.toml`); and **per-blog overrides** with independently editable custom palettes. Effective value = opened-blog override else global.

## v0.2 — richer

- [x] ~~Author `basicTheme` toggle (uniform vs. author's styling).~~ **Dropped** in favour of user-driven customization (see the v0.1 "Customization" item): one consistent, user-controlled render path is simpler than honouring each publication's styling, and is what a reader wants.
- [x] Search UI over the `textContent` index (in the shell; `/` searches the redb inverted index).
- [x] **Incremental refresh** — opening a feed for the first time fetches a **bounded recent window** (`list_documents_window`, ~3 `listRecords` pages) rather than backfilling its whole history up front; subsequent refreshes walk newest-first and stop at already-cached records via the per-publication `sync_cursor` high-water mark, and **load-older** (`↓` past the bottom) pulls the next window from a repo-DID-keyed older cursor. *(1.1.0 changed first-open from exhaustive backfill to this lazy bounded fetch — following a prolific author with many blogs no longer locks the app up.)* Plus live-path hardening: connect/request timeouts on both HTTP clients, and a transient restore failure no longer wipes the session.
- [ ] *Automatic* background sync (periodic / on a timer, not just on `r`/open). Unread badges and mark-read-on-open/`m` are complete.
- [x] `Block::Table` (Pckt tables → box-drawing grid) and `Block::Callout` (Offprint callouts → a tinted box with the author's colour + emoji badge), both first-class in the model and reader.
- [x] **Reader rendering polish** — interactive in-post hyperlinks (keyboard `n`/`N` to cycle + `Enter`, plus mouse click), code blocks framed with a left gutter + language label, and display-width-correct tables / full-width rules (`unicode-width`). Link rects are read back from a temp-buffer render of each line, so click targets match ratatui's actual word-wrap even when a link wraps across rows.

## Later

- [ ] RSS feed support (the original "like an RSS reader" stretch).
- [ ] Recommends (`site.standard.graph.recommend`) as a discovery signal.
- [ ] Show the Bluesky comment thread (`bskyPostRef`).
- [x] **Embeds degrade to links.** Pckt iframes (YouTube → `watch?v=`), Leaflet embeds, and Offprint web embeds/bookmarks become clickable links rather than disappearing.
- [ ] **Richer embed labels.** A Bluesky embed (`bsky.app` / AT-URI) currently links by raw URL; resolve a friendlier label (author + snippet) instead. Same idea for other recognizable embed hosts.
- [ ] `tantivy` search swap (only if ranked/fuzzy search is wanted).
- [x] **Browser / WASM frontend** — a `standard-reader-web` shell over `standard-frontend`:
  - [x] **M0:** extract the platform-agnostic frontend and seam traits.
  - [x] **M1:** ratzilla UI, Web Worker + synchronous-XHR transport, and native `<img>` overlays.
  - [x] **M2:** OPFS-backed offline cache (including images/read state/cursors) plus persisted appearance preferences.
  - [x] **M3:** browser OAuth `AuthProvider` and atproto subscription writes.
- [ ] **PS Vita frontend** — new `Transport` + `Store` impls + framebuffer renderer, reusing `standard-core` + `standard-frontend`.

## 1.0 — released

`sr` **1.0.0** ships the thesis: a TUI reader for standard.site — all six decoders, the
block-flow reader with images, the offline `redb` cache, search, OAuth + subscription sync, and
full layout/theme customization. Distributed as prebuilt binaries (Linux x86_64/aarch64, macOS
Apple Silicon, Windows x86_64) plus `cargo install --git`. The engine, **`standard-core`**, stays
**0.2.0** — its `Transport`/`Store` API is deliberately unpromised (not on crates.io) until a
second frontend (the PS Vita port) validates the seam. See `CHANGELOG.md`.

## 1.1 — released

`sr` **1.1.0** (engine `standard-core` **0.3.0**) makes feeds lazy: following a blog no longer
backfills its whole history (a prolific author with ~25 blogs in one repo used to lock the app up).
First-open fetches a bounded recent window, with **load-older** on demand (an end-of-feed
affordance shows when more remain); adding a multi-publication handle/DID shows a
**pick-which-blogs** checklist (select-all/none); the reader pane caches its computed layout so
sidebar nav and scrolling stay snappy. Adds **unread badges** (per-feed counts + per-post markers)
and **completes Offprint + Leaflet coverage** — every Offprint block/facet now decodes (lists, code,
blockquotes, image carousels/diffs, `highlight`/`@mention` text), and Leaflet's previously-dropped
lists and embeds render, with embeds a terminal can't host degrading to clickable links. Folds in
the OS-keyring session store (macOS/Windows native; Linux Secret Service opt-in), the
metadata-only-post `description` fallback, and the MSRV correction to 1.88. See `CHANGELOG.md`.

## Post-1.0

- [ ] *Automatic* background sync (timer-based, not just on `r`/open).
- [ ] RSS feed support (the original "like an RSS reader" stretch).
- [ ] **Richer embed labels.** Embeds now degrade to clickable links (1.1.0); resolve a friendlier
  label for a Bluesky post / known host (author + snippet) instead of the raw URL. Plus recommends
  (`site.standard.graph.recommend`) and the Bluesky comment thread.
- [ ] `tantivy` search swap (only if ranked/fuzzy is wanted).
- [x] **Browser / WASM frontend M3** — direct DPoP/PKCE/PAR browser OAuth with durable OPFS
  sessions and atproto subscription writes; M0–M3 are complete.
- [ ] **PS Vita frontend** — new `Transport` + `Store` impls + framebuffer renderer, reusing `standard-core` + `standard-frontend`.
- Deferred polish: moving the one-time image encode off the UI thread (`ThreadProtocol`).

## Known limitations

- **Offprint foreground text colour isn't rendered — it isn't in the record.** Offprint's editor
  has two separate colour marks: a `textStyle` *foreground* colour (e.g. "Grey text" shown grey) and
  a `highlight` *background* wash. Only `#highlight` is exported to the `app.offprint.content`
  atproto record (verified: no `textStyle`/`#color` facet type, and a colour-labelled run carries no
  facet) — the foreground colour lives only in the editor JSON embedded in Offprint's web page, which
  we don't (and, given direct-PDS reads, shouldn't) scrape. So coloured text renders plain while the
  highlight shows correctly. If Offprint adds an `app.offprint.richtext.facet#color` (or similar) to
  its published records, honoring it is a one-line decoder arm + a new `Inline` variant.
