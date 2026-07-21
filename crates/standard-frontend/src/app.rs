//! UI state and the update logic. The `App` is a pure state machine: it turns key/mouse
//! events into state transitions plus [`ToWorker`] commands, and folds [`FromWorker`]
//! results back in. It owns no I/O (the worker does) and renders from this snapshot.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;

use image::{DynamicImage, GenericImageView};
use ratatui::layout::Rect;

use crate::input::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

use standard_core::model::{
    Block, Document, ImageSource, Inline, Publication, PublishingPlatform, RichDoc,
};

use crate::account::Account;
use crate::prefs::{LayoutKind, PANE_MAX, PANE_MIN, Prefs};
use crate::ui::theme::{PRESETS, Theme, ThemeColors};
use crate::worker::{FromWorker, ToWorker};

/// Stable cache key for an image source (the blob CID, or the URL).
pub fn image_key(source: &ImageSource) -> String {
    match source {
        ImageSource::Blob { cid, .. } => cid.clone(),
        ImageSource::Url(url) => url.clone(),
    }
}

/// A decoded image plus its pixel dimensions. Portable: the per-display-size encode cache
/// (terminal slices on desktop, an overlay element on web) lives in the [`crate::image_sink::ImageSink`],
/// not here, so `App` holds no platform-specific image-protocol state.
pub struct StoredImage {
    pub image: DynamicImage,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    /// The feed list.
    Sidebar,
    /// The post list (a distinct focus once feeds and posts can be visible at once).
    Posts,
    /// The reader body.
    Reader,
}

/// Shape used for panel corners. Shells can select the form their renderer/font handles best
/// without making the user's persisted appearance platform-specific.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PanelBorderStyle {
    #[default]
    Rounded,
    Square,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Browse,
    DocList,
    Search,
    Palette,
    Help,
    AddFeed,
    /// Prompt for a handle/DID to sign in via OAuth.
    SignIn,
    /// Reconcile local-only follows against the account's atproto subscriptions.
    SyncPrompt,
    /// Pick a colour theme (built-in presets + "custom").
    ThemePicker,
    /// Edit the custom theme's colours (an RGB picker), with live preview.
    ThemeEditor,
    /// Pick a layout (one / two / three-pane / drill-down).
    LayoutPicker,
    /// Per-blog customization menu (theme / layout / use-global) for one publication.
    BlogMenu,
    /// Show the full (untruncated) status line in a popup — opened by clicking the footer.
    StatusDetail,
    /// Choose which of a repo's publications to follow (checklist), after adding a multi-blog repo.
    PublicationPicker,
}

/// Which pane-width field a resize targets (see [`App::focused_pane_width`]).
enum PaneWidth {
    Sidebar,
    Posts,
}

/// The in-app colour editor's transient state: a working palette plus which slot/channel is
/// selected. Lives on `App` while [`Mode::ThemeEditor`] is active; committed to `prefs.custom`
/// on Enter, discarded on Esc.
pub struct ThemeEditor {
    pub draft: ThemeColors,
    /// Selected slot, 0..7 (indexes [`theme::SLOTS`](crate::ui::theme::SLOTS)).
    pub slot: usize,
    /// Selected RGB channel, 0..3 (R, G, B).
    pub channel: usize,
}

/// Palette actions (also reachable by their direct keys).
#[derive(Clone, Copy)]
pub enum Action {
    AddFeed,
    Search,
    Refresh,
    MarkRead,
    Theme,
    Layout,
    SignIn,
    SignOut,
    Help,
    Quit,
}

impl Action {
    pub const ALL: [Action; 10] = [
        Self::AddFeed,
        Self::Search,
        Self::Refresh,
        Self::MarkRead,
        Self::Theme,
        Self::Layout,
        Self::SignIn,
        Self::SignOut,
        Self::Help,
        Self::Quit,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::AddFeed => "Add feed",
            Action::Search => "Search",
            Action::Refresh => "Refresh feed",
            Action::MarkRead => "Mark read",
            Action::Theme => "Theme…",
            Action::Layout => "Layout…",
            Action::SignIn => "Log in",
            Action::SignOut => "Log out",
            Action::Help => "Help",
            Action::Quit => "Quit",
        }
    }
}

/// Pane rectangles from the last render, for mouse hit-testing. Reset to `default()` (zero-area)
/// at the top of every draw, then set only for the panes that layout actually shows — so a click
/// is tested against all three and absent panes (zero-area) never match.
#[derive(Default, Clone, Copy)]
pub struct Rects {
    pub sidebar: Rect,
    pub posts: Rect,
    pub reader: Rect,
    /// Click target for the linked platform name in the reader's bottom border.
    pub attribution: Rect,
    /// The status-text region of the footer (left side only), for click-to-expand. The hints on
    /// the right are deliberately *not* covered, so clicking them doesn't open the status popup.
    pub status: Rect,
}

/// One row of a hyperlink's on-screen footprint in the reader body, in *virtual* document
/// coordinates (`row` is the pre-scroll document row). A link that wraps across rows contributes
/// one `LinkRect` per row, all sharing the same `idx` so any of them resolves back to the same
/// href. Filled by the reader each draw; used for click hit-testing and scroll-to-focus. `idx`
/// indexes [`App::links`].
#[derive(Clone, Copy)]
pub struct LinkRect {
    pub idx: usize,
    pub row: u16,
    pub col: u16,
    pub width: u16,
}

pub struct App {
    pub mode: Mode,
    pub focus: Focus,
    pub feeds: Vec<Publication>,
    pub feed_sel: usize,
    pub docs: Vec<Document>,
    pub doc_sel: usize,
    /// The publication whose documents are currently shown — guards against a late
    /// network update landing after the user has navigated to a different feed.
    pub open_pub: Option<String>,
    pub list_title: String,
    pub reading: Option<RichDoc>,
    pub reading_title: String,
    pub reading_uri: Option<String>,
    /// Publishing application for the open document, when its content lexicon identifies one.
    pub reading_platform: Option<PublishingPlatform>,
    /// The open doc's `description`, kept so the reader can recognise a metadata-only post (whose
    /// body is just the description blurb) and append a "press o to open" hint.
    pub reading_description: Option<String>,
    /// The open document's cover image source, rendered atop the reader.
    pub reading_cover: Option<ImageSource>,
    pub scroll: u16,
    pub input: String,
    pub palette_sel: usize,
    pub status: String,
    pub loading: bool,
    pub should_quit: bool,
    pub rects: Rects,
    /// Host-selected panel-corner style. Rounded remains the portable default; browser DOM font
    /// rendering can opt into square corners when rounded box-drawing glyphs rasterize poorly.
    pub panel_border_style: PanelBorderStyle,
    /// Decoded images, keyed by [`image_key`]. The per-size encode/overlay state lives in the
    /// frontend's [`crate::image_sink::ImageSink`], not here.
    pub images: HashMap<String, StoredImage>,
    /// Text-only toggle: when false, images aren't fetched and render as placeholders.
    pub show_images: bool,
    /// The signed-in account, or `None` when signed out.
    pub account: Option<Account>,
    /// Local-only follows awaiting reconciliation `(publication_uri, name)`; drives `SyncPrompt`.
    pub sync_prompt: Vec<(String, String)>,
    /// Hyperlinks in the open document, in reading order — the navigable set for `n`/`N`/Enter/click.
    pub links: Vec<String>,
    /// Focused link (index into `links`) for keyboard navigation + `Enter` to open.
    pub focused_link: Option<usize>,
    /// Index of the border-attribution link in `links` (always after the body links), if present.
    pub attribution_link: Option<usize>,
    /// Whether the current reader width can render the full or compact attribution. Maintained by
    /// the renderer so keyboard navigation never lands on an invisible border control.
    pub attribution_visible: bool,
    /// Set when a focus change should scroll the focused link into view (honored on the next draw).
    pub scroll_to_focused: bool,
    /// Link rectangles from the last render, for click hit-testing (filled by the reader).
    pub link_rects: Vec<LinkRect>,
    /// Persisted user preferences (layout / theme / per-blog overrides). The canonical copy;
    /// mutated on user action and written back via `ToWorker::SavePrefs`.
    pub prefs: Prefs,
    /// The *resolved, effective* theme for the current view (per-blog override else global, or
    /// the editor's live draft). Recomputed each draw by [`App::recompute_appearance`].
    pub theme: Theme,
    /// The *resolved, effective* layout for the current view (per-blog override else global).
    pub layout: LayoutKind,
    /// Cached reader-pane layout; reused across draws unless an input changed (see the reader's
    /// `ReaderKey`). Lets scroll + sidebar nav skip the expensive re-layout.
    pub(crate) reader_cache: Option<crate::ui::reader::ReaderLayout>,
    /// Bumped whenever the open document's body changes — part of the reader cache key.
    pub reading_version: u64,
    /// Bumped whenever an image loads (which can reflow the layout) — part of the reader cache key.
    pub images_version: u64,
    /// Open colour editor, while [`Mode::ThemeEditor`] is active (drives live preview).
    pub theme_editor: Option<ThemeEditor>,
    /// Selection index for the list-style pickers (theme / layout / per-blog).
    pub menu_sel: usize,
    /// When a theme/layout picker targets a specific publication (per-blog override), its AT-URI;
    /// `None` means the picker edits the global default.
    pub menu_target: Option<String>,
    /// True during the first-launch flow (layout picker → theme picker), so each picker advances
    /// to the next step instead of returning to the reader.
    pub onboarding: bool,
    /// Candidates for the publication picker: `(uri, name, selected)`; drives `PublicationPicker`.
    pub publication_choices: Vec<(String, String, bool)>,
    /// Which of the **current feed's** documents are read (drives the unread markers). Updated live
    /// when a post is opened or marked read, and replaced wholesale on each `Docs` from the worker.
    pub read_uris: HashSet<String>,
    /// Per-feed unread count (publication uri → count) for the sidebar badges; from the worker, kept
    /// live by local decrements as posts are read.
    pub unread_counts: HashMap<String, usize>,
    /// Whether the open feed has older posts left to load (drives the end-of-list affordance).
    pub has_older: bool,
    /// Posts we've already asked the worker to background-freshen this session, so re-opening a
    /// post (cache-first) doesn't re-fetch it every time.
    pub freshened: HashSet<String>,
    /// Snapshot of the status text shown in the [`Mode::StatusDetail`] popup (taken on click, so
    /// it doesn't change underneath the reader while open).
    pub status_detail: String,
    tx: Sender<ToWorker>,
    /// Host hook to open a URL in the user's browser (desktop: the `open` crate; a web shell:
    /// `window.open`). Defaults to a no-op until the shell installs one via [`App::set_open_url`],
    /// so the platform-agnostic state machine carries no process/`open` dependency.
    open_url: Box<dyn Fn(&str)>,
}

