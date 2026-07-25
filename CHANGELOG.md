# Changelog

All notable changes to **standard-reader** (`sr`) are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
aims to follow [Semantic Versioning](https://semver.org/). Versions are per-crate: the `sr`
binary (`standard-reader`) and the `standard-core` engine version independently — `standard-core`
stays pre-1.0 until a second frontend validates its `Transport`/`Store` API.

## [Unreleased]

### Added
- **Browser/WASM shell through Milestone 2.** The shared frontend now runs in the browser via
  ratzilla, with network work isolated on a Web Worker, native `<img>` overlays, and an OPFS-backed
  offline cache for documents, images, follows, read state, cursors, and appearance preferences.
- **Browser mouse support.** Links and list rows are clickable, and wheel/trackpad gestures scroll
  the pane under the pointer without moving the surrounding page.
- **“Published with …” attribution.** Structured posts identify their authoring platform in the
  reader's bottom border; the platform name is a fully keyboard- and mouse-accessible link.
- **WASM CI coverage.** CI now runs the web shell's host-side logic tests, builds its optimized
  WASM distribution with pinned Rust/Trunk versions, and uploads the packaged site as an artifact.

### Fixed
- Browser image overlays now hide while dialogs are open, so customization and other popups always
  render above article images.
- URL-backed images now use stable OPFS-safe filenames. Previously their `/` characters made the
  persistence write fail, so those images could disappear after a reload.
- A successful explicit refresh or uncached document fetch now re-enables background freshening
  after an earlier offline failure.
- Browser panel corners use square box-drawing glyphs, avoiding disconnected rounded hooks in
  common browser monospace fonts.

### Security
- Updated `quinn-proto`, `crossbeam-epoch`, and `anyhow` to patched versions identified by the
  RustSec audit.

## [1.1.1] - 2026-05-30 — `sr` (engine `standard-core` unchanged at 0.3.0)

A packaging-only patch — the `sr` binary is functionally identical to 1.1.0.

### Fixed
- **Prebuilt Linux binaries now run on any distro**, not just glibc ≥ 2.39. The Linux release
  builds are now **static musl** binaries (the build is pure-Rust, so this costs nothing), fixing
  the `version 'GLIBC_2.39' not found` failure on older systems (Debian 12, Ubuntu 22.04, RHEL 9,
  Crostini, …).

### Added
- A combined **`SHA256SUMS`** asset on each release, for verifying downloads.

## [1.1.0] - 2026-05-30 — `sr` (with `standard-core` 0.3.0)

### Added
- **Pick which blogs to follow.** Adding a handle/DID that publishes more than one
  publication now shows a checklist (Space to toggle, `a` all, `n` none, Enter to follow) instead
  of silently following every blog in the repo.
- **Load older posts on demand** — press `↓` past the bottom of a feed's list to fetch the next
  window of older posts. The list shows an end-of-feed affordance (`↓ load older posts` when more
  remain, `— end —` when exhausted), and the footer hints it.
- **Unread badges.** Per-feed unread counts in the sidebar and an unread dot beside each unread
  post (read posts dim); both update live as you read.
- **Posts freshen on open.** Opening a post still renders instantly from cache, but now a
  background re-fetch (once per post per session) updates it in place if the author edited it — or
  if our decoder improved — so cached posts aren't frozen at first-read. Offline-safe: the cached
  copy stands, and freshening backs off while the network is unreachable.
- **Complete Offprint + Leaflet coverage.** Every Offprint block now decodes — blockquotes, code
  blocks, ordered & task lists (with ☑/☐), image carousels and before/after diffs, and web
  embeds/bookmarks — plus the `highlight`, `@mention`, and `webMention` text styles. Leaflet's
  lists and embeds (website / Bluesky post / button / post reference), previously dropped, now
  render too. Embeds a terminal can't host become clickable links (reusing the link machinery);
  highlighted text shows in its authored colour (a per-hue background wash, matching Offprint's
  highlight), and `@mentions` link to the author's Bluesky profile.
- **Text & image alignment.** Center/right `textAlign` on paragraphs/headings and `alignment` on
  images now render aligned instead of flush-left (images already centered by default; an explicit
  "left" is honored too).

### Changed
- **Lazy, bounded fetching.** Following a blog no longer backfills its entire history up front (a
  prolific author with many blogs could lock the app up while it pulled everything). Posts now load
  in a bounded recent window when you first open a feed — like an RSS reader — with older posts via
  load-older. Adding/importing many feeds stays responsive.
- **Faster navigation.** The reader pane caches its computed layout, so scrolling and moving around
  the sidebar no longer re-lay-out the open post every keystroke.

### Security
- The OAuth session (DPoP key + tokens) is now stored in the **OS keyring** where a native
  backend exists — macOS **Keychain**, Windows **Credential Manager** — instead of a plaintext
  file, with a one-time migration of any existing `session.json`. Linux uses the `0600` file by
  default; **Linux Secret Service** is opt-in at build time via `--features secret-service` (it
  pulls `libdbus`, a C dependency, so the prebuilt binaries stay pure-Rust).

### Fixed
- Metadata-only documents (no `content`/`textContent` — e.g. publications that keep the full
  article on the web and publish only a stub to atproto, like `atproto.com/blog`) now render
  their `description` blurb, with a hint to press `o` for the full post, instead of a blank reader.
- Upgrading no longer surfaces a cache deserialization error. A one-time cache-format check
  re-fetches cached post bodies when the decoded-content format changes (your follows and
  downloaded images are kept) instead of a stale entry failing to load.
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

[1.1.1]: https://github.com/koalabuttz/standard-reader/releases/tag/v1.1.1
[1.1.0]: https://github.com/koalabuttz/standard-reader/releases/tag/v1.1.0
[1.0.0]: https://github.com/koalabuttz/standard-reader/releases/tag/v1.0.0
[Unreleased]: https://github.com/koalabuttz/standard-reader/compare/v1.1.1...HEAD
