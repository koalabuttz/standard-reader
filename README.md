# standard-reader (`sr`)

A **TUI reader for [standard.site](https://standard.site)** — long-form
writing published to the AT Protocol (Leaflet, Pckt, Offprint, GreenGale, and any
blog that publishes `site.standard.*` records). Sign in with your atproto account,
pull your subscriptions, and read — with images and real formatting, online or off.

> Status: **1.2.0**, with the browser shell complete through Milestone 3. Add a blog by handle, browse the sidebar →
> document list → reader, and read a block-flow with inline + cover images, search, a command
> palette, and full layout/theme customization — all over an offline cache. Reading needs no
> auth; signing in mirrors your follow-list to atproto subscriptions. No build step beyond
> `cargo`, no runtime services. (RSS support is a later goal.)

**[Try the web reader](https://www.davidlewis.xyz/standard-reader/app/)** — no installation
required; public reading, offline storage, and narrowly scoped atproto sign-in are available.

## Features

- **Read offline, with real images.** A block-flow reader renders inline + cover images
  (`ratatui-image`; iTerm2 graphics where available, halfblocks elsewhere) over a `redb`
  cache, so anything you've opened reads with no network.
- **Fetches like an RSS reader.** Following a blog doesn't backfill its whole history — opening a
  feed pulls a bounded recent window (older posts on demand with `↓`), so adding a prolific author
  with many blogs stays snappy. Open posts render instantly from cache, then freshen in the
  background if the author edited them. **Unread counts** sit beside each feed, with a dot on every
  unread post.
- **Six content decoders + a plaintext fallback** — Markdown/markpub, Leaflet, Pckt,
  Offprint, WordPress HTML, and Unthread all map to one neutral `RichDoc`; unknown content
  degrades to typeset `textContent` rather than failing.
- **Full-text search** across the cache (a hand-rolled inverted index over the spec's
  `textContent` field).
- **Make it yours.** Cycle layouts (one / two / three-pane, or a drill-down) and resize
  the sidebar; pick a built-in colour theme or hand-tune one in an in-app RGB editor; and
  **override layout or theme per blog**, including a separate custom palette owned by each
  publication. The global custom theme remains a distinct default. A first-launch picker sets your
  defaults; everything persists to a human-editable `prefs.toml`.
- **Keyboard-first**, with a command palette, `?` help, vim/arrow navigation, and mouse.
  In-post hyperlinks are navigable too — cycle them with `n`/`N` and open with `Enter`, or
  click them straight from the rendered text.
- **A local follow-list, no account required** — add a blog by handle, DID, or URL and it
  persists; a handle that publishes several blogs lets you **pick which to follow**. Sign in (OAuth)
  and it **mirrors to atproto** `site.standard.graph.subscription`, reconciling local-only follows
  without silent deletes.
- **Graceful images** — JPEG/PNG/GIF/WebP decode directly from the PDS; formats the default-features
  build can't decode (notably **AVIF**, which GreenGale emits) fall back to the Bluesky CDN's
  transcode-to-JPEG, then cache offline.
- **A portable core *and* frontend.** The engine and the App/UI/worker carry no platform stack,
  behind a small set of seam traits. The **browser/WASM shell** already reuses them (public reading,
  local follows, native image overlays, OPFS offline persistence, and browser OAuth with subscription
  writes). A **PS Vita** frontend can reuse the same seams.

## Install

**Prebuilt binaries** (no toolchain needed) — grab the archive for your OS from the
[latest release](https://github.com/koalabuttz/standard-reader/releases/latest), unpack, and put
`sr` on your `PATH`. Builds are published for Linux (x86_64 + aarch64), macOS (Apple Silicon),
and Windows (x86_64); a `SHA256SUMS` is attached to verify them. The Linux binaries are **static
musl**, so they run on any distro regardless of glibc version. On macOS the binary is unsigned —
clear the quarantine flag with `xattr -dr com.apple.quarantine sr` if Gatekeeper blocks it.

**From source** (needs Rust 1.88+):

```
cargo install --git https://github.com/koalabuttz/standard-reader
```

## Usage

```
cargo run -p standard-reader            # run from a clone (binary: sr)
```

```
sr                      launch the interactive reader
sr fetch <handle|did>   (debug) fetch + decode + cache, print to stdout
sr cached               (debug) render the local cache, no network
```

Keys in the reader:

| key | action | key | action |
| --- | ------ | --- | ------ |
| `a` | add a feed (handle / DID / URL) | `Enter` | open feed / post / focused link |
| `/` | search across feeds | `Tab` / `Esc` | cycle / step back pane focus |
| `:` / `Ctrl-P` | command palette | `j`/`k`, `↑`/`↓` | move selection / scroll |
| `r` | refresh the selected feed | `g` / `G` | top / bottom |
| `d` | unfollow the selected feed | `PgUp`/`PgDn` | scroll ±10 |
| `m` | mark the open post read | `o` | open the post in a browser |
| `n` / `N` | focus next / prev link | click | open link / select a row |
| `t` | theme (presets + RGB editor) | `\` | cycle layout (1/2/3-pane, drill-down) |
| `b` | customize this blog | `<` / `>` | narrow / widen the focused pane |
| `i` | toggle images (text-only) | `L` | sign in / out (atproto OAuth) |
| `?` | help | `q` | quit |

## Architecture: a portable core, a portable frontend, a swappable shell

```
crates/
  standard-core/      lib · ZERO platform deps — the whole brain (synchronous)
    model               · the RichDoc AST + Document/Publication/Subscription
    decode              · ContentDecoder trait + per-publisher decoders
    atp                 · AT-URI parsing + XRPC request building (over a Transport)
    store               · the Store cache trait
    search              · inverted index over textContent
  standard-frontend/  lib · platform-agnostic — the App state machine, ratatui UI, and worker
  standard-tui/       bin `sr` · the desktop shell — reqwest + redb + atrium-oauth + ratatui-image
  standard-reader-web/ bin · browser shell — ratzilla + Web Worker + XHR + OPFS + native images
```

The core is **synchronous and I/O-agnostic**, and the frontend is **platform-agnostic**
(`ratatui` + `image` only). A new platform writes a thin **shell** that crosses a handful of
seams:

- **`Transport`** / **`Store`** (core) — perform an XRPC GET/POST, and the offline cache.
  Desktop: `reqwest` + `redb`; browser: worker-side synchronous XHR + an OPFS-persisted memory store.
- **`FrontendStore`** / **`AuthProvider`** / **`ImageSink`** (frontend) — the follow-list,
  sign-in + subscription writes, and "paint an image into a cell rect." Desktop: `redb`
  follows, `atrium-oauth`, `ratatui-image`; browser: OPFS-backed follows and OAuth,
  `atrium-oauth`, native `<img>`s.

So the hard part — atproto reads, content decoding, caching, search, the whole reader UI — is
written once and reused; a different platform (the existing **browser/WASM** shell, or a future
**PS Vita** port) reimplements only those seams. The desktop shell keeps fetches non-blocking by running the
synchronous worker on its own thread, channeling results into the `ratatui` render loop.

## Content decoding

`site.standard.document.content` is an open union; each publisher embeds its own
lexicon. `textContent` is flat plaintext (a fallback only). Decoders dispatch on
`content.$type` and map them all to one `RichDoc`:

| `content.$type`                         | shape                                          | decoder     |
| --------------------------------------- | ---------------------------------------------- | ----------- |
| *(bare string)* / `at.markpub.markdown` | Markdown (GreenGale body, Sequoia, markpub)    | `Markdown`  |
| `pub.leaflet.content`                   | `pages[].blocks[].block` + byte-range facets   | `Leaflet`   |
| `blog.pckt.content`                     | `items: [blog.pckt.block.*]` + facets          | `Pckt`      |
| `app.offprint.content`                  | blocks + byte-range facets                     | `Offprint`  |
| `org.wordpress.html`                    | `{ html }` — rendered HTML                     | `Wordpress` |
| `at.unthread.content`                   | `{ content }` — a Markdown string              | `Unthread`  |
| `*#contentRef`                          | **reference** to another record (GreenGale)    | two-phase   |
| *(unknown / absent)*                    | typeset `textContent`                          | `Plaintext` |

Leaflet/Pckt/Offprint share one byte-range **facet engine**. GreenGale is **two-phase**:
the `#contentRef` names an AT-URI the frontend fetches and re-decodes (the same idea, at
block granularity, resolves Pckt galleries). Adding a platform is one new `ContentDecoder`
plus one line in the registry; decoders are pure and never panic on partial input.

Styling is the reader's own, and **yours to customize**: pick a layout (one/two/three-pane
or a drill-down, with independently resizable panes), pick or hand-edit a colour theme
(built-in presets plus an in-app RGB editor), and override either per blog with an independent
custom palette — all decode the same structure, so customization only changes presentation.

## Build

```
cargo build
cargo test --workspace         # the full suite (188 tests across four crates)
cargo test -p standard-core    # just the engine
cargo run -p standard-reader   # runs the `sr` binary

# Browser release build (nightly is selected by the web crate's rust-toolchain.toml)
cd crates/standard-reader-web
trunk build --release --locked
# dist/ is ready for https://www.davidlewis.xyz/standard-reader/app/
```

Production deployment, required Cloudflare headers, caching, and acceptance checks are documented
in [`docs/WEB_DEPLOYMENT.md`](docs/WEB_DEPLOYMENT.md).

## OAuth

The desktop's `client_metadata.json` is its atproto OAuth **`client_id`**, served at
`https://www.davidlewis.xyz/standard-reader/client_metadata.json` — the canonical `www`
host, because the apex 301-redirects and a `client_id` must not redirect. (A reference copy
lives at this repo's root; the served copy is in the website repo.) Login uses the loopback
redirect to `http://127.0.0.1:4599/callback` (DPoP/PKCE/PAR via `atrium-oauth`).

The session (DPoP key + tokens) is stored in the **OS keyring** where a native backend exists —
macOS Keychain, Windows Credential Manager — falling back to a **`0600` file under XDG config**
elsewhere (Linux by default, and anywhere without a secret store, e.g. headless or Crostini; on
Windows the file fallback relies on per-user-profile ACLs, not `0600`). Linux Secret Service is
opt-in at build time via `--features secret-service` (it pulls `libdbus`, a C dep, so the
prebuilt binaries don't). The non-secret account sidecar (handle + DID) stays a plain file. Set
**`SR_OAUTH_LOCALHOST=1`** to fall back to the no-hosting dev client (local work, or before the
metadata is deployed). Reading public feeds needs no auth — sign-in only enables write/sync of
subscriptions.

The browser is a separate public OAuth client at
`https://www.davidlewis.xyz/standard-reader/web_client_metadata.json`, redirecting back to
`/standard-reader/app/`. It uses the same DPoP/PKCE/PAR protocol and subscription reconciliation,
but keeps its tokens, private DPoP key, and pending authorization state in an acknowledged
origin-private `auth.json` file. Sign-in therefore survives reloads without adding a backend or
putting credentials in localStorage. An origin-scoped Web Lock allows only one auth-capable tab at
a time, preventing races over single-use refresh tokens; other tabs can still read normally. Both
clients request only identity plus create/delete access to
`site.standard.graph.subscription`; an older `transition:generic` session is revoked and must
re-authorize rather than retaining app-password-level access.

## License

[GNU GPL v3](LICENSE) (or later).
