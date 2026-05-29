# Changelog

All notable changes to **standard-reader** (`sr`) are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
aims to follow [Semantic Versioning](https://semver.org/). Versions are per-crate: the `sr`
binary (`standard-reader`) and the `standard-core` engine version independently — `standard-core`
stays pre-1.0 until a second frontend validates its `Transport`/`Store` API.

## [1.0.1] - 2026-05-29 — `sr`

### Security
- The OAuth session (DPoP key + tokens) is now stored in the **OS keyring** where a native
  backend exists — macOS **Keychain**, Windows **Credential Manager** — instead of a plaintext
  file, with a one-time migration of any existing `session.json`. Linux uses the `0600` file by
  default; **Linux Secret Service** is opt-in at build time via `--features secret-service` (it
  pulls `libdbus`, a C dependency, so the prebuilt binaries stay pure-Rust).

### Fixed
- Metadata-only documents (no `content`/`textContent` — e.g. publications that keep the full
  article on the web and publish only a stub to atproto, like `atproto.com/blog`) now render
  their `description` blurb instead of a blank reader.
- Declared MSRV corrected to **Rust 1.88** (the code uses let-chains, stable since 1.88).

## [1.0.0] - 2026-05-29 — `sr` (with `standard-core` 0.2.0)

The first stable release: a TUI reader for [standard.site](https://standard.site)
long-form writing on the AT Protocol — online or fully offline.

### Reading
- **Six content decoders + a plaintext fallback** — Markdown/markpub, Leaflet, Pckt, Offprint,
  WordPress HTML, and Unthread all map to one neutral `RichDoc`; unknown content degrades to
  typeset `textContent` rather than failing. Validated against live-record fixtures.
- **Block-flow reader with images** — inline + cover images via `ratatui-image` (iTerm2 graphics
  where available, halfblocks elsewhere), scrolling with the text. Undecodable formats (notably
  AVIF) fall back to the Bluesky CDN transcode; everything else stays direct-from-PDS.
- **Tables, callouts, framed code blocks, and interactive in-post links** (keyboard `n`/`N` +
  click), display-width-correct rendering.
- **Offline cache** (`redb`) — anything opened reads with no network; incremental refresh
  backfills history then walks newest-first to a per-feed high-water mark.
- **Full-text search** across the cache.

### Accounts & feeds
- **Local follow-list, no account required** — add a blog by handle, DID, or publisher URL.
- **OAuth sign-in** (loopback, DPoP/PKCE/PAR) mirroring the follow-list to atproto
  `site.standard.graph.subscription`, with a no-silent-deletes reconciliation prompt.

### Customization
- **Layouts** — one / two / three-pane and a collapsing **drill-down** (feeds → feeds+posts →
  post), cycled with `\`, with independently resizable panes (`< >`).
- **Themes** — built-in presets plus an in-app RGB editor (`t`) with live preview.
- **Per-blog overrides** (`b`) of layout/theme, resolved per-blog-else-global.
- A **first-launch picker**; everything persists to a human-editable `prefs.toml`.

### Platform
- **Prebuilt binaries** for Linux (x86_64 + aarch64), macOS (Apple Silicon), and Windows (x86_64),
  plus `cargo install --git`. Cross-platform config/cache paths (XDG on unix, known folders on
  Windows) and a panic-safe terminal restore.

### Notes
- Author-`basicTheme` styling was considered and **dropped** in favour of the user-driven
  customization above — one consistent, user-controlled render path.
- Requires Rust 1.88+ to build from source.

[1.0.1]: https://github.com/koalabuttz/standard-reader/releases/tag/v1.0.1
[1.0.0]: https://github.com/koalabuttz/standard-reader/releases/tag/v1.0.0