impl App {
    pub fn new(tx: Sender<ToWorker>, prefs: Prefs) -> Self {
        let mut app = Self {
            mode: Mode::Browse,
            focus: Focus::Sidebar,
            feeds: Vec::new(),
            feed_sel: 0,
            docs: Vec::new(),
            doc_sel: 0,
            open_pub: None,
            list_title: String::new(),
            reading: None,
            reading_title: String::new(),
            reading_uri: None,
            reading_platform: None,
            reading_description: None,
            reading_cover: None,
            scroll: 0,
            input: String::new(),
            palette_sel: 0,
            status: "Loading… (press ? for help, a to add a feed)".into(),
            loading: true,
            should_quit: false,
            rects: Rects::default(),
            panel_border_style: PanelBorderStyle::Rounded,
            images: HashMap::new(),
            show_images: true,
            account: None,
            sync_prompt: Vec::new(),
            links: Vec::new(),
            focused_link: None,
            attribution_link: None,
            attribution_visible: false,
            scroll_to_focused: false,
            link_rects: Vec::new(),
            theme: Theme::modern_dark(),
            layout: prefs.layout,
            reader_cache: None,
            reading_version: 0,
            images_version: 0,
            theme_editor: None,
            menu_sel: 0,
            menu_target: None,
            onboarding: false,
            status_detail: String::new(),
            publication_choices: Vec::new(),
            read_uris: HashSet::new(),
            unread_counts: HashMap::new(),
            has_older: false,
            freshened: HashSet::new(),
            prefs,
            tx,
            open_url: Box::new(|_| {}),
        };
        app.recompute_appearance();
        app.send(ToWorker::LoadHome);
        if !app.prefs.onboarded {
            app.start_onboarding();
        }
        app
    }

    /// Install the host's URL-opener (browser launch). The shell sets this once at startup; until
    /// then it's a no-op, so headless tests never shell out.
    pub fn set_open_url(&mut self, open_url: Box<dyn Fn(&str)>) {
        self.open_url = open_url;
    }

    /// Select the panel-corner shape for this shell's renderer.
    pub fn set_panel_border_style(&mut self, style: PanelBorderStyle) {
        self.panel_border_style = style;
    }

    /// Resolve the effective theme + layout into [`Self::theme`]/[`Self::layout`]. Cheap; called
    /// at the top of every draw so the rendered appearance always matches the current state.
    /// Resolution: a per-blog override (for the active publication) wins over the global setting.
    /// While the theme editor is open, the live draft palette wins (so edits preview instantly).
    pub fn recompute_appearance(&mut self) {
        self.layout = self.effective_layout();
        let colors = match &self.theme_editor {
            Some(ed) => ed.draft.clone(), // live preview while editing
            None => self.effective_colors(),
        };
        self.theme = Theme::from(&colors);
    }

    /// The effective palette (per-blog override else global), ignoring any open editor.
    fn effective_colors(&self) -> ThemeColors {
        let name = self.effective_theme_name();
        if name == "custom" {
            self.prefs.custom.clone()
        } else {
            ThemeColors::preset(&name).unwrap_or_else(ThemeColors::modern_dark)
        }
    }

    /// The publication whose appearance is active: the feed you've opened (`open_pub`), else
    /// `None` (→ global) on the home screen and for search results. Set by `open_feed`/`refresh`,
    /// cleared on search and on returning home — so merely moving the sidebar highlight doesn't
    /// reshuffle anything.
    fn active_pub(&self) -> Option<&str> {
        self.open_pub.as_deref()
    }

    fn effective_theme_name(&self) -> String {
        self.active_pub()
            .and_then(|uri| self.prefs.blog(uri))
            .and_then(|ov| ov.theme.clone())
            .unwrap_or_else(|| self.prefs.theme.clone())
    }

    fn effective_layout(&self) -> LayoutKind {
        self.active_pub()
            .and_then(|uri| self.prefs.blog(uri))
            .and_then(|ov| ov.layout)
            .unwrap_or(self.prefs.layout)
    }

    /// Persist the current preferences through the worker's host-provided sink.
    fn persist_prefs(&self) {
        self.send(ToWorker::SavePrefs(self.prefs.clone()));
    }

    fn send(&self, msg: ToWorker) {
        let _ = self.tx.send(msg);
    }

    // --- folding worker results -------------------------------------------------

    pub fn apply(&mut self, evt: FromWorker) {
        match evt {
            FromWorker::Feeds { feeds, unread } => {
                self.feeds = feeds;
                self.unread_counts = unread.into_iter().collect();
                self.feed_sel = self.feed_sel.min(self.feeds.len().saturating_sub(1));
                self.loading = false;
                self.status = if self.feeds.is_empty() {
                    "No feeds yet — press a to add a blog by handle".into()
                } else {
                    format!("{} feed(s)", self.feeds.len())
                };
            }
            FromWorker::Docs {
                publication,
                docs,
                read_uris,
                has_older,
            } => {
                // Ignore a late update for a feed we've since navigated away from.
                if self.open_pub.as_deref() == Some(publication.as_str()) {
                    self.docs = docs;
                    self.read_uris = read_uris.into_iter().collect();
                    self.has_older = has_older;
                    self.doc_sel = self.doc_sel.min(self.docs.len().saturating_sub(1));
                    self.loading = false;
                }
            }
            FromWorker::Doc {
                uri,
                mut body,
                publishing_platform,
                from_cache,
            } => {
                if self.reading_uri.as_deref() == Some(uri.as_str()) || self.reading_uri.is_none() {
                    // Metadata-only post: the core fell back to the description blurb (the full
                    // article is on the web). Append a hint that `o` opens it.
                    if self.is_description_stub(&body) {
                        body.blocks.push(Block::Paragraph(vec![Inline::Text(
                            "— No full text in this post; press o to open it in your browser."
                                .into(),
                        )]));
                    }
                    let body_changed = self.reading.as_ref() != Some(&body);
                    let platform_changed = self.reading_platform != publishing_platform;
                    self.reading_platform = publishing_platform;
                    if body_changed {
                        self.request_body_images(&body);
                        self.rebuild_links(&body, false);
                        self.reading = Some(body);
                        self.scroll = 0;
                        self.reading_version = self.reading_version.wrapping_add(1);
                    } else if platform_changed {
                        // Keep body focus + scroll when a freshen learns provenance for an older
                        // cache entry; only the persistent border chrome changes.
                        self.rebuild_links(&body, true);
                    }
                    self.note_read(&uri); // opening a post auto-marks it read (mirror the worker)
                    // A cached open is instant but possibly stale (author edit / decoder upgrade):
                    // schedule a background freshen, once per post per session, after the image
                    // requests above so they take the worker first.
                    if from_cache && self.freshened.insert(uri.clone()) {
                        self.send(ToWorker::FreshenDoc(uri.clone()));
                    }
                }
                self.loading = false;
            }
            FromWorker::Image { key, image } => {
                self.images_version = self.images_version.wrapping_add(1); // a new image can reflow
                let (width, height) = image.dimensions();
                // Encoding is the sink's job, built lazily once the display size is known.
                self.images.insert(
                    key,
                    StoredImage {
                        image,
                        width,
                        height,
                    },
                );
            }
            FromWorker::Results(results) => {
                self.docs = results;
                self.doc_sel = 0;
                self.list_title = format!("Search: {}", self.input);
                self.mode = Mode::DocList;
                self.loading = false;
                self.status = format!("{} result(s)", self.docs.len());
            }
            FromWorker::ShowImages(on) => {
                self.show_images = on;
                if on {
                    self.request_current_images();
                }
            }
            FromWorker::Account(account) => {
                self.status = match &account {
                    Some(a) => format!("logged in as @{}", a.handle),
                    None => "logged out".into(),
                };
                self.account = account;
            }
            FromWorker::SyncDiff { local_only } => {
                if local_only.is_empty() {
                    self.sync_prompt.clear();
                } else {
                    self.sync_prompt = local_only;
                    self.mode = Mode::SyncPrompt;
                }
            }
            FromWorker::ChoosePublications { candidates } => {
                // Default everything selected — the user deselects what they don't want.
                self.publication_choices =
                    candidates.into_iter().map(|(u, n)| (u, n, true)).collect();
                self.menu_sel = 0;
                self.mode = Mode::PublicationPicker;
                self.loading = false;
            }
            FromWorker::Status(s) => self.status = s,
            FromWorker::Error(e) => {
                self.loading = false;
                self.status = format!("⚠ {e}");
            }
        }
    }

