//! UI state and the update logic. The `App` is a pure state machine: it turns key/mouse
//! events into state transitions plus [`ToWorker`] commands, and folds [`FromWorker`]
//! results back in. It owns no I/O (the worker does) and renders from this snapshot.

use std::sync::mpsc::Sender;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use standard_core::model::{Document, Publication, RichDoc};

use crate::worker::{FromWorker, ToWorker};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Reader,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browse,
    DocList,
    Search,
    Palette,
    Help,
    AddFeed,
}

/// Palette actions (also reachable by their direct keys).
#[derive(Clone, Copy)]
pub enum Action {
    AddFeed,
    Search,
    Refresh,
    MarkRead,
    Help,
    Quit,
}

impl Action {
    pub const ALL: [Action; 6] =
        [Self::AddFeed, Self::Search, Self::Refresh, Self::MarkRead, Self::Help, Self::Quit];

    pub fn label(self) -> &'static str {
        match self {
            Action::AddFeed => "Add feed",
            Action::Search => "Search",
            Action::Refresh => "Refresh feed",
            Action::MarkRead => "Mark read",
            Action::Help => "Help",
            Action::Quit => "Quit",
        }
    }
}

/// Pane rectangles from the last render, for mouse hit-testing.
#[derive(Default, Clone, Copy)]
pub struct Rects {
    pub sidebar: Rect,
    pub list: Rect,
    pub reader: Rect,
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
    pub scroll: u16,
    pub input: String,
    pub palette_sel: usize,
    pub status: String,
    pub loading: bool,
    pub should_quit: bool,
    pub rects: Rects,
    tx: Sender<ToWorker>,
}

impl App {
    pub fn new(tx: Sender<ToWorker>) -> Self {
        let app = Self {
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
            scroll: 0,
            input: String::new(),
            palette_sel: 0,
            status: "Loading… (press ? for help, a to add a feed)".into(),
            loading: true,
            should_quit: false,
            rects: Rects::default(),
            tx,
        };
        app.send(ToWorker::LoadHome);
        app
    }

    fn send(&self, msg: ToWorker) {
        let _ = self.tx.send(msg);
    }

    // --- folding worker results -------------------------------------------------

