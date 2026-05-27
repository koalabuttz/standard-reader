//! `RichDoc` → ratatui `Text`. Pure styling of the neutral AST; the reader pane wraps and
//! scrolls the result. Block-level images and callouts are split into their own segments by
//! the reader (for real image rendering / tinted boxes); the arms here are the text-flow
//! fallback (e.g. when nested inside a quote or list).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use standard_core::model::{Block, Inline};

use super::theme::Theme;

/// Render a sequence of blocks to text (used by the block-flow reader for the text runs
/// between images; takes references so a run can be built without cloning blocks).
pub fn blocks_to_text<'a>(
    blocks: impl IntoIterator<Item = &'a Block>,
    theme: &Theme,
) -> Text<'static> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, block) in blocks.into_iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw("")); // blank line between blocks
        }
        block_lines(block, theme, &mut lines);
    }
    Text::from(lines)
}

fn block_lines(block: &Block, theme: &Theme, out: &mut Vec<Line<'static>>) {
    match block {
        Block::Heading { level, content } => {
            let hash = Span::styled("#".repeat(*level as usize) + " ", theme.dim_style());
            for (i, mut line) in inline_lines(content, theme.heading(), theme)
                .into_iter()
                .enumerate()
            {
                if i == 0 {
                    line.spans.insert(0, hash.clone());
                }
                out.push(line);
            }
        }
        Block::Paragraph(content) => out.extend(inline_lines(content, theme.body(), theme)),
        Block::Quote(blocks) => {
            let mut inner = Vec::new();
            for b in blocks {
                block_lines(b, theme, &mut inner);
            }
            let bar = Span::styled("▍ ", Style::default().fg(theme.accent2));
            for mut line in inner {
                line.spans.insert(0, bar.clone());
                out.push(line);
            }
        }
        Block::List { ordered, items } => {
            for (i, item) in items.iter().enumerate() {
                let marker = if *ordered {
                    format!("{}. ", i + 1)
                } else {
                    "• ".to_string()
                };
                let mut inner = Vec::new();
                for b in item {
                    block_lines(b, theme, &mut inner);
                }
                for (j, mut line) in inner.into_iter().enumerate() {
                    let prefix = if j == 0 {
                        marker.clone()
                    } else {
                        "  ".to_string()
                    };
                    line.spans
                        .insert(0, Span::styled(prefix, theme.accent_style()));
                    out.push(line);
                }
            }
        }
        Block::Code { lang, text } => {
            let label = lang.clone().unwrap_or_default();
            out.push(Line::styled(format!("```{label}"), theme.dim_style()));
            for l in text.lines() {
                out.push(Line::styled(l.to_string(), theme.code_block()));
            }
            out.push(Line::styled("```", theme.dim_style()));
        }
        Block::Image(img) => {
            let label = if img.alt.is_empty() {
                "(image)".to_string()
            } else {
                img.alt.clone()
            };
            out.push(Line::styled(format!("🖼  {label}"), theme.dim_style()));
        }
        Block::ImageGrid(images) => {
            // Fallback (nested grid); the reader lays a top-level grid out in columns.
            for img in images {
                let label = if img.alt.is_empty() {
                    "(image)"
                } else {
                    &img.alt
                };
                out.push(Line::styled(format!("🖼  {label}"), theme.dim_style()));
            }
        }
        Block::Table { head, rows } => table_lines(head, rows, theme, out),
        Block::Callout {
            emoji,
            tint,
            content,
        } => {
            // Fallback (e.g. a callout nested in a quote/list); the reader draws top-level
            // callouts as a filled box instead. Here: a tinted left bar + emoji + text.
            let bar_color = tint
                .map(|(r, g, b)| Color::Rgb(r, g, b))
                .unwrap_or(theme.accent2);
            let bar = Span::styled("▌ ", Style::default().fg(bar_color));
            let mut lines = inline_lines(content, theme.body(), theme);
            if lines.is_empty() {
                lines.push(Line::default());
            }
            for (i, mut line) in lines.into_iter().enumerate() {
                if i == 0
                    && let Some(e) = emoji
                {
                    line.spans
                        .insert(0, Span::styled(format!("{e} "), theme.body()));
                }
                line.spans.insert(0, bar.clone());
                out.push(line);
            }
        }
        Block::Rule => out.push(Line::styled(
            "─".repeat(48),
            Style::default().fg(theme.border),
        )),
        // Resolved to an ImageGrid in `read::get_document`; only reached if that fetch failed.
        Block::GalleryRef { .. } => {
            out.push(Line::styled("🖼  (gallery)", theme.dim_style()))
        }
    }
}