    // --- input ------------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        match self.mode {
            Mode::Search | Mode::AddFeed | Mode::SignIn => self.input_key(key),
            Mode::Palette => self.palette_key(key),
            Mode::Help | Mode::StatusDetail => self.mode = Mode::Browse,
            Mode::SyncPrompt => self.sync_prompt_key(key),
            Mode::ThemePicker => self.theme_picker_key(key),
            Mode::ThemeEditor => self.theme_editor_key(key),
            Mode::LayoutPicker => self.layout_picker_key(key),
            Mode::BlogMenu => self.blog_menu_key(key),
            Mode::PublicationPicker => self.publication_picker_key(key),
            Mode::DocList => self.doclist_key(key),
            Mode::Browse => self.browse_key(key),
        }
    }

    fn browse_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => self.quit(),
            KeyCode::Char('a') => self.enter_input(Mode::AddFeed),
            KeyCode::Char('/') => self.enter_input(Mode::Search),
            KeyCode::Char(':') => self.enter_palette(),
            KeyCode::Char('p') if ctrl => self.enter_palette(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('r') => self.refresh_current_feed(),
            KeyCode::Char('d') => self.unfollow_current_feed(),
            KeyCode::Char('m') => self.mark_read(),
            KeyCode::Char('o') => self.open_in_browser(),
            KeyCode::Char('i') => self.toggle_images(),
            KeyCode::Char('t') => self.open_theme_picker(),
            KeyCode::Char('b') => self.open_blog_menu(),
            KeyCode::Char('\\') => self.cycle_layout(),
            KeyCode::Char('<') => self.adjust_pane(-2),
            KeyCode::Char('>') => self.adjust_pane(2),
            KeyCode::Char('L') => self.toggle_account(),
            KeyCode::Tab => self.cycle_focus(),
            KeyCode::Esc => self.escape_focus(),
            KeyCode::Char('n') => self.focus_link(1),
            KeyCode::Char('N') => self.focus_link(-1),
            KeyCode::Enter if self.focus == Focus::Sidebar => self.open_feed(),
            KeyCode::Enter if self.focus == Focus::Posts => self.open_doc(),
            KeyCode::Enter => self.open_focused_link(),
            KeyCode::Down | KeyCode::Char('j') => self.move_down(),
            KeyCode::Up | KeyCode::Char('k') => self.move_up(),
            KeyCode::Char('g') => self.go_top(),
            KeyCode::Char('G') => self.go_bottom(),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            _ => {}
        }
    }

    fn doclist_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                self.focus = Focus::Sidebar;
                self.open_pub = None; // back home → global appearance
            }
            KeyCode::Char('q') => self.quit(),
            KeyCode::Enter => self.open_doc(),
            KeyCode::Down | KeyCode::Char('j') => self.posts_down(),
            KeyCode::Up | KeyCode::Char('k') => self.doc_sel = self.doc_sel.saturating_sub(1),
            KeyCode::Char('g') => self.doc_sel = 0,
            KeyCode::Char('G') => self.doc_sel = self.docs.len().saturating_sub(1),
            KeyCode::Char('/') => self.enter_input(Mode::Search),
            KeyCode::Char('o') => self.open_in_browser(),
            KeyCode::Char('i') => self.toggle_images(),
            KeyCode::Char('b') => self.open_blog_menu(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
    }

    fn input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.cancel_input(),
            KeyCode::Enter => self.submit_input(),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    fn palette_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.cancel_input(),
            KeyCode::Backspace => {
                self.input.pop();
                self.palette_sel = 0;
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.palette_sel = 0;
            }
            KeyCode::Down => {
                let n = self.palette_matches().len();
                self.palette_sel = (self.palette_sel + 1).min(n.saturating_sub(1));
            }
            KeyCode::Up => self.palette_sel = self.palette_sel.saturating_sub(1),
            KeyCode::Enter => {
                if let Some(action) = self.palette_matches().get(self.palette_sel).copied() {
                    self.mode = Mode::Browse;
                    self.input.clear();
                    self.run(action);
                }
            }
            _ => {}
        }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) {
        match ev.kind {
            MouseEventKind::ScrollDown => {
                if !matches!(self.mode, Mode::Browse | Mode::DocList) {
                    return;
                }
                if hit(self.rects.reader, ev.column, ev.row).is_some() {
                    self.scroll = self.scroll.saturating_add(3);
                } else if hit(self.rects.posts, ev.column, ev.row).is_some() {
                    self.focus = Focus::Posts;
                    self.move_down();
                } else if hit(self.rects.sidebar, ev.column, ev.row).is_some() {
                    self.focus = Focus::Sidebar;
                    self.move_down();
                }
            }
            MouseEventKind::ScrollUp => {
                if !matches!(self.mode, Mode::Browse | Mode::DocList) {
                    return;
                }
                if hit(self.rects.reader, ev.column, ev.row).is_some() {
                    self.scroll = self.scroll.saturating_sub(3);
                } else if hit(self.rects.posts, ev.column, ev.row).is_some() {
                    self.focus = Focus::Posts;
                    self.move_up();
                } else if hit(self.rects.sidebar, ev.column, ev.row).is_some() {
                    self.focus = Focus::Sidebar;
                    self.move_up();
                }
            }
            MouseEventKind::Down(_) => {
                // A click anywhere dismisses the informational popups, matching their footer
                // hint. Other modal dialogs currently remain keyboard-driven, and must consume
                // clicks so they never activate a feed, post, or link behind the dialog.
                if matches!(self.mode, Mode::Help | Mode::StatusDetail) {
                    self.mode = Mode::Browse;
                    return;
                }
                if !matches!(self.mode, Mode::Browse | Mode::DocList) {
                    return;
                }
                // Clicking the status text (not the hints) expands the full status into a popup.
                if in_rect(self.rects.status, ev.column, ev.row) {
                    if !self.status.is_empty() {
                        self.status_detail = self.status.clone();
                        self.mode = Mode::StatusDetail;
                    }
                    return;
                }
                // Test every pane by rect; absent panes are zero-area and never match, so this is
                // correct for all layouts (including ones showing feeds + posts at once).
                if let Some(i) = hit(self.rects.posts, ev.column, ev.row) {
                    if i < self.docs.len() {
                        self.doc_sel = i;
                        self.focus = Focus::Posts;
                        self.open_doc();
                    }
                } else if let Some(i) = hit(self.rects.sidebar, ev.column, ev.row) {
                    if i < self.feeds.len() {
                        self.feed_sel = i;
                        self.focus = Focus::Sidebar;
                        self.open_feed();
                    }
                } else if let Some(href) = self.link_at(ev.column, ev.row) {
                    self.open_link(&href);
                }
            }
        }
    }

    // --- actions ----------------------------------------------------------------

    pub fn palette_matches(&self) -> Vec<Action> {
        let signed_in = self.account.is_some();
        Action::ALL
            .iter()
            .copied()
            // Only the relevant sign-in/out action for the current state.
            .filter(|a| !matches!(a, Action::SignIn if signed_in))
            .filter(|a| !matches!(a, Action::SignOut if !signed_in))
            .filter(|a| fuzzy(&self.input, a.label()))
            .collect()
    }

    fn run(&mut self, action: Action) {
        match action {
            Action::AddFeed => self.enter_input(Mode::AddFeed),
            Action::Search => self.enter_input(Mode::Search),
            Action::Refresh => self.refresh_current_feed(),
            Action::MarkRead => self.mark_read(),
            Action::Theme => self.open_theme_picker(),
            Action::Layout => self.open_layout_picker(),
            Action::SignIn => self.enter_input(Mode::SignIn),
            Action::SignOut => {
                self.status = "logging out…".into();
                self.send(ToWorker::Logout);
            }
            Action::Help => self.mode = Mode::Help,
            Action::Quit => self.quit(),
        }
    }

    fn enter_input(&mut self, mode: Mode) {
        self.mode = mode;
        self.input.clear();
    }

    fn enter_palette(&mut self) {
        self.mode = Mode::Palette;
        self.input.clear();
        self.palette_sel = 0;
    }

    fn cancel_input(&mut self) {
        self.mode = Mode::Browse;
        self.input.clear();
    }

    fn submit_input(&mut self) {
        let value = self.input.trim().to_string();
        match self.mode {
            Mode::AddFeed if !value.is_empty() => {
                self.status = format!("adding {value}…");
                self.loading = true;
                self.send(ToWorker::AddFeed(value));
            }
            Mode::Search if !value.is_empty() => {
                self.open_pub = None; // results aren't a feed; ignore stray feed updates
                self.loading = true;
                self.send(ToWorker::Search(value));
            }
            Mode::SignIn if !value.is_empty() => {
                self.status = "logging in…".into();
                self.send(ToWorker::Login(value));
            }
            _ => {}
        }
        self.mode = Mode::Browse;
        self.input.clear();
    }

    /// `s` Subscribe local-only follows upstream, `r` remove them locally, Esc dismiss.
    fn sync_prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('s') => {
                let uris: Vec<String> = self
                    .sync_prompt
                    .iter()
                    .map(|(uri, _)| uri.clone())
                    .collect();
                self.send(ToWorker::SubscribeLocal(uris));
                self.dismiss_sync_prompt();
            }
            KeyCode::Char('r') => {
                for (uri, _) in &self.sync_prompt {
                    self.send(ToWorker::Unfollow(uri.clone()));
                }
                self.status = format!("removed {} local-only feed(s)", self.sync_prompt.len());
                self.dismiss_sync_prompt();
            }
            KeyCode::Esc => self.dismiss_sync_prompt(),
            _ => {}
        }
    }

    fn dismiss_sync_prompt(&mut self) {
        self.sync_prompt.clear();
        self.mode = Mode::Browse;
    }

    /// `L`: sign out if signed in, else prompt for a handle to sign in.
    fn toggle_account(&mut self) {
        if self.account.is_some() {
            self.status = "logging out…".into();
            self.send(ToWorker::Logout);
        } else {
            self.enter_input(Mode::SignIn);
        }
    }

    // --- theme customization -----------------------------------------------------

    /// Entries in the theme picker: each built-in preset, then "Custom" (opens the editor).
    fn theme_entry_count(&self) -> usize {
        PRESETS.len() + 1
    }

    /// Open the theme picker for the *global* default.
    fn open_theme_picker(&mut self) {
        self.menu_target = None;
        self.show_theme_picker();
    }

    /// Show the theme picker, pre-selecting the current theme for whatever target is set
    /// (`menu_target`: a publication for a per-blog override, or `None` for the global default).
    fn show_theme_picker(&mut self) {
        let current = self.target_theme_name();
        self.menu_sel = match current.as_str() {
            "custom" => PRESETS.len(),
            name => PRESETS.iter().position(|&p| p == name).unwrap_or(0),
        };
        self.mode = Mode::ThemePicker;
    }

    /// The theme name in effect for the current picker target.
    fn target_theme_name(&self) -> String {
        match &self.menu_target {
            Some(uri) => self
                .prefs
                .blog(uri)
                .and_then(|o| o.theme.clone())
                .unwrap_or_else(|| self.prefs.theme.clone()),
            None => self.prefs.theme.clone(),
        }
    }

    fn theme_picker_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc if self.onboarding => self.finish_onboarding(),
            KeyCode::Esc => self.mode = Mode::Browse,
            KeyCode::Up | KeyCode::Char('k') => self.menu_sel = self.menu_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.menu_sel = (self.menu_sel + 1).min(self.theme_entry_count() - 1)
            }
            KeyCode::Enter => self.choose_theme_entry(self.menu_sel),
            _ => {}
        }
    }

    /// Apply the selected theme picker entry. For the global target a preset commits immediately
    /// and the trailing "Custom" entry opens the RGB editor; for a per-blog target the entry
    /// (preset or "custom") is stored as that publication's override — the editor stays global.
    fn choose_theme_entry(&mut self, i: usize) {
        let preset = PRESETS.get(i).map(|s| s.to_string());
        match self.menu_target.take() {
            Some(uri) => {
                let name = preset.unwrap_or_else(|| "custom".to_string());
                let n = name.clone();
                self.prefs.edit_blog(&uri, |o| o.theme = Some(n));
                self.persist_prefs();
                self.mode = Mode::Browse;
                self.status = format!("theme for this blog: {name}");
            }
            None => match preset {
                Some(name) => {
                    self.prefs.theme = name.clone();
                    self.persist_prefs();
                    self.status = format!("theme: {name}");
                    if self.onboarding {
                        self.finish_onboarding();
                    } else {
                        self.mode = Mode::Browse;
                    }
                }
                None => self.open_theme_editor(), // "Custom" → editor (onboarding finishes on save)
            },
        }
    }

    /// Open the colour editor, seeded from whatever palette is currently in effect.
    fn open_theme_editor(&mut self) {
        self.theme_editor = Some(ThemeEditor {
            draft: self.effective_colors(),
            slot: 0,
            channel: 0,
        });
        self.mode = Mode::ThemeEditor;
    }

    fn theme_editor_key(&mut self, key: KeyEvent) {
        if self.theme_editor.is_none() {
            self.mode = Mode::Browse;
            return;
        }
        match key.code {
            // Esc discards the draft (recompute reverts to the saved palette next frame).
            KeyCode::Esc => {
                self.theme_editor = None;
                self.status = "theme edit cancelled".into();
                if self.onboarding {
                    self.finish_onboarding();
                } else {
                    self.mode = Mode::Browse;
                }
            }
            KeyCode::Enter => self.commit_theme_editor(),
            KeyCode::Up | KeyCode::Char('k') => {
                let ed = self.theme_editor.as_mut().unwrap();
                ed.slot = ed.slot.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let ed = self.theme_editor.as_mut().unwrap();
                ed.slot = (ed.slot + 1).min(6);
            }
            KeyCode::Left | KeyCode::Char('h') => {
                let ed = self.theme_editor.as_mut().unwrap();
                ed.channel = ed.channel.saturating_sub(1);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                let ed = self.theme_editor.as_mut().unwrap();
                ed.channel = (ed.channel + 1).min(2);
            }
            KeyCode::Char('-') => self.adjust_channel(-1),
            KeyCode::Char('+') | KeyCode::Char('=') => self.adjust_channel(1),
            KeyCode::Char('[') => self.adjust_channel(-16),
            KeyCode::Char(']') => self.adjust_channel(16),
            _ => {}
        }
    }

    /// Nudge the selected slot's selected channel by `delta`, clamped to 0..=255.
    fn adjust_channel(&mut self, delta: i32) {
        if let Some(ed) = self.theme_editor.as_mut() {
            let mut rgb = ed.draft.slot_rgb(ed.slot);
            rgb[ed.channel] = (rgb[ed.channel] as i32 + delta).clamp(0, 255) as u8;
            ed.draft.set_slot(ed.slot, rgb);
        }
    }

    /// Commit the editor draft as the custom theme and select it.
    fn commit_theme_editor(&mut self) {
        if let Some(ed) = self.theme_editor.take() {
            self.prefs.custom = ed.draft;
            self.prefs.theme = "custom".into();
            self.persist_prefs();
            self.status = "custom theme saved".into();
        }
        if self.onboarding {
            self.finish_onboarding();
        } else {
            self.mode = Mode::Browse;
        }
    }

    // --- layout customization -----------------------------------------------------

    /// Cycle the global layout (one → two → three → drill-down → …) and persist it. A per-blog
    /// override, if any, still wins for the active feed (change it via the per-blog menu).
    fn cycle_layout(&mut self) {
        self.prefs.layout = self.prefs.layout.next();
        self.persist_prefs();
        self.status = format!("layout: {}", self.prefs.layout.label());
    }

    /// Open the layout picker for the *global* default.
    fn open_layout_picker(&mut self) {
        self.menu_target = None;
        self.show_layout_picker();
    }

    fn show_layout_picker(&mut self) {
        let current = match &self.menu_target {
            Some(uri) => self
                .prefs
                .blog(uri)
                .and_then(|o| o.layout)
                .unwrap_or(self.prefs.layout),
            None => self.prefs.layout,
        };
        self.menu_sel = LayoutKind::ALL
            .iter()
            .position(|&l| l == current)
            .unwrap_or(1);
        self.mode = Mode::LayoutPicker;
    }

    fn layout_picker_key(&mut self, key: KeyEvent) {
        match key.code {
            // During onboarding, skipping the layout step keeps the default and moves to the theme.
            KeyCode::Esc if self.onboarding => self.show_theme_picker(),
            KeyCode::Esc => self.mode = Mode::Browse,
            KeyCode::Up | KeyCode::Char('k') => self.menu_sel = self.menu_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.menu_sel = (self.menu_sel + 1).min(LayoutKind::ALL.len() - 1)
            }
            KeyCode::Enter => {
                let layout = LayoutKind::ALL[self.menu_sel.min(LayoutKind::ALL.len() - 1)];
                match self.menu_target.take() {
                    Some(uri) => {
                        self.prefs.edit_blog(&uri, |o| o.layout = Some(layout));
                        self.status = format!("layout for this blog: {}", layout.label());
                    }
                    None => {
                        self.prefs.layout = layout;
                        self.status = format!("layout: {}", layout.label());
                    }
                }
                self.persist_prefs();
                if self.onboarding {
                    self.show_theme_picker(); // step 2: pick a theme
                } else {
                    self.mode = Mode::Browse;
                }
            }
            _ => {}
        }
    }

    // --- first-launch onboarding --------------------------------------------------

    /// Begin the first-launch flow: pick a layout, then a theme. Each picker advances to the
    /// next step (see the `onboarding` branches in the picker handlers).
    fn start_onboarding(&mut self) {
        self.onboarding = true;
        self.menu_target = None;
        self.status = "welcome — choose a layout (esc to skip)".into();
        self.show_layout_picker();
    }

    /// Finish onboarding: mark it done so it never shows again, and land in the reader.
    fn finish_onboarding(&mut self) {
        self.onboarding = false;
        self.prefs.onboarded = true;
        self.persist_prefs();
        self.mode = Mode::Browse;
        self.status = "all set — press a to add a blog, ? for help".into();
    }

    // --- per-blog overrides -------------------------------------------------------

    /// The publication a per-blog edit targets: the open feed if any, else the selected feed.
    fn target_pub(&self) -> Option<(String, String)> {
        let uri = self
            .open_pub
            .clone()
            .or_else(|| self.feeds.get(self.feed_sel).map(|p| p.uri.clone()))?;
        let name = self
            .feeds
            .iter()
            .find(|p| p.uri == uri)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| uri.clone());
        Some((uri, name))
    }

    /// Open the per-blog customization menu for the active/selected feed.
    fn open_blog_menu(&mut self) {
        match self.target_pub() {
            Some((uri, _)) => {
                self.menu_target = Some(uri);
                self.menu_sel = 0;
                self.mode = Mode::BlogMenu;
            }
            None => self.status = "select a feed first".into(),
        }
    }

    fn blog_menu_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.menu_target = None;
                self.mode = Mode::Browse;
            }
            KeyCode::Up | KeyCode::Char('k') => self.menu_sel = self.menu_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.menu_sel = (self.menu_sel + 1).min(2),
            KeyCode::Enter => match self.menu_sel {
                0 => self.show_theme_picker(),  // keeps menu_target → per-blog
                1 => self.show_layout_picker(), // keeps menu_target → per-blog
                _ => {
                    if let Some(uri) = self.menu_target.take() {
                        self.prefs.per_blog.remove(&uri);
                        self.persist_prefs();
                        self.status = "this blog now uses the global appearance".into();
                    }
                    self.mode = Mode::Browse;
                }
            },
            _ => {}
        }
    }

    /// Which width the focused pane controls in the current layout (or `None` when the focused
    /// pane has no fixed width to resize: the reader is always the flexible remainder, and a
    /// one-pane / full-width list has no neighbour to trade space with).
    fn focused_pane_width(&self) -> Option<(PaneWidth, &'static str)> {
        match (self.layout, self.focus) {
            (LayoutKind::OnePane, _) | (_, Focus::Reader) => None,
            // Two-pane has a single left column (feeds or posts) → one width.
            (LayoutKind::TwoPane, _) => Some((PaneWidth::Sidebar, "sidebar")),
            (LayoutKind::ThreePane, Focus::Sidebar) => Some((PaneWidth::Sidebar, "feeds")),
            (LayoutKind::ThreePane, Focus::Posts) => Some((PaneWidth::Posts, "posts")),
            // Drill-down's only divider is the feeds column in the feeds+posts stage.
            (LayoutKind::DrillDown, Focus::Posts) => Some((PaneWidth::Sidebar, "feeds")),
            _ => None,
        }
    }

    /// Widen/narrow the focused pane by `delta` (independently per pane), clamped, and persist.
    fn adjust_pane(&mut self, delta: i16) {
        let Some((which, label)) = self.focused_pane_width() else {
            self.status = "this pane fills the available space".into();
            return;
        };
        let cur = match which {
            PaneWidth::Sidebar => self.prefs.sidebar_width,
            PaneWidth::Posts => self.prefs.posts_width,
        };
        let new = (cur as i16 + delta).clamp(PANE_MIN as i16, PANE_MAX as i16) as u16;
        if new != cur {
            match which {
                PaneWidth::Sidebar => self.prefs.sidebar_width = new,
                PaneWidth::Posts => self.prefs.posts_width = new,
            }
            self.persist_prefs();
            self.status = format!("{label} width: {new}");
        }
    }

    /// The panes that can hold focus in the current layout, in cycle order. `Posts` is skipped
    /// until a feed is opened (no posts to focus); `Reader` likewise needs an open document in
    /// the drill-down layout.
    fn focus_ring(&self) -> Vec<Focus> {
        match self.layout {
            LayoutKind::TwoPane => vec![Focus::Sidebar, Focus::Reader],
            LayoutKind::ThreePane | LayoutKind::OnePane => {
                let mut ring = vec![Focus::Sidebar];
                if !self.docs.is_empty() {
                    ring.push(Focus::Posts);
                }
                ring.push(Focus::Reader);
                ring
            }
            LayoutKind::DrillDown => {
                // A drill-down: feeds → feeds+posts → the post. Each level is reachable only once
                // the previous one has something to open (`Tab` descends, `Esc` ascends).
                let mut ring = vec![Focus::Sidebar];
                if self.open_pub.is_some() {
                    ring.push(Focus::Posts);
                }
                if self.reading.is_some() {
                    ring.push(Focus::Reader);
                }
                ring
            }
        }
    }

    /// Move focus to the next pane the layout shows.
    fn cycle_focus(&mut self) {
        let ring = self.focus_ring();
        let i = ring.iter().position(|&f| f == self.focus).unwrap_or(0);
        self.focus = ring[(i + 1) % ring.len()];
    }

    /// Step focus back toward the start of the ring (`Esc`): reader → posts → feeds. Useful in
    /// the single-pane / drill-down layouts where the reader otherwise hides the lists; in
    /// two-pane it just returns focus from the reader to the sidebar.
    fn escape_focus(&mut self) {
        let ring = self.focus_ring();
        if let Some(i) = ring.iter().position(|&f| f == self.focus)
            && i > 0
        {
            self.focus = ring[i - 1];
        }
    }

    /// The multi-publication add picker: ↑↓ move, Space toggle, `a` all, `n` none, Enter follow
    /// the selected, Esc cancel.
    fn publication_picker_key(&mut self, key: KeyEvent) {
        let n = self.publication_choices.len();
        match key.code {
            KeyCode::Esc => {
                self.publication_choices.clear();
                self.mode = Mode::Browse;
                self.status = "add cancelled".into();
            }
            KeyCode::Up | KeyCode::Char('k') => self.menu_sel = self.menu_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.menu_sel = (self.menu_sel + 1).min(n.saturating_sub(1))
            }
            KeyCode::Char(' ') => {
                if let Some(choice) = self.publication_choices.get_mut(self.menu_sel) {
                    choice.2 = !choice.2;
                }
            }
            KeyCode::Char('a') => self.publication_choices.iter_mut().for_each(|c| c.2 = true),
            KeyCode::Char('n') => self
                .publication_choices
                .iter_mut()
                .for_each(|c| c.2 = false),
            KeyCode::Enter => {
                let chosen: Vec<String> = self
                    .publication_choices
                    .iter()
                    .filter(|c| c.2)
                    .map(|c| c.0.clone())
                    .collect();
                self.publication_choices.clear();
                self.mode = Mode::Browse;
                self.status = format!("following {} publication(s)…", chosen.len());
                self.send(ToWorker::FollowPublications(chosen));
            }
            _ => {}
        }
    }

    fn open_feed(&mut self) {
        let Some(p) = self.feeds.get(self.feed_sel) else {
            return;
        };
        let (uri, name) = (p.uri.clone(), p.name.clone());
        self.list_title = name;
        self.open_pub = Some(uri.clone());
        self.docs.clear();
        self.doc_sel = 0;
        self.loading = true;
        // Resolve this feed's effective layout (it may carry a per-blog override) before deciding
        // how to present it: in two-pane the post list *replaces* the sidebar (Mode::DocList);
        // in the multi-pane layouts the post list is its own always-visible pane (stay in Browse,
        // just move focus to it).
        self.recompute_appearance();
        self.mode = if self.layout == LayoutKind::TwoPane {
            Mode::DocList
        } else {
            Mode::Browse
        };
        self.focus = Focus::Posts;
        self.send(ToWorker::OpenFeed(uri));
    }

    /// Whether `body` is the core's description-blurb fallback for a metadata-only document — a
    /// single paragraph equal to the open doc's `description`. Drives the "press o to open" hint.
    fn is_description_stub(&self, body: &RichDoc) -> bool {
        let Some(desc) = self.reading_description.as_deref() else {
            return false;
        };
        !desc.trim().is_empty()
            && body.blocks.len() == 1
            && body.blocks[0] == Block::Paragraph(vec![Inline::Text(desc.to_string())])
    }

    fn open_doc(&mut self) {
        let Some(d) = self.docs.get(self.doc_sel) else {
            return;
        };
        self.reading_title = if d.title.is_empty() {
            "(untitled)".into()
        } else {
            d.title.clone()
        };
        self.reading_uri = Some(d.uri.clone());
        self.reading_platform = d.publishing_platform;
        self.reading_description = d.description.clone();
        self.reading_cover = d.cover_image.as_ref().map(|i| i.source.clone());
        let doc_uri = d.uri.clone();
        let cover = self.reading_cover.clone(); // ends the borrow of self.docs (`d`)

        self.reading = None;
        self.links.clear();
        self.append_attribution_link();
        self.focused_link = None;
        self.attribution_visible = false;
        self.link_rects.clear();
        self.scroll = 0;
        self.loading = true;
        self.mode = Mode::Browse;
        self.focus = Focus::Reader;
        if let Some(src) = cover {
            self.request_image(src);
        }
        self.send(ToWorker::OpenDoc(doc_uri));
    }

    /// Rebuild the reader's navigable links in visual reading order: body links first, then the
    /// persistent bottom-border attribution. A metadata-only platform refresh may preserve focus
    /// because the body and its link ordering did not change.
    fn rebuild_links(&mut self, body: &RichDoc, preserve_focus: bool) {
        let previous_focus = preserve_focus.then_some(self.focused_link).flatten();
        self.links.clear();
        collect_links(&body.blocks, &mut self.links);
        self.append_attribution_link();
        self.focused_link = previous_focus.filter(|i| *i < self.links.len());
        self.attribution_visible = false;
        self.link_rects.clear();
        self.reader_cache = None;
    }

    fn append_attribution_link(&mut self) {
        self.attribution_link = self.reading_platform.map(|platform| {
            let idx = self.links.len();
            self.links.push(platform.homepage().to_string());
            idx
        });
    }

    /// Request any not-yet-loaded images in `body` from the worker — including ones nested in
    /// quotes/lists or inline within a paragraph (the reader renders those framed in place).
    fn request_body_images(&self, body: &RichDoc) {
        let mut sources = Vec::new();
        collect_image_sources(&body.blocks, &mut sources);
        for source in sources {
            self.request_image(source);
        }
    }

    fn request_image(&self, source: ImageSource) {
        if !self.show_images {
            return; // text-only mode: don't fetch
        }
        let key = image_key(&source);
        if !self.images.contains_key(&key) {
            self.send(ToWorker::LoadImage { key, source });
        }
    }

    /// (Re)request the images of the currently open document + its cover.
    fn request_current_images(&self) {
        if let Some(body) = &self.reading {
            self.request_body_images(body);
        }
        if let Some(src) = &self.reading_cover {
            self.request_image(src.clone());
        }
    }

    /// Toggle text-only mode (`i`); persist it and load this doc's images when turning on.
    fn toggle_images(&mut self) {
        self.show_images = !self.show_images;
        self.send(ToWorker::SetShowImages(self.show_images));
        self.status = if self.show_images {
            "images: on"
        } else {
            "images: off (text-only)"
        }
        .into();
        if self.show_images {
            self.request_current_images();
        }
    }

    /// Advance the post selection; at (or past) the bottom, ask the worker to load older posts.
    fn posts_down(&mut self) {
        if self.doc_sel + 1 >= self.docs.len() {
            self.load_older_current();
        } else {
            self.doc_sel += 1;
        }
    }

    /// Request the next older window for the open feed (the worker no-ops when exhausted).
    fn load_older_current(&mut self) {
        if let Some(uri) = self.open_pub.clone() {
            self.status = "loading older…".into();
            self.send(ToWorker::LoadOlder(uri));
        }
    }

    fn refresh_current_feed(&mut self) {
        if let Some(p) = self.feeds.get(self.feed_sel) {
            self.open_pub = Some(p.uri.clone());
            self.loading = true;
            self.status = format!("refreshing {}…", p.name);
            self.send(ToWorker::Refresh(p.uri.clone()));
        }
    }

    fn unfollow_current_feed(&mut self) {
        if self.focus == Focus::Sidebar
            && let Some(p) = self.feeds.get(self.feed_sel)
        {
            self.status = format!("unfollowed {}", p.name);
            self.send(ToWorker::Unfollow(p.uri.clone()));
        }
    }

    fn mark_read(&mut self) {
        if let Some(uri) = self.reading_uri.clone() {
            self.send(ToWorker::SetRead(uri.clone(), true));
            self.note_read(&uri);
            self.status = "marked read".into();
        }
    }

    /// Reflect a post becoming read in local state so the unread marker + sidebar count update
    /// immediately (the worker persists it; this keeps the UI live without a round-trip). Newly
    /// read → decrement the open feed's unread count.
    fn note_read(&mut self, uri: &str) {
        if self.read_uris.insert(uri.to_string())
            && let Some(pub_uri) = self.open_pub.clone()
            && let Some(count) = self.unread_counts.get_mut(&pub_uri)
        {
            *count = count.saturating_sub(1);
        }
    }

    /// Cycle the focused link (`delta` +1/-1, wrapping) and ask the reader to scroll it into view.
    fn focus_link(&mut self, delta: i32) {
        let n = self.navigable_link_count();
        if n == 0 {
            self.status = "no links in this post".into();
            return;
        }
        let n = n as i32;
        let current = self.focused_link.filter(|i| *i < n as usize);
        let next = match current {
            None if delta < 0 => (n - 1) as usize,
            None => 0,
            Some(i) => (i as i32 + delta).rem_euclid(n) as usize,
        };
        self.focused_link = Some(next);
        self.scroll_to_focused = self.attribution_link != Some(next);
        self.focus = Focus::Reader;
        self.status = format!("link {}/{}: {}", next + 1, n, self.links[next]);
    }

    fn navigable_link_count(&self) -> usize {
        match (self.attribution_link, self.attribution_visible) {
            (Some(idx), false) => idx,
            _ => self.links.len(),
        }
    }

    /// Open the focused link (`Enter` while reading).
    fn open_focused_link(&mut self) {
        if self
            .attribution_link
            .is_some_and(|idx| self.focused_link == Some(idx))
            && !self.attribution_visible
        {
            return;
        }
        match self.focused_link.and_then(|i| self.links.get(i)) {
            Some(href) => {
                let href = href.clone();
                self.open_link(&href);
            }
            None if !self.links.is_empty() => {
                self.status = "press n to focus a link, then Enter".into();
            }
            None => {}
        }
    }

    /// Open a hyperlink in the browser.
    fn open_link(&mut self, href: &str) {
        self.status = format!("opening {href}");
        (self.open_url)(href);
    }

    /// The link under a click in the reader pane, if any (maps screen → virtual doc coordinates).
    fn link_at(&self, col: u16, row: u16) -> Option<String> {
        if in_rect(self.rects.attribution, col, row) {
            return self
                .attribution_link
                .and_then(|idx| self.links.get(idx))
                .cloned();
        }
        let r = self.rects.reader;
        let (inner_x, inner_y) = (r.x + 1, r.y + 1); // inside the 1-cell border
        if col < inner_x
            || row < inner_y
            || col >= r.right().saturating_sub(1)
            || row >= r.bottom().saturating_sub(1)
        {
            return None;
        }
        let vrow = (row - inner_y) + self.scroll;
        let vcol = col - inner_x;
        self.link_rects
            .iter()
            .find(|lr| lr.row == vrow && vcol >= lr.col && vcol < lr.col + lr.width)
            .and_then(|lr| self.links.get(lr.idx).cloned())
    }

    /// Open the focused/open document's post in the browser (`o`).
    fn open_in_browser(&mut self) {
        let url = match self.docs.get(self.doc_sel).and_then(|d| self.doc_url(d)) {
            Some(url) => url,
            None => {
                self.status = "no web URL for this post".into();
                return;
            }
        };
        self.status = format!("opening {url}");
        (self.open_url)(&url);
    }

    /// The post's browser URL: the publication's `url` (looked up among the feeds) joined
    /// with the document's `path`.
    fn doc_url(&self, doc: &Document) -> Option<String> {
        let pub_url = self
            .feeds
            .iter()
            .find(|p| p.uri == doc.publication)?
            .url
            .clone();
        Some(match doc.path.as_deref() {
            Some(path) if !path.is_empty() => web_url(&pub_url, path),
            _ => pub_url,
        })
    }

    fn move_down(&mut self) {
        match self.focus {
            Focus::Sidebar => {
                self.feed_sel = (self.feed_sel + 1).min(self.feeds.len().saturating_sub(1))
            }
            Focus::Posts => self.posts_down(),
            Focus::Reader => self.scroll = self.scroll.saturating_add(1),
        }
    }

    fn move_up(&mut self) {
        match self.focus {
            Focus::Sidebar => self.feed_sel = self.feed_sel.saturating_sub(1),
            Focus::Posts => self.doc_sel = self.doc_sel.saturating_sub(1),
            Focus::Reader => self.scroll = self.scroll.saturating_sub(1),
        }
    }

    fn go_top(&mut self) {
        match self.focus {
            Focus::Sidebar => self.feed_sel = 0,
            Focus::Posts => self.doc_sel = 0,
            Focus::Reader => self.scroll = 0,
        }
    }

    fn go_bottom(&mut self) {
        match self.focus {
            Focus::Sidebar => self.feed_sel = self.feeds.len().saturating_sub(1),
            Focus::Posts => self.doc_sel = self.docs.len().saturating_sub(1),
            Focus::Reader => {}
        }
    }

    fn quit(&mut self) {
        self.should_quit = true;
        self.send(ToWorker::Quit);
    }
}

