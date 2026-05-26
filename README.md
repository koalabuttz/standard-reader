# standard-reader (`sr`)

A lean, polished **TUI reader for [standard.site](https://standard.site)** — long-form
writing published to the AT Protocol (Leaflet, Pckt, Offprint, GreenGale, and any
blog that publishes `site.standard.*` records). Sign in with your atproto account,
pull your subscriptions, and read — with images and real formatting, online or off.

> Status: **scaffold.** The engine (`standard-core`) is taking shape; the terminal
> frontend (`sr`) comes next. No build step beyond `cargo`, no runtime services.

## Architecture: a portable core, a swappable frontend

```
crates/
  standard-core/   lib · ZERO platform deps — the whole brain (sync, lean)
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
drawing the `RichDoc`.

## Content decoding

`site.standard.document.content` is an open union; each publisher embeds its own
lexicon. Decoders map them all to one `RichDoc`:

| `content.$type`     | shape                                  | decoder      |
| ------------------- | -------------------------------------- | ------------ |
| *(bare string)*     | Markdown (GreenGale, Sequoia, markpub) | `Markdown`   |
| `pub.leaflet.*`     | blocks + facets                        | `Leaflet`    |
| `blog.pckt.content` | `items: [blog.pckt.block.*]`           | `Pckt`       |
| *(unknown/absent)*  | typeset `textContent`                  | `Plaintext`  |

Two render modes: **uniform** (the reader's consistent theme) and **author's**
(honoring each publication's `basicTheme`).

## Build

```
cargo build
cargo test -p standard-core
cargo run -p standard-reader   # runs the `sr` binary
```

## OAuth

`client_metadata.json` is the atproto OAuth **`client_id`**, served at
`https://davidlewis.xyz/standard-reader/client_metadata.json`. Login uses the
loopback redirect (`http://127.0.0.1:4599/callback`). The file here is
**provisional** — validate `redirect_uris`/`scope` against `atrium-oauth` before
going live.

## License

MIT
