//! The terminal UI: theme, the `RichDoc` renderer, and the screen drawing.

pub mod doc;
pub mod reader;
pub mod theme;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{Action, App, Focus, Mode};
use crate::prefs::{LayoutKind, PANE_MAX, PANE_MIN};
use theme::{PRESETS, SLOTS, Theme, ThemeColors};

/// Draw the whole UI for one frame. Records pane rects on `app` for mouse hit-testing.
pub fn draw(f: &mut Frame, app: &mut App) {
    // Resolve the effective theme/layout for this frame, then copy the theme out into a local so
    // the renderer can hold `&theme` and `&mut app` at once (the reader mutates `app.link_rects`
    // while reading the theme — a borrow of `app.theme` would conflict).
    app.recompute_appearance();
    let theme = app.theme;
    let theme = &theme;

    let area = f.area();
    f.render_widget(Block::new().style(theme.base()), area); // base fill

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let (body, footer) = (rows[0], rows[1]);

    // Reset pane rects each frame; each layout sets only the panes it draws (the rest stay
    // zero-area, so mouse hit-testing never matches an invisible pane).
    app.rects = crate::app::Rects::default();
    match app.layout {
        LayoutKind::TwoPane => draw_two_pane(f, app, theme, body),
        LayoutKind::ThreePane => draw_three_pane(f, app, theme, body),
        LayoutKind::OnePane => draw_one_pane(f, app, theme, body),
        LayoutKind::DrillDown => draw_drill_down(f, app, theme, body),
    }
    draw_footer(f, app, theme, footer);

    match app.mode {
        Mode::Help => draw_help(f, theme, area),
        Mode::Search => draw_input(f, app, theme, area, "Search"),
        Mode::AddFeed => draw_input(f, app, theme, area, "Add a blog — handle, DID, or URL"),
        Mode::SignIn => draw_input(f, app, theme, area, "Log in — your handle or DID"),
        Mode::Palette => draw_palette(f, app, theme, area),
        Mode::SyncPrompt => draw_sync_prompt(f, app, theme, area),
        Mode::ThemePicker => draw_theme_picker(f, app, theme, area),
        Mode::ThemeEditor => draw_theme_editor(f, app, theme, area),
        Mode::LayoutPicker => draw_layout_picker(f, app, theme, area),
        Mode::BlogMenu => draw_blog_menu(f, app, theme, area),
        _ => {}
    }
}

/// Clamp a configured pane width to a sane range for the current terminal width — never below
/// the minimum, never so wide the reader can't breathe (defensive against a hand-edited
/// `prefs.toml` or a tiny window).
fn clamp_pane(width: u16, total: u16) -> u16 {
    let hi = PANE_MAX.min(total.saturating_sub(10)).max(PANE_MIN);
    width.clamp(PANE_MIN, hi)
}

/// Two-pane (the default): the left pane is the feed list, or — once a feed is opened — its post
/// list (Mode::DocList); the reader fills the right.
fn draw_two_pane(f: &mut Frame, app: &mut App, theme: &Theme, body: Rect) {
    let sw = clamp_pane(app.prefs.sidebar_width, body.width);
    let cols = Layout::horizontal([Constraint::Length(sw), Constraint::Min(0)]).split(body);
    let (left, right) = (cols[0], cols[1]);
    if app.mode == Mode::DocList {
        app.rects.posts = left;
        draw_doclist(f, app, theme, left, true);
    } else {
        app.rects.sidebar = left;
        draw_sidebar(f, app, theme, left);
    }
    app.rects.reader = right;
    reader::draw(f, app, theme, right);
}

/// Three-pane: feeds | posts | reader, all visible at once (feeds + posts sized independently).
fn draw_three_pane(f: &mut Frame, app: &mut App, theme: &Theme, body: Rect) {
    let sw = clamp_pane(app.prefs.sidebar_width, body.width);
    let pw = clamp_pane(app.prefs.posts_width, body.width);
    let cols = Layout::horizontal([
        Constraint::Length(sw),
        Constraint::Length(pw),
        Constraint::Min(0),
    ])
    .split(body);
    app.rects.sidebar = cols[0];
    draw_sidebar(f, app, theme, cols[0]);
    app.rects.posts = cols[1];
    draw_doclist(f, app, theme, cols[1], app.focus == Focus::Posts);
    app.rects.reader = cols[2];
    reader::draw(f, app, theme, cols[2]);
}