/// Collect every image source in `blocks`, in document order — recursing into quotes/lists and
/// scanning inline content (so images nested in a quote, list, or mid-paragraph are fetched too).
fn collect_image_sources(blocks: &[Block], out: &mut Vec<ImageSource>) {
    for block in blocks {
        match block {
            Block::Image(img) => out.push(img.source.clone()),
            Block::ImageGrid(images) => out.extend(images.iter().map(|i| i.source.clone())),
            Block::Quote(inner) => collect_image_sources(inner, out),
            Block::List { items, .. } => {
                for item in items {
                    collect_image_sources(item, out);
                }
            }
            Block::Paragraph(inlines)
            | Block::Heading {
                content: inlines, ..
            }
            | Block::Callout {
                content: inlines, ..
            } => collect_inline_images(inlines, out),
            Block::Aligned { content, .. } => {
                collect_image_sources(std::slice::from_ref(content.as_ref()), out)
            }
            _ => {}
        }
    }
}

/// Collect hyperlink hrefs in `blocks`, in document/reading order — the navigable set for
/// `n`/`N`/Enter and clicks. Mirrors how the reader renders links: tables render as plain text,
/// so their links are skipped to keep this list in lock-step with the rendered link order.
fn collect_links(blocks: &[Block], out: &mut Vec<String>) {
    for block in blocks {
        match block {
            Block::Paragraph(c)
            | Block::Heading { content: c, .. }
            | Block::Callout { content: c, .. } => collect_inline_links(c, out),
            Block::Quote(inner) => collect_links(inner, out),
            Block::List { items, .. } => {
                for item in items {
                    collect_links(item, out);
                }
            }
            Block::Aligned { content, .. } => {
                collect_links(std::slice::from_ref(content.as_ref()), out)
            }
            _ => {}
        }
    }
}