    pub fn apply(&mut self, evt: FromWorker) {
        match evt {
            FromWorker::Feeds(feeds) => {
                self.feeds = feeds;
                self.feed_sel = self.feed_sel.min(self.feeds.len().saturating_sub(1));
                self.loading = false;
                self.status = if self.feeds.is_empty() {
                    "No feeds yet — press a to add a blog by handle".into()
                } else {
                    format!("{} feed(s)", self.feeds.len())
                };
            }
            FromWorker::Docs { publication, docs } => {
                // Ignore a late update for a feed we've since navigated away from.
                if self.open_pub.as_deref() == Some(publication.as_str()) {
                    self.docs = docs;
                    self.doc_sel = self.doc_sel.min(self.docs.len().saturating_sub(1));
                    self.loading = false;
                }
            }
            FromWorker::Doc { uri, body } => {
                if self.reading_uri.as_deref() == Some(uri.as_str()) || self.reading_uri.is_none() {
                    self.reading = Some(body);
                    self.scroll = 0;
                }
                self.loading = false;
            }
            FromWorker::Results(results) => {
                self.docs = results;
                self.doc_sel = 0;
                self.list_title = format!("Search: {}", self.input);
                self.mode = Mode::DocList;
                self.loading = false;
                self.status = format!("{} result(s)", self.docs.len());
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
            Mode::Search | Mode::AddFeed => self.input_key(key),
            Mode::Palette => self.palette_key(key),
            Mode::Help => self.mode = Mode::Browse,
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
            KeyCode::Tab => self.toggle_focus(),
            KeyCode::Enter if self.focus == Focus::Sidebar => self.open_feed(),
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
            }
            KeyCode::Char('q') => self.quit(),
            KeyCode::Enter => self.open_doc(),
            KeyCode::Down | KeyCode::Char('j') => {
                self.doc_sel = (self.doc_sel + 1).min(self.docs.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => self.doc_sel = self.doc_sel.saturating_sub(1),
            KeyCode::Char('g') => self.doc_sel = 0,
            KeyCode::Char('G') => self.doc_sel = self.docs.len().saturating_sub(1),
            KeyCode::Char('/') => self.enter_input(Mode::Search),
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
                if hit(self.rects.reader, ev.column, ev.row).is_some() {
                    self.scroll = self.scroll.saturating_add(3);
                } else {
                    self.move_down();
                }
            }
            MouseEventKind::ScrollUp => {
                if hit(self.rects.reader, ev.column, ev.row).is_some() {
                    self.scroll = self.scroll.saturating_sub(3);
                } else {
                    self.move_up();
                }
            }
            MouseEventKind::Down(_) => {
                if self.mode == Mode::DocList {
                    if let Some(i) = hit(self.rects.list, ev.column, ev.row)
                        && i < self.docs.len() {
                            self.doc_sel = i;
                            self.open_doc();
                        }
                } else if let Some(i) = hit(self.rects.sidebar, ev.column, ev.row)
                    && i < self.feeds.len() {
                        self.feed_sel = i;
                        self.focus = Focus::Sidebar;
                        self.open_feed();
                    }
            }
            _ => {}
        }
    }

    // --- actions ----------------------------------------------------------------

    pub fn palette_matches(&self) -> Vec<Action> {
        Action::ALL.iter().copied().filter(|a| fuzzy(&self.input, a.label())).collect()
    }

    fn run(&mut self, action: Action) {
        match action {
            Action::AddFeed => self.enter_input(Mode::AddFeed),
            Action::Search => self.enter_input(Mode::Search),
            Action::Refresh => self.refresh_current_feed(),
            Action::MarkRead => self.mark_read(),
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
            _ => {}
        }
        self.mode = Mode::Browse;
        self.input.clear();
    }

    fn open_feed(&mut self) {
        if let Some(p) = self.feeds.get(self.feed_sel) {
            self.list_title = p.name.clone();
            self.open_pub = Some(p.uri.clone());
            self.docs.clear();
            self.doc_sel = 0;
            self.loading = true;
            self.mode = Mode::DocList;
            self.send(ToWorker::OpenFeed(p.uri.clone()));
        }
    }

    fn open_doc(&mut self) {
        if let Some(d) = self.docs.get(self.doc_sel) {
            self.reading_title = if d.title.is_empty() { "(untitled)".into() } else { d.title.clone() };
            self.reading_uri = Some(d.uri.clone());
            self.reading = None;
            self.scroll = 0;
            self.loading = true;
            self.mode = Mode::Browse;
            self.focus = Focus::Reader;
            self.send(ToWorker::OpenDoc(d.uri.clone()));
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
            && let Some(p) = self.feeds.get(self.feed_sel) {
                self.status = format!("unfollowed {}", p.name);
                self.send(ToWorker::Unfollow(p.uri.clone()));
            }
    }

    fn mark_read(&mut self) {
        if let Some(uri) = &self.reading_uri {
            self.send(ToWorker::SetRead(uri.clone(), true));
            self.status = "marked read".into();
        }
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Sidebar => Focus::Reader,
            Focus::Reader => Focus::Sidebar,
        };
    }

    fn move_down(&mut self) {
        match self.focus {
            Focus::Sidebar => self.feed_sel = (self.feed_sel + 1).min(self.feeds.len().saturating_sub(1)),
            Focus::Reader => self.scroll = self.scroll.saturating_add(1),
        }
    }

    fn move_up(&mut self) {
        match self.focus {
            Focus::Sidebar => self.feed_sel = self.feed_sel.saturating_sub(1),
            Focus::Reader => self.scroll = self.scroll.saturating_sub(1),
        }
    }

    fn go_top(&mut self) {
        match self.focus {
            Focus::Sidebar => self.feed_sel = 0,
            Focus::Reader => self.scroll = 0,
        }
    }

    fn go_bottom(&mut self) {
        if self.focus == Focus::Sidebar {
            self.feed_sel = self.feeds.len().saturating_sub(1);
        }
    }

    fn quit(&mut self) {
        self.should_quit = true;
        self.send(ToWorker::Quit);
    }
}

/// Row index clicked inside a bordered list `rect` (None if outside / on the border).
fn hit(rect: Rect, x: u16, y: u16) -> Option<usize> {
    let inside = x > rect.x && x < rect.right().saturating_sub(1) && y > rect.y && y < rect.bottom().saturating_sub(1);
    inside.then(|| (y - rect.y - 1) as usize)
}

/// Case-insensitive subsequence match (the command-palette filter).
pub fn fuzzy(query: &str, candidate: &str) -> bool {
    let mut q = query.chars().filter(|c| !c.is_whitespace()).map(|c| c.to_ascii_lowercase()).peekable();
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

#[cfg(test)]
mod tests {
    use super::fuzzy;

    #[test]
    fn fuzzy_subsequence() {
        assert!(fuzzy("", "anything"));
        assert!(fuzzy("adf", "Add feed"));
        assert!(fuzzy("srch", "Search"));
        assert!(fuzzy("quit", "Quit"));
        assert!(!fuzzy("xyz", "Add feed"));
        assert!(!fuzzy("feedx", "Add feed"));
    }
}
