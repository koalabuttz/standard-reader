//! The terminal UI: theme, the `RichDoc` renderer, and the screen drawing.

pub mod doc;
pub mod reader;
pub mod theme;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{Action, App, Focus, Mode};
use theme::Theme;

/// Draw the whole UI for one frame. Records pane rects on `app` for mouse hit-testing.
pub fn draw(f: &mut Frame, app: &mut App, theme: &Theme) {
    let area = f.area();
    f.render_widget(Block::new().style(theme.base()), area); // base fill

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let cols = Layout::horizontal([Constraint::Length(30), Constraint::Min(0)]).split(rows[0]);
    let (left, right, footer) = (cols[0], cols[1], rows[1]);

    if app.mode == Mode::DocList {
        app.rects.list = left;
        draw_doclist(f, app, theme, left);
    } else {
        app.rects.sidebar = left;
        draw_sidebar(f, app, theme, left);
    }
    app.rects.reader = right;
    reader::draw(f, app, theme, right);
    draw_footer(f, app, theme, footer);

    match app.mode {
        Mode::Help => draw_help(f, theme, area),
        Mode::Search => draw_input(f, app, theme, area, "Search"),
        Mode::AddFeed => draw_input(f, app, theme, area, "Add a blog — handle, DID, or URL"),
        Mode::SignIn => draw_input(f, app, theme, area, "Sign in — your handle or DID"),
        Mode::Palette => draw_palette(f, app, theme, area),
        Mode::SyncPrompt => draw_sync_prompt(f, app, theme, area),
        _ => {}
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
    // The signed-in identity (or a hint to sign in) along the panel's bottom edge.
    let account = match &app.account {
        Some(a) => format!(" @{} ", a.handle),
        None => " not signed in · L ".into(),
    };
    let block = panel(theme, "Feeds", focused).title_bottom(Span::styled(account, theme.dim_style()));
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

fn draw_doclist(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let title = if app.list_title.is_empty() {
        "Documents".to_string()
    } else {
        app.list_title.clone()
    };
    let block = panel(theme, &title, true);
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
    let hints = match app.mode {
        Mode::Browse => {
            "a add · ⇥ focus · enter open · o browser · / search · L sign in · : palette · ? help"
        }
        Mode::DocList => "↑↓ select · enter read · o browser · esc back · / search",
        Mode::Search | Mode::AddFeed | Mode::SignIn => "type · enter submit · esc cancel",
        Mode::Palette => "↑↓ choose · enter run · esc cancel",
        Mode::SyncPrompt => "s subscribe · r remove · esc dismiss",
        Mode::Help => "any key to close",
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
        ("i", "toggle images (text-only mode)"),
        ("m", "mark the open post read"),
        ("L", "sign in / out (atproto)"),
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
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        popup,
    );
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
        let mut app = App::new(tx, Picker::halfblocks());
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

        let theme = Theme::modern_dark();
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
        terminal.draw(|f| draw(f, &mut app, &theme)).unwrap();
        let text = buffer_text(terminal.backend().buffer());

        assert!(text.contains("Feeds"), "sidebar title");
        assert!(text.contains("half baked"), "feed name in sidebar");
        assert!(text.contains("Hello world"), "reader shows the open doc");
        assert!(text.contains("add"), "footer key hints");
    }
}
