# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repository.

## Project

`standard-reader` (binary **`sr`**) is a lean, polished **TUI reader for [standard.site](https://standard.site)** — long-form writing published to the AT Protocol (Leaflet, Pckt, Offprint, GreenGale, and any blog emitting `site.standard.*` records). Sign in with an atproto account, pull your subscriptions, and read — with images and real formatting, online or **offline**. (RSS support is a later goal.)

**Ethos (load-bearing, mirrors davidlewis.xyz): lean, not bloated.** No build step beyond `cargo`; no runtime services. Keep the dependency surface small and justified; prefer pure-Rust crates. Every dependency should earn its place.

## Architecture — portable core, swappable frontend

A Cargo workspace. The split exists for **portability**: a PS Vita frontend is a stated future goal, so the engine must not assume a desktop.

```
crates/
  standard-core/   lib · ZERO platform deps · SYNCHRONOUS · the whole brain
    model            · RichDoc AST + Document/Publication/Subscription
    decode           · ContentDecoder trait + Registry + per-publisher decoders
    atp              · AtUri parsing + XRPC request building (over a Transport)
    store            · the Store cache trait
    search           · inverted index over textContent
  standard-tui/    bin `sr` · the desktop frontend (ratatui + reqwest + redb + OAuth)
```

**Two traits are the seam** — the only things a new platform implements:

- **`atp::Transport`** — perform an XRPC GET/POST and attach auth. Desktop impl: `reqwest`. A Vita impl: the Vita's net stack. The core *builds* every request URL and *parses* every response; it never opens a socket.
- **`store::Store`** — the offline cache (documents, read-state, blobs, sync cursors). Desktop impl: `redb`.

**Hard rule: never let `tokio`, `reqwest`, `redb`, `ratatui`, or `async` into `standard-core`.** The core is synchronous. The desktop frontend gets non-blocking fetches by running core operations on a worker thread and channeling results into the `ratatui` render loop; a Vita frontend calls core inline. Auth is also a frontend concern (the Vita would likely use an app-password instead of the loopback flow).

Pipeline: **`atp`** builds/parses XRPC → **`decode`** maps each publisher's `content` lexicon to one `RichDoc` → **`store`** caches it for offline → **`search`** indexes `textContent`.

## Content decoding (validated against real records)

`site.standard.document.content` is an **open union** — each publisher embeds its own lexicon. `textContent` is flat plaintext (the spec says it carries *no* formatting), so it is a **fallback only**. Decoders dispatch on `content.$type` and all target the one neutral `RichDoc` AST:

Shapes below were validated against **live records** (the published survey had several wrong field names). All decoders are ✅ implemented and tested against fixtures in `crates/standard-core/tests/fixtures/`:

| `content.$type`                          | shape                                            | decoder      |
| ---------------------------------------- | ------------------------------------------------ | ------------ |
| *(bare string)* / `at.markpub.markdown`  | Markdown (GreenGale body, Sequoia, markpub)      | `Markdown` (pulldown-cmark) |
| `pub.leaflet.content`                    | `pages[].blocks[].block` + byte-range facets     | `Leaflet`    |
| `blog.pckt.content`                      | `items: [blog.pckt.block.*]`                     | `Pckt`       |
| `app.offprint.content`                   | `items: [app.offprint.block.*]` + byte-range facets | `Offprint` |
| `org.wordpress.html`                     | `{ html }` — rendered HTML (`tl` walker)         | `Wordpress`  |
| `at.unthread.content`                    | `{ content }` — a Markdown string (Unthread)     | `Unthread` (reuses `from_markdown`) |
| `*#contentRef`                           | **reference** to another record (GreenGale)      | `content_ref` → two-phase |
| *(unknown / absent)*                     | typeset `textContent`                            | `Plaintext`  |

Leaflet/Pckt/Offprint share one **byte-range facet engine** (`decode/facets.rs`): each carries `{ index:{byteStart,byteEnd}, features:[{$type}] }` over a `plaintext` string, differing only by namespace + `#suffix`. **GreenGale is two-phase**: `site.standard.document.content` is a `#contentRef` pointing at an `app.greengale.document` whose own `content` is the bare Markdown string — the core's `content_ref()` returns the AT-URI; the frontend fetches it and re-runs `decode`. Block decoders need the owning repo DID (passed via `DecodeCtx`) to build blob image refs. **Pckt `gallery` is the same two-phase idea at block granularity:** the decoder emits a `Block::GalleryRef { uri }` placeholder and `read::get_document` fetches the `blog.pckt.gallery` record (`{ images: [{ blob }] }`) and splices in a resolved `Block::ImageGrid` — so no `GalleryRef` ever reaches a frontend.

Adding a platform = **one new `ContentDecoder`** in `decode/<name>.rs` + one line in `Registry::with_defaults`; nothing else changes. Decoders are **pure** (no I/O) and never panic on partial input (return `None` → next decoder → `textContent`). Two render modes (a frontend concern): **uniform** (the reader's own consistent theme) and **author's** (honor each publication's `basicTheme`) — both decode the same structure; the mode only changes theming.

## atproto read model

- A reader's **subscriptions live in its own repo**: `listRecords` for `site.standard.graph.subscription`; each record points to a publication AT-URI.
- A document's `site` field is the AT-URI of its owning publication.
- Resolve identity: handle → DID (`com.atproto.identity.resolveHandle`) → PDS (`plc.directory` `serviceEndpoint`) → `listRecords` / `getRecord`. Adding by **handle/DID** follows the whole repo (every `site.standard.publication` it publishes); adding by **handle** uses well-known + DNS-over-HTTPS (`resolve_did`).
- **Publisher-URL resolution** (`read::discover_publication_uri`): a vanity host like `retrobailey.leaflet.pub` is *not* an atproto handle (no `.well-known/atproto-did`, no `_atproto` DNS), so handle resolution fails. Fallback: fetch the page and read its `<link rel="site.standard.publication" href="at://…">` discovery tag — every standard.site page emits one — then `getRecord` that one publication and follow just it (a repo can host several; the URL names one). Still direct-PDS: only the URL→AT-URI hop touches the web page.
- Images: blob CID via `com.atproto.sync.getBlob?did=<did>&cid=<cid>`. `coverImage` is a blob. The lean `image` build decodes JPEG/PNG/GIF/WebP only; **AVIF** (which GreenGale emits) and other unsupported formats fall back to the **bsky CDN transcode** (`cdn.bsky.app/img/feed_fullsize/plain/<did>/<cid>@jpeg`) — triggered only on local decode failure, so decodable formats stay direct-from-PDS (no pure-Rust AVIF decoder exists worth pulling in; dav1d is a C dep).
- **Direct PDS reads, no aggregator.** A personal-subscriptions reader has a bounded set of publications; firehose indexing (à la docs.surf) is for *global discovery* and is unnecessary here.
- **Known-good test record:** `did:plc:xn3l7ogsxym5ixxugidum5dw` (handle `david.yapfest.club`, PDS `https://yapfest.club`) has both a GreenGale (Markdown) and a Pckt (blocks) document — use it to test decoders/reads.

## Auth

OAuth via loopback redirect (`atrium-oauth`). The **`client_id`** is the hosted `client_metadata.json` at `https://www.davidlewis.xyz/standard-reader/client_metadata.json` (canonical `www` host — the apex 301-redirects, which a `client_id` must not do); the served copy lives in the **website repo** (`standard-reader/client_metadata.json`), with a matching reference copy at this repo's root. The browser redirects to `http://127.0.0.1:4599/callback` (a `native`-client loopback redirect — atproto-valid, and `atrium-oauth` doesn't restrict it). `build_client` uses this hosted client by default; set **`SR_OAUTH_LOCALHOST=1`** to fall back to the no-hosting dev client (local work, or before the metadata is deployed). Store the session in a **`0600` file** under XDG config (the dev box's Crostini lacks a Secret Service daemon, so the `keyring` crate fails there; keep keyring opt-in).

## Storage & search

`redb` behind the `Store` trait. v1 search is the hand-rolled inverted index in `search.rs` over `textContent` (the spec's purpose-built plaintext field). `tantivy` (pure-Rust, ranked/fuzzy/phrase) is the later drop-in — it slots beside `redb` without touching the engine. Because it's a *cache*, switching backends means a re-fetch, not a migration.

## Conventions

- Rust **edition 2024**.
- Keep `standard-core` **synchronous and dependency-light**; heavy/platform deps live only in frontends.
- Decoders are pure functions of their input `Value` → `RichDoc`. Unknown/partial content degrades gracefully (return `None` → next decoder → plaintext fallback), never panics.
- `ratatui-image` auto-detects the terminal graphics protocol (iTerm2 works on the maintainer's hterm box; halfblocks elsewhere).
- This is a personal solo repo: commit directly to `main` when asked; don't push unless asked.

## Build

```
cargo build
cargo test -p standard-core
cargo run -p standard-reader   # runs the `sr` binary
```

## Status & roadmap

See **ROADMAP.md** (authoritative). The core engine is real and tested (RichDoc model, decoder `Registry` + `Plaintext` fallback, `AtUri` + `Transport` trait + XRPC builders, `Store` trait, inverted-index search). **All six content decoders are implemented** — Markdown/markpub, Leaflet, Pckt, Offprint, WordPress HTML, Unthread — plus the shared byte-range facet engine and the GreenGale `content_ref` two-phase seam, all validated against live-record fixtures (`cargo test -p standard-core`). The `standard-tui` frontend is built too: the `ratatui` reader (sidebar + block-flow reader with inline/cover images, search, palette), the `reqwest`/`redb` worker with `content_ref` fetch-then-decode and blob images, and OAuth sign-in with follow-list ↔ atproto subscription sync.