/// A run of inline content as one wrappable `Text` (used by the reader's callout box).
pub fn inline_paragraph(content: &[Inline], theme: &Theme) -> Text<'static> {
    Text::from(inline_lines(content, theme.body(), theme))
}

/// Render a table with box-drawing borders. Cells are flattened to plain text, each column
/// sized to its widest cell (capped), so it lays out as styled `Line`s in the text flow.
fn table_lines(
    head: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    theme: &Theme,
    out: &mut Vec<Line<'static>>,
) {
    const MAX_COL: usize = 28;

    let to_row =
        |cells: &[Vec<Inline>]| -> Vec<String> { cells.iter().map(|c| inline_text(c)).collect() };
    let mut grid: Vec<Vec<String>> = Vec::new();
    if !head.is_empty() {
        grid.push(to_row(head));
    }
    grid.extend(rows.iter().map(|r| to_row(r)));

    let ncols = grid.iter().map(Vec::len).max().unwrap_or(0);
    if ncols == 0 {
        return;
    }
    let mut widths = vec![1usize; ncols];
    for row in &grid {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count().min(MAX_COL));
        }
    }

    let border = Style::default().fg(theme.border);
    let rule = |left: &str, mid: &str, right: &str| -> Line<'static> {
        let mut s = String::from(left);
        for (i, w) in widths.iter().enumerate() {
            s.push_str(&"─".repeat(w + 2));
            s.push_str(if i + 1 < ncols { mid } else { right });
        }
        Line::styled(s, border)
    };

    out.push(rule("┌", "┬", "┐"));
    let body_start = if head.is_empty() {
        0
    } else {
        out.push(row_line(&grid[0], &widths, ncols, theme.heading(), border));
        out.push(rule("├", "┼", "┤"));
        1
    };
    for row in &grid[body_start..] {
        out.push(row_line(row, &widths, ncols, theme.body(), border));
    }
    out.push(rule("└", "┴", "┘"));
}

fn row_line(
    cells: &[String],
    widths: &[usize],
    ncols: usize,
    cell_style: Style,
    border: Style,
) -> Line<'static> {
    let mut spans = Vec::with_capacity(ncols * 3 + 1);
    for (i, w) in widths.iter().enumerate() {
        spans.push(Span::styled("│ ", border));
        let text = cells.get(i).map(String::as_str).unwrap_or("");
        spans.push(Span::styled(
            format!("{:<width$}", truncate(text, *w), width = w),
            cell_style,
        ));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled("│", border));
    Line::from(spans)
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else if width == 0 {
        String::new()
    } else {
        let kept: String = s.chars().take(width - 1).collect();
        format!("{kept}…")
    }
}

/// Flatten inline content to plain text (for table cells).
fn inline_text(inlines: &[Inline]) -> String {
    let mut s = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(t) | Inline::Code(t) => s.push_str(t),
            Inline::Strong(c) | Inline::Emphasis(c) | Inline::Strike(c) | Inline::Underline(c) => {
                s.push_str(&inline_text(c))
            }
            Inline::Link { content, .. } => s.push_str(&inline_text(content)),
            Inline::Image(img) => s.push_str(&img.alt),
            Inline::LineBreak => s.push(' '),
        }
    }
    s
}