/// One-pane: only the focused pane, full width.
fn draw_one_pane(f: &mut Frame, app: &mut App, theme: &Theme, body: Rect) {
    match app.focus {
        Focus::Sidebar => {
            app.rects.sidebar = body;
            draw_sidebar(f, app, theme, body);
        }
        Focus::Posts => {
            app.rects.posts = body;
            draw_doclist(f, app, theme, body, true);
        }
        Focus::Reader => {
            app.rects.reader = body;
            reader::draw(f, app, theme, body);
        }
    }
}

/// Drill-down: a collapsing layout that grows with focus — feeds (full) → feeds + posts → just
/// the open post (full). `Tab` descends a level, `Esc` ascends.
fn draw_drill_down(f: &mut Frame, app: &mut App, theme: &Theme, body: Rect) {
    match app.focus {
        // Stage 1 — the feed list, full width (choose a feed).
        Focus::Sidebar => {
            app.rects.sidebar = body;
            draw_sidebar(f, app, theme, body);
        }
        // Stage 2 — feeds + posts side by side (choose a post; feeds stay visible to switch).
        Focus::Posts => {
            let sw = clamp_pane(app.prefs.sidebar_width, body.width);
            let cols = Layout::horizontal([Constraint::Length(sw), Constraint::Min(0)]).split(body);
            app.rects.sidebar = cols[0];
            draw_sidebar(f, app, theme, cols[0]);
            app.rects.posts = cols[1];
            draw_doclist(f, app, theme, cols[1], true);
        }
        // Stage 3 — just the open post, full width.
        Focus::Reader => {
            app.rects.reader = body;
            reader::draw(f, app, theme, body);
        }
    }
}

fn panel<'a>(theme: &Theme, title: &'a str, focused: bool) -> Block<'a> {
    let border = if focused { theme.accent } else { theme.border };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            format!(" {title} "),
            theme.accent_style().add_modifier(Modifier::BOLD),
        ))
        .style(theme.base())
}

