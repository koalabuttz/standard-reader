# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repository.

## Project

`standard-reader` (binary **`sr`**) is a **TUI reader for [standard.site](https://standard.site)** — long-form writing published to the AT Protocol (Leaflet, Pckt, Offprint, GreenGale, and any blog emitting `site.standard.*` records). Sign in with an atproto account, pull your subscriptions, and read — with images and real formatting, online or **offline**. (RSS support is a later goal.)

**Ethos (load-bearing, mirrors davidlewis.xyz): a small, justified dependency surface.** No build step beyond `cargo`; no runtime services. Prefer pure-Rust crates; every dependency should earn its place (the AVIF C decoder and an openssl TLS stack were both turned down on these grounds).

## Architecture — portable core, portable frontend, swappable shell

A Cargo workspace, split for **portability**: a PS Vita frontend *and* a browser/WASM frontend are stated goals, so neither the engine nor the UI may assume a desktop.

```
crates/
  standard-core/      lib · ZERO platform deps · SYNCHRONOUS · the whole brain
    model               · RichDoc AST + Document/Publication/Subscription
    decode              · ContentDecoder trait + Registry + per-publisher decoders
    atp                 · AtUri parsing + XRPC request building (over a Transport)
    store               · the Store cache trait
    search              · inverted index over textContent
  standard-frontend/  lib · platform-agnostic frontend · App + UI + worker, no platform stack
    app                 · the App state machine (events → state + ToWorker commands; I/O-free)
    ui                  · the ratatui renderer (sidebar + block-flow reader, themes, pickers)
    worker              · generic orchestration: cache-first reads, lazy fetch, sync, freshen
    prefs               · layout/theme/per-blog prefs (+ their (de)serialization)
    {frontend_store,auth_provider,image_sink} · the frontend seam traits (+ Account)
  standard-tui/       bin `sr` · the desktop shell (reqwest + redb + atrium-oauth + ratatui-image + open)
```

Dependency direction: `standard-tui` → `standard-frontend` → `standard-core`. A future `standard-reader-web` is a second shell over the same `standard-frontend` (ratzilla/WASM); a Vita shell, a third.

**The seams** — the only things a new platform implements. Two are *core-level* (in `standard-core`, which builds every URL and parses every response, and never opens a socket):

- **`atp::Transport`** — perform an XRPC GET/POST and attach auth. Desktop: `reqwest::blocking`; a Vita: its net stack; web: a synchronous `XMLHttpRequest` in a worker.
- **`store::Store`** — the offline cache (documents, read-state, blobs, sync cursors). Desktop: `redb`; web: OPFS/IndexedDB.

Three more are *frontend-level* (in `standard-frontend`, extending those for the App + worker):

- **`FrontendStore: Store`** — the local follow-list (+ each follow's subscription rkey) and the show-images preference.
- **`AuthProvider`** — sign-in + subscription writes, a **synchronous (blocking)** trait (the worker blocks on it). Desktop: `DesktopAuth` owns a tokio runtime and `block_on`s `atrium-oauth`, keeping the worker async-free; a Vita might use an app-password, a web shell a browser-OAuth island.
- **`ImageSink`** — *"paint an image into a reserved cell rect."* Desktop: `TerminalImageSink` builds `ratatui-image` row-slices; web: a native `<img>`/canvas overlay. The reader is image-protocol-agnostic and carries no `ratatui-image` types.

Plus three **host hooks** the shell injects at the seam, so the frontend touches no filesystem/process: a debug-log sink and a prefs-save sink (closures passed to `worker::spawn`), and `App::set_open_url` (browser launch).

**Hard rules:** the core stays synchronous — never let `tokio`/`reqwest`/`redb`/`ratatui`/`async` into `standard-core`. And `standard-frontend` stays platform-agnostic — never let a platform stack (`reqwest`/`redb`/`tokio`/`atrium`/`ratatui-image`/`open`) into it; it depends only on `ratatui` + `image` (both pure-Rust, WASM-clean). A shell gets non-blocking fetches by running the synchronous `worker` off the render thread — desktop: a worker thread + `mpsc` channels feeding the `ratatui` loop; a Vita/web shell wires the same `worker` to its own thread model. `FromWorker::Image` carries a concrete `image::DynamicImage`, so the worker (decode + AVIF→CDN fallback) is shared verbatim with no image-payload generics.

Pipeline: **`atp`** builds/parses XRPC → **`decode`** maps each publisher's `content` lexicon to one `RichDoc` → **`store`** caches it for offline → **`search`** indexes `textContent`.

## Content decoding (validated against real records)

`site.standard.document.content` is an **open union** — each publisher embeds its own lexicon. `textContent` is flat plaintext (the spec says it carries *no* formatting), so it is a **fallback only**. Decoders dispatch on `content.$type` and all target the one neutral `RichDoc` AST:

Shapes below were validated against **live records** (the published survey had several wrong field names). All decoders are ✅ implemented and tested against fixtures in `crates/standard-core/tests/fixtures/`:

| `content.$type`                          | shape                                            | decoder      |
| ---------------------------------------- | ------------------------------------------------ | ------------ |
| *(bare string)* / `at.markpub.markdown`  | Markdown (GreenGale body, Sequoia, markpub)      | `Markdown` (pulldown-cmark) |
| `pub.leaflet.content`                    | `pages[].blocks[].block` + byte-range facets     | `Leaflet`    |
| `blog.pckt.content`                      | `items: [blog.pckt.block.*]` (large → in a blob) | `Pckt`       |
| `app.offprint.content`                   | `items: [app.offprint.block.*]` + byte-range facets | `Offprint` |
| `org.wordpress.html`                     | `{ html }` — rendered HTML (`tl` walker)         | `Wordpress`  |
| `at.unthread.content`                    | `{ content }` — a Markdown string (Unthread)     | `Unthread` (reuses `from_markdown`) |
| `*#contentRef`                           | **reference** to another record (GreenGale)      | `content_ref` → two-phase |
| *(unknown / absent)*                     | typeset `textContent`                            | `Plaintext`  |

Leaflet/Pckt/Offprint share one **byte-range facet engine** (`decode/facets.rs`): each carries `{ index:{byteStart,byteEnd}, features:[{$type}] }` over a `plaintext` string, differing only by namespace + `#suffix`. The suffix classifier maps `bold/italic/strikethrough/underline/code/link` and — added in 1.1.0 — `highlight` (→ `Inline::Highlight` carrying the author's `color`, rendered as a per-hue **background** wash via `Theme::tint`, matching Offprint's highlight — a Tiptap background marker; Offprint's separate foreground `textStyle` text-colour isn't exported to the atproto record, so we never receive it), `mention`/`didMention` (→ a link to the account's `bsky.app/profile/<did>`), and `webMention` (→ a `uri` link). **Offprint and Leaflet coverage is complete (validated against each platform's "all blocks" reference + a live Leaflet doc):** Offprint decodes all 15 block types (text/heading/image/imageGrid, blockquote→`Quote`, codeBlock→`Code`, ordered/bulletList→`List`, taskList→`List` with ☑/☐ prefixes, imageCarousel/imageDiff→`ImageGrid`, callout, horizontalRule, and webEmbed/webBookmark→a link paragraph); Leaflet adds unordered/orderedList→`List` and its embeds (website/bskyPost/button/standardSitePost/poll), where a `bskyPost` AT-URI is rewritten to its `bsky.app` web URL. Embeds a terminal can't host degrade to a clickable `Inline::Link` (the same treatment as the Pckt iframe), never a dropped block. **Alignment** (`textAlign`/`alignment`) is carried by a thin `Block::Aligned { align: Align, content: Box<Block> }` wrapper emitted only when not the default — text wraps on center/right (left stays bare), images wrap on any explicit value incl. left (the reader centers a *bare* image by default, so honoring "left" needs the wrapper). The reader applies it per-`Line` for text (so a merged run of blocks keeps per-block alignment and the link-rect scan agrees) and generalizes its existing image/grid centering to left/center/right. **GreenGale is two-phase**: `site.standard.document.content` is a `#contentRef` pointing at an `app.greengale.document` whose own `content` is the bare Markdown string — the core's `content_ref()` returns the AT-URI; the frontend fetches it and re-runs `decode`. Block decoders need the owning repo DID (passed via `DecodeCtx`) to build blob image refs. **Pckt `gallery` is the same two-phase idea at block granularity:** the decoder emits a `Block::GalleryRef { uri }` placeholder and `read::get_document` fetches the `blog.pckt.gallery` record (`{ images: [{ blob }] }`) and splices in a resolved `Block::ImageGrid` — so no `GalleryRef` ever reaches a frontend. **Pckt also externalizes a *large* block list into a blob:** past ~20 KB, `content.items` is empty and the `[blog.pckt.block.*]` JSON array lives in a `text/plain` blob (`content.blob`); `read::get_document` fetches it (CID via `pckt::external_content_cid`), splices the array back into `items`, and decodes normally — keeping the decoder pure. (Without this, such a doc would degrade to the flat `textContent` fallback and lose its structure.) A Pckt **`iframe` embed** (e.g. a YouTube player, which a TUI can't host) decodes to a paragraph holding one `Inline::Link` — a YouTube `/embed/<id>` URL is rewritten to its `watch?v=<id>` page; so it becomes a clickable / keyboard-navigable link in the reader, reusing the link machinery rather than a new block type.

Adding a platform = **one new `ContentDecoder`** in `decode/<name>.rs` + one line in `Registry::with_defaults`; nothing else changes. Decoders are **pure** (no I/O) and never panic on partial input (return `None` → next decoder → `textContent`). Styling is a frontend concern and **user-driven**: the reader has its own consistent theme, customizable by the user (layout, colour theme, per-blog overrides) — decoding is independent of presentation. (Honoring each publication's own `basicTheme` was considered and dropped: one consistent, user-controlled render path is simpler and is what a reader wants.)

## atproto read model

- A reader's **subscriptions live in its own repo**: `listRecords` for `site.standard.graph.subscription`; each record points to a publication AT-URI.
- A document's `site` field is the AT-URI of its owning publication.
- Resolve identity: handle → DID (`com.atproto.identity.resolveHandle`) → PDS (`plc.directory` `serviceEndpoint`) → `listRecords` / `getRecord`. Adding by **handle/DID** discovers every `site.standard.publication` the repo publishes; if there's more than one, the frontend shows a **pick-which-to-follow checklist** rather than silently following all (adding by **handle** uses well-known + DNS-over-HTTPS, `resolve_did`).
- **Cache-first reads, freshen on open.** Opening a post serves the cached decoded body instantly (offline-friendly); the UI then schedules a one-shot background `ToWorker::FreshenDoc` (once per post per session, after the post's image loads) that re-fetches + re-decodes and pushes an updated body **only if the re-decoded `RichDoc` differs** from the cached one — so an author's edit *or* a decoder upgrade both reach already-opened posts without a version stamp. The worker backs off (`Ctx::network_ok`) after a failed freshen so offline navigation never eats repeated timeouts.
- **Lazy, bounded fetching (not exhaustive backfill).** Following a publication does **no** document backfill — it only upserts the publication + writes the follow/subscription. Documents load when a feed is first **opened**: `read::list_documents_window(t, repo, cursor, max_pages)` pulls a bounded window (~3 `listRecords` pages) and returns the resume cursor, which is stored as the repo's **older cursor** (`store::older_cursor`/`set_older_cursor`, keyed by **repo DID** since `listRecords` is repo-wide; `None` = never fetched, `""` = exhausted, non-empty = more). Subsequent opens run the incremental newest-catch-up (`fetch_new_documents`, stopping at the per-publication `sync_cursor`); **load-older** (`ToWorker::LoadOlder`, triggered by `↓` past the bottom of the list) fetches the next window and advances the older cursor. This is the fix for the lock-up where adding a prolific author (~25 blogs in one repo) backfilled every blog's full history in one synchronous worker command — no command does unbounded work now.
- **Publisher-URL resolution** (`read::discover_publication_uri`): a vanity host like `retrobailey.leaflet.pub` is *not* an atproto handle (no `.well-known/atproto-did`, no `_atproto` DNS), so handle resolution fails. Fallback: fetch the page and read its `<link rel="site.standard.publication" href="at://…">` discovery tag — every standard.site page emits one — then `getRecord` that one publication and follow just it (a repo can host several; the URL names one). Still direct-PDS: only the URL→AT-URI hop touches the web page.
- Images: blob CID via `com.atproto.sync.getBlob?did=<did>&cid=<cid>`. `coverImage` is a blob. The default-features `image` build decodes JPEG/PNG/GIF/WebP only; **AVIF** (which GreenGale emits) and other unsupported formats fall back to the **bsky CDN transcode** (`cdn.bsky.app/img/feed_fullsize/plain/<did>/<cid>@jpeg`) — triggered only on local decode failure, so decodable formats stay direct-from-PDS (no pure-Rust AVIF decoder exists worth pulling in; dav1d is a C dep).
- **Direct PDS reads, no aggregator.** A personal-subscriptions reader has a bounded set of publications; firehose indexing (à la docs.surf) is for *global discovery* and is unnecessary here.
- **Known-good test record:** `did:plc:xn3l7ogsxym5ixxugidum5dw` (handle `david.yapfest.club`, PDS `https://yapfest.club`) has both a GreenGale (Markdown) and a Pckt (blocks) document — use it to test decoders/reads.

## Auth

Desktop OAuth uses a loopback redirect (`atrium-oauth`). Its **`client_id`** is the hosted `client_metadata.json` at `https://www.davidlewis.xyz/standard-reader/client_metadata.json` (canonical `www` host — the apex 301-redirects, which a `client_id` must not do); the served copy lives in the **website repo** (`standard-reader/client_metadata.json`), with a matching reference copy at this repo's root. The system browser redirects to `http://127.0.0.1:4599/callback` (a `native`-client loopback redirect — atproto-valid, and `atrium-oauth` doesn't restrict it). The web shell is a separate public client (`web_client_metadata.json`) returning to `/standard-reader/app/`, with acknowledged OPFS session storage. Both request the exact grant `atproto repo:site.standard.graph.subscription?action=create&action=delete`; saved sessions carrying the obsolete broad `transition:generic` grant are revoked and require re-authorization. `build_client` uses the hosted desktop client by default; set **`SR_OAUTH_LOCALHOST=1`** to fall back to the no-hosting dev client (local work, or before the hosted metadata is deployed). The desktop session (DPoP key + tokens) is stored in the **OS keyring** where a native backend exists — macOS Keychain, Windows Credential Manager (pure-Rust, on by default) — falling back to a **`0600` file** under XDG config elsewhere (Linux by default, and anywhere without a secret store: the dev box's Crostini has no Secret Service daemon, so the probe fails there and it uses the file). Linux Secret Service is opt-in via `--features secret-service` (it pulls `dbus-secret-service` → libdbus, a C dep, kept out of every shipped binary). The seam is `SrSessionStore` (`KeyringSessionStore` | `FileSessionStore`) chosen by an availability probe in `build_client`, with a one-time migration of any legacy `session.json` into the keyring. The non-secret `account.json` sidecar (did + handle) stays a plain file.

## Storage & search

`redb` behind the `Store` trait. v1 search is the hand-rolled inverted index in `search.rs` over `textContent` (the spec's purpose-built plaintext field). `tantivy` (pure-Rust, ranked/fuzzy/phrase) is the later drop-in — it slots beside `redb` without touching the engine. Because it's a *cache*, switching backends means a re-fetch, not a migration.

## Conventions

- Rust **edition 2024**.
- Keep `standard-core` **synchronous and dependency-light**, and `standard-frontend` **platform-agnostic** (`ratatui` + `image` only); every heavy/platform dep (`reqwest`/`redb`/`tokio`/`atrium`/`ratatui-image`/`open`) lives in the **shell** (`standard-tui`).
- Decoders are pure functions of their input `Value` → `RichDoc`. Unknown/partial content degrades gracefully (return `None` → next decoder → plaintext fallback), never panics.
- `ratatui-image` auto-detects the terminal graphics protocol (iTerm2 works on the maintainer's hterm box; halfblocks elsewhere).
- This is a personal solo repo: commit directly to `main` when asked; don't push unless asked.

## Build

```
cargo build
cargo test --workspace          # standard-core + standard-frontend + the sr shell
cargo run -p standard-reader    # runs the `sr` binary
```

## Status & roadmap

See **ROADMAP.md** (authoritative). The core engine is real and tested (RichDoc model, decoder `Registry` + `Plaintext` fallback, `AtUri` + `Transport` trait + XRPC builders, `Store` trait, inverted-index search). **All six content decoders are implemented** — Markdown/markpub, Leaflet, Pckt, Offprint, WordPress HTML, Unthread — plus the shared byte-range facet engine and the GreenGale `content_ref` two-phase seam, all validated against live-record fixtures (`cargo test -p standard-core`). The frontend is built too — the `App`/`ui`/`worker` now live in the platform-agnostic **`standard-frontend`** crate, with `standard-tui` the desktop shell supplying the `reqwest`/`redb`/`atrium-oauth`/`ratatui-image` impls (see Architecture): the `ratatui` reader (sidebar + block-flow reader with inline/cover images, search, palette), the worker with `content_ref` fetch-then-decode and blob images, and OAuth sign-in with follow-list ↔ atproto subscription sync. **Customization is implemented** (the v0.1 capstone): four cycleable layouts (one/two/three-pane + drill-down, `\`) with an adjustable sidebar width (`< >`), colour themes (built-in presets + an in-app RGB editor, `t`), and **per-blog overrides** of layout/theme (`b`), all chosen on a first-launch picker and persisted to a human-editable `prefs.toml` in the config dir (loaded by the shell, written via a host `save_prefs` closure the worker invokes on `ToWorker::SavePrefs`; `App` stays I/O-free). `App` holds the *resolved* `theme`/`layout`, recomputed each draw (`recompute_appearance`): per-blog override (keyed on the active `open_pub`) else global, with the editor's live draft winning while open. Author-`basicTheme` styling was considered and dropped in favour of this user-driven path. **1.1.0** made fetching lazy and bounded (publication picker + first-open window + load-older with an end-of-feed affordance; see the atproto read-model section), added **unread badges** (per-feed counts in the sidebar + per-post markers, with read-state plumbed via `Store::read_uris`/`unread_count` and carried on `FromWorker::Docs` alongside a `has_older` flag), **completed Offprint + Leaflet decoder coverage** (all blocks/facets incl. lists, embeds→links, and `Inline::Highlight`; see the content-decoding section), an OS-keyring session store (macOS/Windows native, Linux Secret Service opt-in), a `description` fallback for metadata-only posts, and a reader-layout cache so navigation doesn't re-lay-out the open post each draw.
