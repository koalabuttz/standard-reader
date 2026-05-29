# standard-reader (`sr`)

A **TUI reader for [standard.site](https://standard.site)** — long-form
writing published to the AT Protocol (Leaflet, Pckt, Offprint, GreenGale, and any
blog that publishes `site.standard.*` records). Sign in with your atproto account,
pull your subscriptions, and read — with images and real formatting, online or off.

> Status: **1.0.** Add a blog by handle, browse the sidebar →
> document list → reader, and read a block-flow with inline + cover images, search, a command
> palette, and full layout/theme customization — all over an offline cache. Reading needs no
> auth; signing in mirrors your follow-list to atproto subscriptions. No build step beyond
> `cargo`, no runtime services. (RSS support is a later goal.)

## Features

- **Read offline, with real images.** A block-flow reader renders inline + cover images
  (`ratatui-image`; iTerm2 graphics where available, halfblocks elsewhere) over a `redb`
  cache, so anything you've opened reads with no network.
- **Six content decoders + a plaintext fallback** — Markdown/markpub, Leaflet, Pckt,
  Offprint, WordPress HTML, and Unthread all map to one neutral `RichDoc`; unknown content
  degrades to typeset `textContent` rather than failing.
- **Full-text search** across the cache (a hand-rolled inverted index over the spec's
  `textContent` field).
- **Make it yours.** Cycle layouts (one / two / three-pane, or a drill-down) and resize
  the sidebar; pick a built-in colour theme or hand-tune one in an in-app RGB editor; and
  **override layout or theme per blog**. A first-launch picker sets your defaults; everything
  persists to a human-editable `prefs.toml`.
- **Keyboard-first**, with a command palette, `?` help, vim/arrow navigation, and mouse.
  In-post hyperlinks are navigable too — cycle them with `n`/`N` and open with `Enter`, or
  click them straight from the rendered text.
- **A local follow-list, no account required** — add a blog by handle, DID, or URL and it
  persists. Sign in (OAuth) and it **mirrors to atproto** `site.standard.graph.subscription`,
  reconciling local-only follows without silent deletes.
- **Graceful images** — JPEG/PNG/GIF/WebP decode directly from the PDS; formats the default-features
  build can't decode (notably **AVIF**, which GreenGale emits) fall back to the Bluesky CDN's
  transcode-to-JPEG, then cache offline.
- **A portable core.** The engine has zero platform dependencies — a **PS Vita** frontend is
  a stated future goal, reusing all of it.

## Install

**Prebuilt binaries** (no toolchain needed) — grab the archive for your OS from the
[latest release](https://github.com/koalabuttz/standard-reader/releases/latest), unpack, and put
`sr` on your `PATH`. Builds are published for Linux (x86_64 + aarch64), macOS (Apple Silicon),
and Windows (x86_64).

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

## Architecture: a portable core, a swappable frontend

```
crates/
  standard-core/   lib · ZERO platform deps — the whole brain (synchronous)
    model            · the RichDoc AST + Document/Publication/Subscription
    decode           · ContentDecoder trait + per-publisher decoders
    atp              · AT-URI parsing + XRPC request building (over a Transport)
    store            · the Store cache trait
    search           · inverted index over textContent
  standard-tui/    bin `sr` · ratatui + reqwest + redb + OAuth (the impls)
```

The core is **synchronous and I/O-agnostic**. Two traits are the only seam a new
platform must cross:

- **`atp::Transport`** — perform an XRPC GET/POST (and attach auth). Desktop:
  `reqwest`. A PS Vita port: the Vita's net stack.
- **`store::Store`** — the offline cache (docs, read-state, blobs, sync cursors).
  Desktop: `redb`. Elsewhere: whatever fits.

So the hard part — atproto reads, content decoding, caching, search — is written
once and reused; a different platform reimplements only transport, storage, and
drawing the `RichDoc`. The desktop frontend keeps fetches non-blocking by running core
operations on a worker thread and channeling results into the `ratatui` render loop.

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
(built-in presets plus an in-app RGB editor), and override either per blog — all decode the
same structure, so customization only changes presentation.

## Build

```
cargo build
cargo test                     # the full suite (124 tests across both crates)
cargo test -p standard-core    # just the engine
cargo run -p standard-reader   # runs the `sr` binary
```

## OAuth

`client_metadata.json` is the atproto OAuth **`client_id`**, served at
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

## License

[GNU GPL v3](LICENSE) (or later).
