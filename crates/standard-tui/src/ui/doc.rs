//! `RichDoc` → ratatui `Text`. Pure styling of the neutral AST; the reader pane wraps and
//! scrolls the result. (Real inline images are a later milestone — they render as a dim
//! placeholder here.)

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};

use standard_core::model::{Block, Inline};

use super::theme::Theme;

/// Render a sequence of blocks to text (used by the block-flow reader for the text runs
/// between images; takes references so a run can be built without cloning blocks).
pub fn blocks_to_text<'a>(blocks: impl IntoIterator<Item = &'a Block>, theme: &Theme) -> Text<'static> {
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
            for (i, mut line) in inline_lines(content, theme.heading(), theme).into_iter().enumerate() {
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
                let marker = if *ordered { format!("{}. ", i + 1) } else { "• ".to_string() };
                let mut inner = Vec::new();
                for b in item {
                    block_lines(b, theme, &mut inner);
                }
                for (j, mut line) in inner.into_iter().enumerate() {
                    let prefix = if j == 0 { marker.clone() } else { "  ".to_string() };
                    line.spans.insert(0, Span::styled(prefix, theme.accent_style()));
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
            let label = if img.alt.is_empty() { "(image)".to_string() } else { img.alt.clone() };
            out.push(Line::styled(format!("🖼  {label}"), theme.dim_style()));
        }
        Block::Rule => out.push(Line::styled("─".repeat(48), Style::default().fg(theme.border))),
    }
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
        Inline::Link { content, .. } => {
            run(content, base.fg(theme.accent).add_modifier(Modifier::UNDERLINED), theme, out)
        }
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
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn heading_and_styled_paragraph() {
        let theme = Theme::modern_dark();
        let doc = RichDoc {
            blocks: vec![
                Block::Heading { level: 2, content: vec![Inline::Text("Title".into())] },
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
            blocks: vec![Block::Quote(vec![Block::Paragraph(vec![Inline::Text("q".into())])])],
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
}
