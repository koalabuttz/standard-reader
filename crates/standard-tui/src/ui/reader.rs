//! The block-flow reader: text runs and images stacked vertically, scrolled by row.
//!
//! A `Paragraph` can't embed a widget, so the body is split into **segments** — runs of
//! non-image blocks (rendered as wrapped paragraphs) interleaved with images. Each
//! segment is measured (text via `Paragraph::line_count`, images from their pixel size),
//! placed at an absolute row, and the visible window is drawn at `top - scroll`.
//!
//! Images render only when *fully* in view, at a fixed (capped) size — so the protocol
//! encodes once and never re-encodes on scroll. Partially-scrolled images show a
//! placeholder bar until fully visible (smooth clipping is a later refinement).

use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Span, Text};
use ratatui::widgets::{Block, BorderType, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::{Resize, StatefulImage};

use standard_core::model::Block as DocBlock;

use super::doc;
use super::theme::Theme;
use crate::app::{image_key, App, Focus, Mode};

const GAP: u16 = 1; // blank row between segments

enum Segment {
    Text { text: Text<'static>, top: u16, height: u16 },
    Image { key: String, alt: String, top: u16, height: u16 },
}

impl Segment {
    fn top(&self) -> u16 {
        match self {
            Segment::Text { top, .. } | Segment::Image { top, .. } => *top,
        }
    }
    fn height(&self) -> u16 {
        match self {
            Segment::Text { height, .. } | Segment::Image { height, .. } => *height,
        }
    }
}

/// Draw the reader pane (bordered panel + scrolled block-flow body).
pub fn draw(f: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    let focused = app.focus == Focus::Reader && app.mode == Mode::Browse;
    let title = if app.reading_title.is_empty() { "Reader".to_string() } else { app.reading_title.clone() };
    let border = if focused { theme.accent } else { theme.border };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(format!(" {title} "), theme.heading()))
        .style(theme.base());
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.reading.is_none() {
        let msg = if app.loading {
            "loading…"
        } else {
            "Select a feed (Enter), pick a post, and it appears here."
        };
        f.render_widget(
            Paragraph::new(msg).style(theme.dim_style()).alignment(Alignment::Center),
            inner,
        );
        return;
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let (segments, total) = build(app, theme, inner.width, inner.height);
    app.scroll = app.scroll.min(total.saturating_sub(inner.height));
    let scroll = app.scroll;
    render(f, app, theme, inner, &segments, scroll);
}

/// Build measured, positioned segments for the current document (+ its cover). Borrows
/// `app` only to read; returns owned segments so rendering can borrow `app.images` freely.
fn build(app: &App, theme: &Theme, width: u16, vh: u16) -> (Vec<Segment>, u16) {
    let mut segs: Vec<Segment> = Vec::new();
    let mut y: u16 = 0;

    let push_image = |segs: &mut Vec<Segment>, y: &mut u16, key: String, alt: String| {
        let height = image_rows(app, &key, width, vh);
        if !segs.is_empty() {
            *y += GAP;
        }
        segs.push(Segment::Image { key, alt, top: *y, height });
        *y += height;
    };

    if let Some(src) = &app.reading_cover {
        push_image(&mut segs, &mut y, image_key(src), "cover".into());
    }

    if let Some(body) = &app.reading {
        let mut run: Vec<&DocBlock> = Vec::new();
        for block in &body.blocks {
            if let DocBlock::Image(img) = block {
                flush_text(&mut run, theme, width, &mut segs, &mut y);
                push_image(&mut segs, &mut y, image_key(&img.source), img.alt.clone());
            } else {
                run.push(block);
            }
        }
        flush_text(&mut run, theme, width, &mut segs, &mut y);
    }

    (segs, y)
}

fn flush_text(run: &mut Vec<&DocBlock>, theme: &Theme, width: u16, segs: &mut Vec<Segment>, y: &mut u16) {
    if run.is_empty() {
        return;
    }
    let text = doc::blocks_to_text(run.iter().copied(), theme);
    let height = Paragraph::new(text.clone()).wrap(Wrap { trim: false }).line_count(width) as u16;
    if !segs.is_empty() {
        *y += GAP;
    }
    segs.push(Segment::Text { text, top: *y, height });
    *y += height;
    run.clear();
}

/// Rows to reserve for an image: its pixel aspect mapped to cells (a cell is ~2× taller
/// than wide, hence ×0.5), clamped so it always fits the viewport.
fn image_rows(app: &App, key: &str, width: u16, vh: u16) -> u16 {
    let (iw, ih) = app.images.get(key).map(|li| (li.width, li.height)).unwrap_or((4, 3));
    let rows = (width as f32 * (ih as f32 / iw.max(1) as f32) * 0.5).round() as u16;
    rows.clamp(3, vh.saturating_sub(4).max(3))
}

fn render(f: &mut Frame, app: &mut App, theme: &Theme, inner: Rect, segments: &[Segment], scroll: u16) {
    for seg in segments {
        let (top, height) = (seg.top(), seg.height());
        let vis_top = top.max(scroll);
        let vis_bottom = (top + height).min(scroll + inner.height);
        if vis_bottom <= vis_top {
            continue; // fully off-screen
        }
        let rect = Rect {
            x: inner.x,
            y: inner.y + (vis_top - scroll),
            width: inner.width,
            height: vis_bottom - vis_top,
        };
        match seg {
            Segment::Text { text, .. } => {
                let skip = vis_top - top;
                let para = Paragraph::new(text.clone())
                    .wrap(Wrap { trim: false })
                    .scroll((skip, 0))
                    .style(theme.body());
                f.render_widget(para, rect);
            }
            Segment::Image { key, alt, .. } => {
                let fully_visible = vis_top == top && rect.height == height;
                if fully_visible
                    && let Some(loaded) = app.images.get_mut(key) {
                        f.render_stateful_widget(
                            StatefulImage::default().resize(Resize::Fit(None)),
                            rect,
                            &mut loaded.protocol,
                        );
                        continue;
                    }
                let label = if app.images.contains_key(key) {
                    format!("🖼 {alt}")
                } else {
                    format!("🖼 loading… {alt}")
                };
                f.render_widget(
                    Paragraph::new(label.trim().to_string())
                        .style(theme.dim_style())
                        .alignment(Alignment::Center),
                    rect,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::ToWorker;
    use ratatui_image::picker::Picker;
    use standard_core::model::{Block as DocBlock, Image, ImageSource, Inline, RichDoc};
    use std::sync::mpsc::channel;

    fn app_with(doc: RichDoc) -> App {
        let (tx, _rx) = channel::<ToWorker>();
        let mut app = App::new(tx, Picker::halfblocks());
        app.reading = Some(doc);
        app
    }

    #[test]
    fn segments_put_the_image_between_text() {
        let app = app_with(RichDoc {
            blocks: vec![
                DocBlock::Paragraph(vec![Inline::Text("intro".into())]),
                DocBlock::Image(Image {
                    alt: "pic".into(),
                    source: ImageSource::Url("https://i.test/a.png".into()),
                }),
                DocBlock::Paragraph(vec![Inline::Text("outro".into())]),
            ],
        });
        let theme = Theme::modern_dark();
        let (segs, total) = build(&app, &theme, 40, 40);
        assert_eq!(segs.len(), 3);
        assert!(matches!(segs[0], Segment::Text { .. }));
        assert!(matches!(segs[1], Segment::Image { .. }));
        assert!(matches!(segs[2], Segment::Text { .. }));
        // tops are strictly increasing and total covers the last segment.
        assert!(segs[0].top() < segs[1].top() && segs[1].top() < segs[2].top());
        assert!(total >= segs[2].top() + segs[2].height());
    }

    #[test]
    fn renders_a_loaded_image_without_panic() {
        use crate::app::LoadedImage;
        use ratatui::{backend::TestBackend, Terminal};

        let (tx, _rx) = channel::<ToWorker>();
        let mut app = App::new(tx, Picker::halfblocks());
        let source = ImageSource::Url("https://i.test/a.png".into());
        let key = image_key(&source);
        let protocol = app.picker.new_resize_protocol(image::DynamicImage::ImageRgba8(
            image::RgbaImage::new(4, 4),
        ));
        app.images.insert(key, LoadedImage { protocol, width: 4, height: 4 });
        app.reading = Some(RichDoc {
            blocks: vec![
                DocBlock::Paragraph(vec![Inline::Text("before".into())]),
                DocBlock::Image(Image { alt: "pic".into(), source }),
                DocBlock::Paragraph(vec![Inline::Text("after".into())]),
            ],
        });
        app.loading = false;

        let theme = Theme::modern_dark();
        let mut terminal = Terminal::new(TestBackend::new(60, 30)).unwrap();
        // The block-flow reader (incl. the StatefulImage path) must render without panicking.
        terminal.draw(|f| crate::ui::draw(f, &mut app, &theme)).unwrap();
    }
}