/// Collect hrefs from inline content, in order (recursing into styled spans + link content).
fn collect_inline_links(inlines: &[Inline], out: &mut Vec<String>) {
    for inline in inlines {
        match inline {
            Inline::Link { href, content } => {
                out.push(href.clone());
                collect_inline_links(content, out);
            }
            Inline::Strong(c) | Inline::Emphasis(c) | Inline::Strike(c) | Inline::Underline(c) => {
                collect_inline_links(c, out)
            }
            Inline::Highlight { content, .. } => collect_inline_links(content, out),
            _ => {}
        }
    }
}

/// Collect image sources from inline content (recursing into styled/link spans).
fn collect_inline_images(inlines: &[Inline], out: &mut Vec<ImageSource>) {
    for inline in inlines {
        match inline {
            Inline::Image(img) => out.push(img.source.clone()),
            Inline::Strong(c) | Inline::Emphasis(c) | Inline::Strike(c) | Inline::Underline(c) => {
                collect_inline_images(c, out)
            }
            Inline::Highlight { content: c, .. } | Inline::Link { content: c, .. } => {
                collect_inline_images(c, out)
            }
            _ => {}
        }
    }
}

/// Whether a screen coordinate falls inside `rect` (a plain point-in-rect test; zero-area → no).
fn in_rect(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

/// Row index clicked inside a bordered list `rect` (None if outside / on the border). A
/// zero-area rect (an absent pane in the current layout) never matches.
fn hit(rect: Rect, x: u16, y: u16) -> Option<usize> {
    if rect.width == 0 || rect.height == 0 {
        return None;
    }
    let inside = x > rect.x
        && x < rect.right().saturating_sub(1)
        && y > rect.y
        && y < rect.bottom().saturating_sub(1);
    inside.then(|| (y - rect.y - 1) as usize)
}

/// Case-insensitive subsequence match (the command-palette filter).
pub fn fuzzy(query: &str, candidate: &str) -> bool {
    let mut q = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .peekable();
    if q.peek().is_none() {
        return true;
    }
    let mut chars = candidate.chars().map(|c| c.to_ascii_lowercase());
    for want in q {
        if !chars.any(|c| c == want) {
            return false;
        }
    }
    true
}

/// Join a publication base URL with a document path → the post's browser URL.
pub fn web_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::{collect_links, fuzzy, web_url};
    use standard_core::model::{Block, Inline, PublishingPlatform, RichDoc};

    #[test]
    fn collect_links_walks_in_document_order() {
        let link = |href: &str, text: &str| Inline::Link {
            href: href.into(),
            content: vec![Inline::Text(text.into())],
        };
        let blocks = vec![
            Block::Paragraph(vec![
                Inline::Text("see ".into()),
                link("https://a", "a"),
                Inline::Text(" and ".into()),
                Inline::Strong(vec![link("https://b", "b")]),
            ]),
            Block::Quote(vec![Block::Paragraph(vec![link("https://c", "c")])]),
            // Table links are skipped (they render as plain text), keeping order in lock-step
            // with the reader.
            Block::Table {
                head: vec![vec![link("https://skip", "x")]],
                rows: vec![],
            },
        ];
        let mut out = Vec::new();
        collect_links(&blocks, &mut out);
        assert_eq!(out, vec!["https://a", "https://b", "https://c"]);
    }

    #[test]
    fn link_at_maps_a_click_to_its_href() {
        use super::{App, LinkRect};
        use crate::worker::ToWorker;
        use ratatui::layout::Rect;
        use std::sync::mpsc::channel;

        let (tx, _rx) = channel::<ToWorker>();
        let mut app = App::new(tx, crate::prefs::Prefs::default());
        app.rects.reader = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 20,
        };
        app.links = vec!["https://x".into()];
        // Link at virtual row 2, cols 5..9. Inner origin is (1,1) past the border.
        app.link_rects = vec![LinkRect {
            idx: 0,
            row: 2,
            col: 5,
            width: 4,
        }];
        // screen (col 6, row 3) → virtual (col 5, row 2): a hit.
        assert_eq!(app.link_at(6, 3).as_deref(), Some("https://x"));
        // outside the link's column span, and a different row: misses.
        assert_eq!(app.link_at(10, 3), None);
        assert_eq!(app.link_at(6, 4), None);
    }

    #[test]
    fn attribution_is_the_last_keyboard_link_and_does_not_scroll() {
        let mut app = test_app(Prefs::for_test());
        let body = RichDoc {
            blocks: vec![Block::Paragraph(vec![Inline::Link {
                href: "https://article.test/link".into(),
                content: vec![Inline::Text("body link".into())],
            }])],
        };
        app.reading_platform = Some(PublishingPlatform::Leaflet);
        app.rebuild_links(&body, false);
        app.attribution_visible = true;

        app.focus_link(1);
        assert_eq!(app.focused_link, Some(0), "body link comes first");
        assert!(app.scroll_to_focused);
        app.focus_link(1);
        assert_eq!(app.focused_link, app.attribution_link);
        assert!(!app.scroll_to_focused, "persistent chrome never scrolls");
        assert_eq!(app.links[1], PublishingPlatform::Leaflet.homepage());
    }

    #[test]
    fn hidden_attribution_is_not_keyboard_navigable() {
        let mut app = test_app(Prefs::for_test());
        app.reading_platform = Some(PublishingPlatform::GreenGale);
        app.rebuild_links(&RichDoc::default(), false);
        app.attribution_visible = false;

        app.focus_link(1);
        assert_eq!(app.focused_link, None);
        assert_eq!(app.status, "no links in this post");
    }

    #[test]
    fn attribution_border_click_and_enter_open_the_platform_homepage() {
        use crate::input::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::layout::Rect;
        use std::sync::{Arc, Mutex};

        let opened = Arc::new(Mutex::new(Vec::<String>::new()));
        let capture = Arc::clone(&opened);
        let mut app = test_app(Prefs::for_test());
        app.set_open_url(Box::new(move |url| {
            capture.lock().unwrap().push(url.to_string());
        }));
        app.reading_platform = Some(PublishingPlatform::Offprint);
        app.rebuild_links(&RichDoc::default(), false);
        app.attribution_visible = true;
        app.rects.attribution = Rect::new(20, 9, 10, 1);

        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 24,
            row: 9,
            modifiers: KeyModifiers::NONE,
        });
        app.focused_link = app.attribution_link;
        app.open_focused_link();

        assert_eq!(
            opened.lock().unwrap().as_slice(),
            [
                PublishingPlatform::Offprint.homepage(),
                PublishingPlatform::Offprint.homepage()
            ]
        );
    }

    #[test]
    fn attribution_only_worker_update_preserves_body_and_scroll() {
        use crate::worker::FromWorker;

        let mut app = test_app(Prefs::for_test());
        app.reading_uri = Some("at://d/1".into());
        let body = RichDoc {
            blocks: vec![Block::Paragraph(vec![Inline::Text("body".into())])],
        };
        app.apply(FromWorker::Doc {
            uri: "at://d/1".into(),
            body: body.clone(),
            publishing_platform: None,
            from_cache: false,
        });
        app.scroll = 12;
        let version = app.reading_version;

        app.apply(FromWorker::Doc {
            uri: "at://d/1".into(),
            body,
            publishing_platform: Some(PublishingPlatform::Wordpress),
            from_cache: false,
        });

        assert_eq!(app.scroll, 12);
        assert_eq!(app.reading_version, version);
        assert_eq!(app.attribution_link, Some(0));
        assert_eq!(app.links, [PublishingPlatform::Wordpress.homepage()]);
    }

    #[test]
    fn fuzzy_subsequence() {
        assert!(fuzzy("", "anything"));
        assert!(fuzzy("adf", "Add feed"));
        assert!(fuzzy("srch", "Search"));
        assert!(fuzzy("quit", "Quit"));
        assert!(!fuzzy("xyz", "Add feed"));
        assert!(!fuzzy("feedx", "Add feed"));
    }

    #[test]
    fn builds_post_urls() {
        // path with leading slash (the common case across platforms)
        assert_eq!(
            web_url("https://greengale.app/david.yapfest.club", "/3mmozgypkle2s"),
            "https://greengale.app/david.yapfest.club/3mmozgypkle2s"
        );
        // offprint keeps its /a/ prefix in the path
        assert_eq!(
            web_url(
                "https://chaospocket.offprint.app",
                "/a/3mi34zu4buc23-oh-hey"
            ),
            "https://chaospocket.offprint.app/a/3mi34zu4buc23-oh-hey"
        );
        // trailing slash on base + no leading slash on path both handled
        assert_eq!(web_url("https://x.test/", "post"), "https://x.test/post");
    }

    // --- customization (layout / theme / per-blog) --------------------------------

    use super::{App, Focus, Mode, hit};
    use crate::prefs::{LayoutKind, Prefs};
    use standard_core::model::{Document, Publication};
    use std::sync::mpsc::channel;

    fn test_app(prefs: Prefs) -> App {
        let (tx, _rx) = channel::<crate::worker::ToWorker>();
        App::new(tx, prefs)
    }

    fn feed(uri: &str) -> Publication {
        Publication {
            uri: uri.into(),
            url: "https://x.test".into(),
            name: "feed".into(),
            description: None,
            icon: None,
        }
    }

    fn doc(uri: &str, publication: &str) -> Document {
        Document {
            uri: uri.into(),
            title: "t".into(),
            description: None,
            publication: publication.into(),
            published_at: "2026-01-01".into(),
            updated_at: None,
            publishing_platform: None,
            cover_image: None,
            text_content: None,
            tags: vec![],
            path: None,
        }
    }

    #[test]
    fn per_blog_override_beats_global() {
        let mut prefs = Prefs::for_test();
        prefs.layout = LayoutKind::TwoPane;
        prefs.theme = "modern-dark".into();
        prefs.edit_blog("at://p/1", |o| {
            o.layout = Some(LayoutKind::ThreePane);
            o.theme = Some("light".into());
        });
        let mut app = test_app(prefs);

        // Home (no feed open) → global.
        app.open_pub = None;
        app.recompute_appearance();
        assert_eq!(app.layout, LayoutKind::TwoPane);
        let dark = crate::ui::theme::Theme::modern_dark();
        assert_eq!(app.theme.bg, dark.bg, "global theme on home");

        // The overridden feed open → its layout + theme win.
        app.open_pub = Some("at://p/1".into());
        app.recompute_appearance();
        assert_eq!(app.layout, LayoutKind::ThreePane);
        let light = crate::ui::theme::Theme::from(&crate::ui::theme::ThemeColors::light());
        assert_eq!(app.theme.bg, light.bg, "per-blog theme override applied");
    }

    #[test]
    fn focus_cycle_skips_posts_until_a_feed_is_open() {
        let mut prefs = Prefs::for_test();
        prefs.layout = LayoutKind::ThreePane;
        let mut app = test_app(prefs);
        app.recompute_appearance();

        // No docs yet: Posts is not focusable.
        app.docs.clear();
        app.focus = Focus::Sidebar;
        app.cycle_focus();
        assert_eq!(app.focus, Focus::Reader, "Posts skipped when empty");
        app.cycle_focus();
        assert_eq!(app.focus, Focus::Sidebar);

        // With a doc, Posts joins the ring.
        app.docs = vec![doc("at://d/1", "at://p/1")];
        app.focus = Focus::Sidebar;
        app.cycle_focus();
        assert_eq!(app.focus, Focus::Posts);
    }

    #[test]
    fn escape_focus_steps_back_through_the_ring() {
        let mut prefs = Prefs::for_test();
        prefs.layout = LayoutKind::ThreePane;
        let mut app = test_app(prefs);
        app.recompute_appearance();
        app.docs = vec![doc("at://d/1", "at://p/1")];
        app.focus = Focus::Reader;
        app.escape_focus(); // Reader → Posts
        assert_eq!(app.focus, Focus::Posts);
        app.escape_focus(); // Posts → Sidebar
        assert_eq!(app.focus, Focus::Sidebar);
        app.escape_focus(); // already first → stays
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn theme_editor_adjusts_and_commits_custom() {
        let mut app = test_app(Prefs::for_test());
        app.open_theme_editor();
        let ed = app.theme_editor.as_mut().expect("editor open");
        ed.slot = 4; // accent
        ed.channel = 0; // R
        let before = ed.draft.slot_rgb(4)[0];
        app.adjust_channel(10);
        let after = app.theme_editor.as_ref().unwrap().draft.slot_rgb(4)[0];
        assert_eq!(after, (before as i32 + 10).clamp(0, 255) as u8);
        app.commit_theme_editor();
        assert_eq!(app.prefs.theme, "custom");
        assert!(app.theme_editor.is_none());
    }

    #[test]
    fn hit_rejects_zero_area_panes() {
        use ratatui::layout::Rect;
        assert_eq!(
            hit(Rect::default(), 5, 5),
            None,
            "absent pane never matches"
        );
        let r = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        assert_eq!(hit(r, 3, 3), Some(2), "row inside a present pane");
    }

    #[test]
    fn click_selects_in_the_pane_under_the_cursor() {
        use crate::input::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::layout::Rect;
        let mut prefs = Prefs::for_test();
        prefs.layout = LayoutKind::ThreePane;
        let mut app = test_app(prefs);
        app.feeds = vec![feed("at://p/1")];
        app.docs = vec![doc("at://d/1", "at://p/1"), doc("at://d/2", "at://p/1")];
        // Sidebar on the left, posts beside it (both visible in three-pane).
        app.rects.sidebar = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 10,
        };
        app.rects.posts = Rect {
            x: 20,
            y: 0,
            width: 20,
            height: 10,
        };
        app.rects.reader = Rect {
            x: 40,
            y: 0,
            width: 40,
            height: 10,
        };
        let click = |x, y| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: crate::input::KeyModifiers::NONE,
        };
        // A click in the posts pane (not the sidebar) selects that row's document and opens it,
        // even though the sidebar is visible too — proving the click hit the right pane.
        app.on_mouse(click(22, 2)); // posts pane, inner row 1 → docs[1]
        assert_eq!(app.doc_sel, 1);
        assert_eq!(
            app.reading_uri.as_deref(),
            Some("at://d/2"),
            "opened the clicked post"
        );

        // A modal consumes the same click instead of activating the post behind it.
        app.doc_sel = 0;
        app.mode = Mode::ThemePicker;
        app.on_mouse(click(22, 2));
        assert_eq!(app.doc_sel, 0);
        assert_eq!(app.mode, Mode::ThemePicker);

        let wheel = |kind, x, y| MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: crate::input::KeyModifiers::NONE,
        };
        app.mode = Mode::Browse;
        app.focus = Focus::Sidebar;
        app.on_mouse(wheel(MouseEventKind::ScrollDown, 22, 2));
        assert_eq!(app.focus, Focus::Posts, "wheel targets the hovered pane");
        assert_eq!(app.doc_sel, 1);

        app.scroll = 6;
        app.on_mouse(wheel(MouseEventKind::ScrollUp, 42, 2));
        assert_eq!(app.scroll, 3, "reader wheel moves three document rows");

        app.mode = Mode::ThemePicker;
        app.on_mouse(wheel(MouseEventKind::ScrollDown, 42, 2));
        assert_eq!(app.scroll, 3, "modal consumes wheel events");
    }

    #[test]
    fn pane_resize_targets_only_the_focused_pane() {
        let mut prefs = Prefs::for_test();
        prefs.layout = LayoutKind::ThreePane;
        prefs.sidebar_width = 30;
        prefs.posts_width = 36;
        let mut app = test_app(prefs);
        app.recompute_appearance();
        app.docs = vec![doc("at://d/1", "at://p/1")];

        // Feeds focused → only the sidebar width moves.
        app.focus = Focus::Sidebar;
        app.adjust_pane(2);
        assert_eq!(app.prefs.sidebar_width, 32);
        assert_eq!(app.prefs.posts_width, 36, "posts width untouched");

        // Posts focused → only the posts width moves.
        app.focus = Focus::Posts;
        app.adjust_pane(-4);
        assert_eq!(app.prefs.posts_width, 32);
        assert_eq!(app.prefs.sidebar_width, 32, "sidebar width untouched");

        // Reader is the flexible remainder — nothing to resize.
        app.focus = Focus::Reader;
        app.adjust_pane(2);
        assert_eq!((app.prefs.sidebar_width, app.prefs.posts_width), (32, 32));
    }

    #[test]
    fn footer_click_opens_then_dismisses_status_detail() {
        use crate::input::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::layout::Rect;
        let mut app = test_app(Prefs::for_test());
        app.status = "a long error message worth reading in full".into();
        // Only the status text region (left side) is the click target — not the whole footer.
        app.rects.status = Rect {
            x: 0,
            y: 23,
            width: 20,
            height: 1,
        };
        let click_at = |col| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row: 23,
            modifiers: KeyModifiers::NONE,
        };
        // Clicking the hints region (outside the status rect) does nothing.
        app.on_mouse(click_at(60));
        assert_eq!(
            app.mode,
            Mode::Browse,
            "clicking the hints doesn't open the status"
        );
        // Clicking the status text opens the popup with the full text.
        app.on_mouse(click_at(4));
        assert_eq!(app.mode, Mode::StatusDetail);
        assert_eq!(
            app.status_detail,
            "a long error message worth reading in full"
        );
        // Any click dismisses it (without triggering a pane action behind it).
        app.on_mouse(click_at(4));
        assert_eq!(app.mode, Mode::Browse);
    }

    // --- publication picker -------------------------------------------------------

    fn picker_choices() -> Vec<(String, String, bool)> {
        vec![
            ("at://r/p/1".into(), "One".into(), true),
            ("at://r/p/2".into(), "Two".into(), true),
            ("at://r/p/3".into(), "Three".into(), true),
        ]
    }

    #[test]
    fn publication_picker_toggles_select_all_and_none() {
        use crate::input::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app(Prefs::for_test());
        app.publication_choices = picker_choices();
        app.menu_sel = 0;
        app.mode = Mode::PublicationPicker;
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);

        // Space toggles only the row under the cursor.
        app.publication_picker_key(key(KeyCode::Char(' ')));
        assert!(!app.publication_choices[0].2);
        assert!(app.publication_choices[1].2);

        // 'n' clears all; 'a' re-selects all.
        app.publication_picker_key(key(KeyCode::Char('n')));
        assert!(app.publication_choices.iter().all(|c| !c.2));
        app.publication_picker_key(key(KeyCode::Char('a')));
        assert!(app.publication_choices.iter().all(|c| c.2));

        // Down advances the cursor, clamped to the last row.
        app.publication_picker_key(key(KeyCode::Down));
        assert_eq!(app.menu_sel, 1);
        app.menu_sel = 2;
        app.publication_picker_key(key(KeyCode::Down));
        assert_eq!(app.menu_sel, 2, "cursor clamps at the last choice");
    }

    #[test]
    fn publication_picker_enter_follows_only_the_checked_subset() {
        use crate::input::{KeyCode, KeyEvent, KeyModifiers};
        use crate::worker::ToWorker;
        use std::sync::mpsc::channel;

        let (tx, rx) = channel::<ToWorker>();
        let mut app = App::new(tx, Prefs::for_test());
        app.publication_choices = vec![
            ("at://r/p/1".into(), "One".into(), true),
            ("at://r/p/2".into(), "Two".into(), false),
            ("at://r/p/3".into(), "Three".into(), true),
        ];
        app.mode = Mode::PublicationPicker;
        // Drain the startup commands (`App::new` sends `LoadHome`) so we read only the picker's.
        while rx.try_recv().is_ok() {}
        app.publication_picker_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Only the checked publications are sent to the worker; the picker then closes.
        let sent = rx.try_recv().expect("a follow command was sent");
        match sent {
            ToWorker::FollowPublications(uris) => assert_eq!(
                uris,
                vec!["at://r/p/1".to_string(), "at://r/p/3".to_string()]
            ),
            _ => panic!("expected FollowPublications"),
        }
        assert_eq!(app.mode, Mode::Browse);
        assert!(app.publication_choices.is_empty());
    }

    // --- unread badges + load-older state ----------------------------------------

    #[test]
    fn unread_counts_and_markers_fold_in_and_update_live() {
        use crate::worker::FromWorker;
        use standard_core::model::RichDoc;

        let mut app = test_app(Prefs::for_test());
        app.open_pub = Some("at://p/1".into());

        // Feeds → per-feed unread counts for the sidebar.
        app.apply(FromWorker::Feeds {
            feeds: vec![feed("at://p/1")],
            unread: vec![("at://p/1".to_string(), 2)],
        });
        assert_eq!(app.unread_counts.get("at://p/1").copied(), Some(2));

        // Docs → the open feed's read-set + whether older posts remain.
        app.apply(FromWorker::Docs {
            publication: "at://p/1".into(),
            docs: vec![doc("at://d/1", "at://p/1"), doc("at://d/2", "at://p/1")],
            read_uris: vec!["at://d/1".to_string()],
            has_older: true,
        });
        assert!(app.read_uris.contains("at://d/1"));
        assert!(app.has_older);

        // Opening the still-unread post marks it read locally and drops the count (2 → 1).
        app.reading_uri = Some("at://d/2".into());
        app.apply(FromWorker::Doc {
            uri: "at://d/2".into(),
            body: RichDoc::default(),
            publishing_platform: None,
            from_cache: false,
        });
        assert!(app.read_uris.contains("at://d/2"));
        assert_eq!(app.unread_counts.get("at://p/1").copied(), Some(1));

        // Re-applying the same doc must not double-decrement.
        app.apply(FromWorker::Doc {
            uri: "at://d/2".into(),
            body: RichDoc::default(),
            publishing_platform: None,
            from_cache: false,
        });
        assert_eq!(app.unread_counts.get("at://p/1").copied(), Some(1));
    }

    #[test]
    fn cached_open_schedules_one_background_freshen() {
        use crate::worker::{FromWorker, ToWorker};
        use standard_core::model::RichDoc;
        use std::sync::mpsc::channel;

        let (tx, rx) = channel::<ToWorker>();
        let mut app = App::new(tx, Prefs::for_test());
        while rx.try_recv().is_ok() {} // drain startup (LoadHome)

        let cached = |uri: &str| FromWorker::Doc {
            uri: uri.into(),
            body: RichDoc::default(),
            publishing_platform: None,
            from_cache: true,
        };

        // A cache-served body schedules exactly one freshen for that post.
        app.reading_uri = Some("at://d/9".into());
        app.apply(cached("at://d/9"));
        let n = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|c| matches!(c, ToWorker::FreshenDoc(u) if u.as_str() == "at://d/9"))
            .count();
        assert_eq!(n, 1, "one freshen scheduled on a cached open");

        // Re-opening it (cache-first again) does not re-schedule — once per session.
        app.apply(cached("at://d/9"));
        assert!(
            !std::iter::from_fn(|| rx.try_recv().ok())
                .any(|c| matches!(c, ToWorker::FreshenDoc(_))),
            "no re-freshen on re-open"
        );

        // A freshly fetched body (from_cache:false) is already current → never freshened.
        app.reading_uri = Some("at://d/10".into());
        app.apply(FromWorker::Doc {
            uri: "at://d/10".into(),
            body: RichDoc::default(),
            publishing_platform: None,
            from_cache: false,
        });
        assert!(
            !std::iter::from_fn(|| rx.try_recv().ok())
                .any(|c| matches!(c, ToWorker::FreshenDoc(_))),
            "a fresh fetch isn't freshened again"
        );
    }
}