/// Inlines → one or more `Line`s, breaking on a top-level `LineBreak`.
fn inline_lines(inlines: &[Inline], base: Style, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    for inline in inlines {
        if matches!(inline, Inline::LineBreak) {
            lines.push(Line::from(std::mem::take(&mut cur)));
        } else {
            spans_into(inline, base, theme, &mut cur);
        }
    }
    lines.push(Line::from(cur));
    lines
}

fn spans_into(inline: &Inline, base: Style, theme: &Theme, out: &mut Vec<Span<'static>>) {
    match inline {
        Inline::Text(t) => out.push(Span::styled(t.clone(), base)),
        Inline::Strong(c) => run(c, base.add_modifier(Modifier::BOLD), theme, out),
        Inline::Emphasis(c) => run(c, base.add_modifier(Modifier::ITALIC), theme, out),
        Inline::Strike(c) => run(c, base.add_modifier(Modifier::CROSSED_OUT), theme, out),
        Inline::Underline(c) => run(c, base.add_modifier(Modifier::UNDERLINED), theme, out),
        Inline::Code(t) => out.push(Span::styled(t.clone(), theme.code_inline())),
        Inline::Link { content, .. } => run(
            content,
            base.fg(theme.accent).add_modifier(Modifier::UNDERLINED),
            theme,
            out,
        ),
        Inline::Image(img) => out.push(Span::styled(format!("🖼 {}", img.alt), theme.dim_style())),
        // A nested line break degrades to a space (top-level breaks split lines instead).
        Inline::LineBreak => out.push(Span::raw(" ")),
    }
}

fn run(inlines: &[Inline], base: Style, theme: &Theme, out: &mut Vec<Span<'static>>) {
    for inline in inlines {
        spans_into(inline, base, theme, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;
    use standard_core::model::RichDoc;

    fn flat(text: &Text) -> String {
        text.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn heading_and_styled_paragraph() {
        let theme = Theme::modern_dark();
        let doc = RichDoc {
            blocks: vec![
                Block::Heading {
                    level: 2,
                    content: vec![Inline::Text("Title".into())],
                },
                Block::Paragraph(vec![
                    Inline::Text("a ".into()),
                    Inline::Strong(vec![Inline::Text("bold".into())]),
                    Inline::Text(" word".into()),
                ]),
            ],
        };
        let text = blocks_to_text(&doc.blocks, &theme);
        assert_eq!(flat(&text), "## Title\n\na bold word");

        // The "bold" span carries the BOLD modifier.
        let para = &text.lines[2];
        let bold = para.spans.iter().find(|s| s.content == "bold").unwrap();
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn quote_gets_an_accent_bar() {
        let theme = Theme::modern_dark();
        let doc = RichDoc {
            blocks: vec![Block::Quote(vec![Block::Paragraph(vec![Inline::Text(
                "q".into(),
            )])])],
        };
        let text = blocks_to_text(&doc.blocks, &theme);
        assert!(text.lines[0].spans[0].content.starts_with('▍'));
    }

    #[test]
    fn linebreak_splits_a_paragraph() {
        let theme = Theme::modern_dark();
        let doc = RichDoc {
            blocks: vec![Block::Paragraph(vec![
                Inline::Text("one".into()),
                Inline::LineBreak,
                Inline::Text("two".into()),
            ])],
        };
        assert_eq!(flat(&blocks_to_text(&doc.blocks, &theme)), "one\ntwo");
    }

    #[test]
    fn table_renders_with_box_drawing() {
        let theme = Theme::modern_dark();
        let doc = RichDoc {
            blocks: vec![Block::Table {
                head: vec![
                    vec![Inline::Text("Name".into())],
                    vec![Inline::Text("Qty".into())],
                ],
                rows: vec![vec![
                    vec![Inline::Text("apples".into())],
                    vec![Inline::Text("3".into())],
                ]],
            }],
        };
        let out = flat(&blocks_to_text(&doc.blocks, &theme));
        // box-drawing borders + the header/cell text are present
        assert!(out.contains('┌') && out.contains('┼') && out.contains('└'));
        assert!(out.contains("Name") && out.contains("Qty") && out.contains("apples"));
    }
}