fn draw_sidebar(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    // The logged-in identity (or a hint to log in) along the panel's bottom edge.
    let account = match &app.account {
        Some(a) => format!(" @{} ", a.handle),
        None => " not logged in · L ".into(),
    };
    let block =
        panel(theme, "Feeds", focused).title_bottom(Span::styled(account, theme.dim_style()));
    if app.feeds.is_empty() {
        let hint = Paragraph::new("No feeds yet.\n\nPress a to add a blog\nby handle.")
            .style(theme.dim_style())
            .alignment(Alignment::Center)
            .block(block);
        f.render_widget(hint, area);
        return;
    }
    let items: Vec<ListItem> = app
        .feeds
        .iter()
        .map(|p| ListItem::new(p.name.clone()))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.selected())
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(app.feed_sel));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_doclist(f: &mut Frame, app: &App, theme: &Theme, area: Rect, focused: bool) {
    let title = if app.list_title.is_empty() {
        "Documents".to_string()
    } else {
        app.list_title.clone()
    };
    let block = panel(theme, &title, focused);
    if app.docs.is_empty() {
        let msg = if app.loading {
            "loading…"
        } else {
            "No documents."
        };
        f.render_widget(
            Paragraph::new(msg)
                .style(theme.dim_style())
                .alignment(Alignment::Center)
                .block(block),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = app
        .docs
        .iter()
        .map(|d| {
            let title = if d.title.is_empty() {
                "(untitled)"
            } else {
                &d.title
            };
            let date = d.published_at.get(..10).unwrap_or("");
            ListItem::new(Line::from(vec![
                Span::styled(title.to_string(), theme.body()),
                Span::styled(format!("  {date}"), theme.dim_style()),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.selected())
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(app.doc_sel));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_footer(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let login_hint = if app.account.is_some() {
        "L log out"
    } else {
        "L log in"
    };
    let hints: String = match app.mode {
        Mode::Browse => {
            format!(
                "a add · ⇥ focus · enter open · \\ layout · t theme · / search · {login_hint} · : palette · ? help"
            )
        }
        Mode::DocList => "↑↓ select · enter read · o browser · esc back · / search".into(),
        Mode::Search | Mode::AddFeed | Mode::SignIn => "type · enter submit · esc cancel".into(),
        Mode::Palette => "↑↓ choose · enter run · esc cancel".into(),
        Mode::SyncPrompt => "s subscribe · r remove · esc dismiss".into(),
        Mode::ThemePicker | Mode::LayoutPicker | Mode::BlogMenu => {
            "↑↓ choose · enter select · esc cancel".into()
        }
        Mode::ThemeEditor => {
            "↑↓ slot · ←→ channel · -/+ adjust · [ ] ±16 · enter save · esc cancel".into()
        }
        Mode::Help => "any key to close".into(),
    };
    let line = Line::from(vec![
        Span::styled(format!(" {} ", app.status), theme.dim_style()),
        Span::styled("· ", Style::default().fg(theme.border)),
        Span::styled(hints, theme.accent_style()),
    ]);
    f.render_widget(Paragraph::new(line).style(theme.base()), area);
}

fn draw_input(f: &mut Frame, app: &App, theme: &Theme, area: Rect, title: &str) {
    let popup = centered(area, 64, 3);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.accent_style())
        .title(Span::styled(
            format!(" {title} "),
            theme.accent_style().add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(theme.panel));
    let text = Line::from(vec![
        Span::styled(&app.input, theme.body()),
        Span::styled("▏", theme.accent_style()),
    ]);
    f.render_widget(Paragraph::new(text).block(block), popup);
}

fn draw_palette(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let matches = app.palette_matches();
    let height = (matches.len() as u16 + 4).min(area.height);
    let popup = centered(area, 50, height);
    f.render_widget(Clear, popup);

    let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(popup);
    let input_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.accent_style())
        .title(Span::styled(
            " Command ",
            theme.accent_style().add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(theme.panel));
    let input = Line::from(vec![
        Span::styled(&app.input, theme.body()),
        Span::styled("▏", theme.accent_style()),
    ]);
    f.render_widget(Paragraph::new(input).block(input_block), rows[0]);

    let items: Vec<ListItem> = matches
        .iter()
        .map(|a: &Action| ListItem::new(a.label()))
        .collect();
    let list = List::new(items)
        .block(Block::new().style(Style::default().bg(theme.panel)))
        .highlight_style(theme.selected())
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !matches.is_empty() {
        state.select(Some(app.palette_sel.min(matches.len() - 1)));
    }
    f.render_stateful_widget(list, rows[1], &mut state);
}

fn draw_help(f: &mut Frame, theme: &Theme, area: Rect) {
    let keys = [
        ("↑↓ / j k", "move selection / scroll"),
        ("⇥ Tab", "switch sidebar ↔ reader"),
        ("Enter", "open feed, then open a post"),
        ("Esc", "back / close"),
        ("a", "add a blog (handle, DID, or URL)"),
        ("/", "search across feeds"),
        (": / Ctrl-P", "command palette"),
        ("r", "refresh the selected feed"),
        ("d", "unfollow the selected feed"),
        ("o", "open this post in your browser"),
        ("n / N", "focus next / prev link in the post"),
        ("Enter / click", "open the focused / clicked link"),
        ("i", "toggle images (text-only mode)"),
        ("t", "theme (presets + custom editor)"),
        ("\\", "cycle layout (1 / 2 / 3-pane, drill-down)"),
        ("< >", "narrow / widen the focused pane"),
        ("b", "customize this blog (theme / layout)"),
        ("m", "mark the open post read"),
        ("L", "log in / out (atproto)"),
        ("? ", "this help"),
        ("q", "quit"),
    ];
    let popup = centered(area, 52, keys.len() as u16 + 2);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.accent_style())
        .title(Span::styled(
            " Help ",
            theme.accent_style().add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(theme.panel));
    let lines: Vec<Line> = keys
        .iter()
        .map(|(k, desc)| {
            Line::from(vec![
                Span::styled(format!(" {k:<12}"), theme.accent_style()),
                Span::styled((*desc).to_string(), theme.body()),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

/// The subscription-reconciliation modal: local-only follows + the s/r/esc choices.
fn draw_sync_prompt(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let height = (app.sync_prompt.len() as u16 + 7).min(area.height);
    let popup = centered(area, 64, height);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.accent_style())
        .title(Span::styled(
            " Sync subscriptions ",
            theme.accent_style().add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(theme.panel));

    let bold = theme.accent_style().add_modifier(Modifier::BOLD);
    let mut lines = vec![
        Line::from(Span::styled(
            "Followed here but not in your atproto account:",
            theme.body(),
        )),
        Line::from(""),
    ];
    for (_, name) in &app.sync_prompt {
        lines.push(Line::from(vec![
            Span::styled("  • ", theme.accent_style()),
            Span::styled(name.clone(), theme.body()),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("s", bold),
        Span::styled(" subscribe   ", theme.body()),
        Span::styled("r", bold),
        Span::styled(" remove locally   ", theme.body()),
        Span::styled("esc", bold),
        Span::styled(" later", theme.body()),
    ]));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// The theme picker: each built-in preset (rendered in its own colours as a live swatch), then
/// a "Custom — edit colours" entry that opens the RGB editor.
fn draw_theme_picker(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let entries = PRESETS.len() + 1;
    let popup = centered(area, 40, (entries as u16 + 2).min(area.height));
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.accent_style())
        .title(Span::styled(
            " Theme ",
            theme.accent_style().add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(theme.panel));

    let mut items: Vec<ListItem> = PRESETS
        .iter()
        .map(|name| {
            let preview = Theme::from(&ThemeColors::preset(name).unwrap());
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {name:<14}"),
                    Style::default().fg(preview.fg).bg(preview.bg),
                ),
                Span::styled(" ██", Style::default().fg(preview.accent)),
                Span::styled("██", Style::default().fg(preview.accent2)),
            ]))
        })
        .collect();
    items.push(ListItem::new(Span::styled(
        " Custom — edit colours…",
        theme.body(),
    )));

    let list = List::new(items)
        .block(block)
        .highlight_style(theme.selected())
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(app.menu_sel.min(entries - 1)));
    f.render_stateful_widget(list, popup, &mut state);
}

/// The RGB colour editor: one row per theme slot (with a live swatch + hex + R/G/B values), the
/// selected slot/channel highlighted. The whole screen behind it previews the draft palette.
fn draw_theme_editor(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let Some(ed) = &app.theme_editor else {
        return;
    };
    let popup = centered(area, 52, (SLOTS.len() as u16 + 2).min(area.height));
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.accent_style())
        .title(Span::styled(
            " Edit theme ",
            theme.accent_style().add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(theme.panel));

    let lines: Vec<Line> = SLOTS
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let [r, g, b] = ed.draft.slot_rgb(i);
            let selected = i == ed.slot;
            let name_style = if selected {
                theme.body().add_modifier(Modifier::BOLD)
            } else {
                theme.body()
            };
            let mut spans = vec![
                Span::styled(if selected { "▸ " } else { "  " }, theme.accent_style()),
                Span::styled("██ ", Style::default().fg(Color::Rgb(r, g, b))),
                Span::styled(format!("{name:<11}"), name_style),
                Span::styled(format!("{:<8} ", ed.draft.slot(i)), theme.dim_style()),
            ];
            for (c, (label, val)) in [("R", r), ("G", g), ("B", b)].iter().enumerate() {
                let style = if selected && c == ed.channel {
                    theme.selected()
                } else if selected {
                    theme.body()
                } else {
                    theme.dim_style()
                };
                spans.push(Span::styled(format!(" {label}{val:>3}"), style));
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

/// The layout picker: the four arrangements, with a one-line description each.
fn draw_layout_picker(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let popup = centered(
        area,
        46,
        (LayoutKind::ALL.len() as u16 + 2).min(area.height),
    );
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.accent_style())
        .title(Span::styled(
            " Layout ",
            theme.accent_style().add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(theme.panel));
    let desc = |l: LayoutKind| match l {
        LayoutKind::OnePane => "one pane, full width",
        LayoutKind::TwoPane => "feeds/posts + reader",
        LayoutKind::ThreePane => "feeds | posts | reader",
        LayoutKind::DrillDown => "feeds → posts → post",
    };
    let items: Vec<ListItem> = LayoutKind::ALL
        .iter()
        .map(|&l| {
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {:<12}", l.label()), theme.body()),
                Span::styled(desc(l), theme.dim_style()),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.selected())
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(app.menu_sel.min(LayoutKind::ALL.len() - 1)));
    f.render_stateful_widget(list, popup, &mut state);
}

/// The per-blog customization menu: this publication's theme / layout override (or "global"),
/// plus an entry to clear back to the global appearance.
fn draw_blog_menu(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let Some(uri) = &app.menu_target else {
        return;
    };
    let name = app
        .feeds
        .iter()
        .find(|p| &p.uri == uri)
        .map(|p| p.name.as_str())
        .unwrap_or("this blog");
    let ov = app.prefs.blog(uri);
    let theme_val = ov
        .and_then(|o| o.theme.clone())
        .unwrap_or_else(|| "global".into());
    let layout_val = ov
        .and_then(|o| o.layout)
        .map(|l| l.label())
        .unwrap_or("global");

    let popup = centered(area, 44, 5.min(area.height));
    f.render_widget(Clear, popup);
    let title = format!(" Customize {name} ");
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.accent_style())
        .title(Span::styled(
            title,
            theme.accent_style().add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(theme.panel));
    let entries = [
        format!("Theme: {theme_val}"),
        format!("Layout: {layout_val}"),
        "Use global appearance".to_string(),
    ];
    let items: Vec<ListItem> = entries.iter().map(|e| ListItem::new(e.clone())).collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.selected())
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(app.menu_sel.min(2)));
    f.render_stateful_widget(list, popup, &mut state);
}

/// A centered rect of the given width/height, clamped to `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::ToWorker;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Position;
    use ratatui_image::picker::Picker;
    use standard_core::model::{Block, Inline, Publication, RichDoc};
    use std::sync::mpsc::channel;

    fn buffer_text(buf: &Buffer) -> String {
        let area = buf.area;
        let mut s = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = buf.cell(Position::new(x, y)) {
                    s.push_str(cell.symbol());
                }
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn renders_feed_reader_and_footer() {
        let (tx, _rx) = channel::<ToWorker>();
        let mut app = App::new(tx, Picker::halfblocks(), crate::prefs::Prefs::for_test());
        app.loading = false;
        app.feeds = vec![Publication {
            uri: "at://d/site.standard.publication/1".into(),
            url: "https://x.test".into(),
            name: "half baked".into(),
            description: None,
            icon: None,
        }];
        app.reading_title = "Hello world".into();
        app.reading = Some(RichDoc {
            blocks: vec![Block::Heading {
                level: 1,
                content: vec![Inline::Text("Hello world".into())],
            }],
        });

        let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(terminal.backend().buffer());

        assert!(text.contains("Feeds"), "sidebar title");
        assert!(text.contains("half baked"), "feed name in sidebar");
        assert!(text.contains("Hello world"), "reader shows the open doc");
        assert!(text.contains("add"), "footer key hints");
    }

    #[test]
    fn pane_width_clamps_to_range_and_terminal() {
        assert_eq!(clamp_pane(30, 90), 30, "in-range width is kept");
        assert_eq!(clamp_pane(5, 90), PANE_MIN, "floored at the minimum");
        assert_eq!(clamp_pane(200, 90), PANE_MAX, "capped at the maximum");
        assert!(
            clamp_pane(50, 30) <= 30,
            "never wider than a narrow terminal"
        );
    }

    #[test]
    fn drill_down_sidebar_focus_shows_the_feed_list() {
        use crate::prefs::{LayoutKind, Prefs};
        let (tx, _rx) = channel::<ToWorker>();
        let mut app = App::new(tx, Picker::halfblocks(), Prefs::for_test());
        app.prefs.layout = LayoutKind::DrillDown;
        app.loading = false;
        app.feeds = vec![Publication {
            uri: "at://p/1".into(),
            url: "https://x.test".into(),
            name: "my feed".into(),
            description: None,
            icon: None,
        }];
        app.reading = Some(RichDoc {
            blocks: vec![Block::Paragraph(vec![Inline::Text("body".into())])],
        });
        // Even with a post open, focusing the feed list must surface it (not stay on posts|reader).
        app.focus = crate::app::Focus::Sidebar;
        let mut t = Terminal::new(TestBackend::new(80, 20)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(t.backend().buffer());
        assert!(
            text.contains("Feeds"),
            "feed list visible when sidebar focused"
        );
        assert!(text.contains("my feed"), "the feed name shows");
    }

    #[test]
    fn drill_down_posts_focus_shows_feeds_and_posts() {
        use crate::prefs::{LayoutKind, Prefs};
        let (tx, _rx) = channel::<ToWorker>();
        let mut app = App::new(tx, Picker::halfblocks(), Prefs::for_test());
        app.prefs.layout = LayoutKind::DrillDown;
        app.loading = false;
        app.open_pub = Some("at://p/1".into());
        app.feeds = vec![Publication {
            uri: "at://p/1".into(),
            url: "https://x.test".into(),
            name: "my feed".into(),
            description: None,
            icon: None,
        }];
        app.docs = vec![standard_core::model::Document {
            uri: "at://d/1".into(),
            title: "a post".into(),
            description: None,
            publication: "at://p/1".into(),
            published_at: "2026-01-01".into(),
            updated_at: None,
            cover_image: None,
            text_content: None,
            tags: vec![],
            path: None,
        }];
        // Stage 2 (posts focused): both the feed list and the post list are on screen.
        app.focus = crate::app::Focus::Posts;
        let mut t = Terminal::new(TestBackend::new(80, 20)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(t.backend().buffer());
        assert!(text.contains("Feeds"), "feeds pane visible in stage 2");
        assert!(text.contains("a post"), "posts pane visible in stage 2");
    }

    #[test]
    fn customization_overlays_and_layouts_render_without_panic() {
        use crate::app::{Mode, ThemeEditor};
        use crate::prefs::{LayoutKind, Prefs};
        use theme::ThemeColors;

        // Every new overlay, across all layouts, at a normal and a cramped size — must not panic
        // (guards the centered-popup clamping + slot/preset indexing).
        for (w, h) in [(90u16, 24u16), (24, 8)] {
            for layout in LayoutKind::ALL {
                for mode in [
                    Mode::Browse,
                    Mode::ThemePicker,
                    Mode::ThemeEditor,
                    Mode::LayoutPicker,
                    Mode::BlogMenu,
                ] {
                    let (tx, _rx) = channel::<ToWorker>();
                    let mut app = App::new(tx, Picker::halfblocks(), Prefs::for_test());
                    app.loading = false;
                    app.prefs.layout = layout;
                    app.feeds = vec![Publication {
                        uri: "at://p/1".into(),
                        url: "https://x.test".into(),
                        name: "feed".into(),
                        description: None,
                        icon: None,
                    }];
                    app.mode = mode;
                    if mode == Mode::ThemeEditor {
                        app.theme_editor = Some(ThemeEditor {
                            draft: ThemeColors::modern_dark(),
                            slot: 0,
                            channel: 0,
                        });
                    }
                    if mode == Mode::BlogMenu {
                        app.menu_target = Some("at://p/1".into());
                    }
                    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
                    t.draw(|f| draw(f, &mut app)).unwrap();
                }
            }
        }
    }
}
