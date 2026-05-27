//! The block-flow reader: text runs and images stacked vertically, scrolled by row.
//!
//! A `Paragraph` can't embed a widget, so the body is split into **segments** — runs of
//! non-image blocks (rendered as wrapped paragraphs) interleaved with images. Each
//! segment is measured (text via `Paragraph::line_count`, images from their pixel size),
//! placed at an absolute row, and the visible window is drawn at `top - scroll`.
//!
//! Each image is sized to fit the available width (capped in height, using the terminal's
//! real font-cell metrics) and rendered centered, as soon as its top scrolls into view.

use image::DynamicImage;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect, Size};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Paragraph, Wrap};
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};

use standard_core::model::{Block as DocBlock, Image, Inline};

/// Hard ceiling on image height (rows), so a tall/portrait image never dominates the pane.
const MAX_IMAGE_ROWS: u16 = 20;

use super::doc;
use super::theme::Theme;
use crate::app::{App, Focus, Mode, image_key};

const GAP: u16 = 1; // blank row between segments

enum Segment {
    Text {
        text: Text<'static>,
        top: u16,
        height: u16,
    },
    Image {
        key: String,
        alt: String,
        top: u16,
        height: u16,
        width: u16,
    },
    /// A callout box: pre-built text (emoji already prefixed) drawn over a tinted fill.
    Callout {
        text: Text<'static>,
        tint: Option<(u8, u8, u8)>,
        top: u16,
        height: u16,
    },
    /// A grid of images laid out in columns; each cell positioned relative to the grid top.
    Grid {
        cells: Vec<GridCell>,
        top: u16,
        height: u16,
    },
}

/// One image cell within a [`Segment::Grid`]: its cache key, offset from the grid's
/// top-left (`dx`, `dy`), and rendered size (`w` × `h` cells).
struct GridCell {
    key: String,
    dx: u16,
    dy: u16,
    w: u16,
    h: u16,
}

impl Segment {
    fn top(&self) -> u16 {
        match self {
            Segment::Text { top, .. }
            | Segment::Image { top, .. }
            | Segment::Callout { top, .. }
            | Segment::Grid { top, .. } => *top,
        }
    }
    fn height(&self) -> u16 {
        match self {
            Segment::Text { height, .. }
            | Segment::Image { height, .. }
            | Segment::Callout { height, .. }
            | Segment::Grid { height, .. } => *height,
        }
    }
}

/// Draw the reader pane (bordered panel + scrolled block-flow body).
pub fn draw(f: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    let focused = app.focus == Focus::Reader && app.mode == Mode::Browse;
    let title = if app.reading_title.is_empty() {
        "Reader".to_string()
    } else {
        app.reading_title.clone()
    };
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
            Paragraph::new(msg)
                .style(theme.dim_style())
                .alignment(Alignment::Center),
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
    ensure_slices(app, &segments);
    render(f, app, theme, inner, &segments, scroll);
}

/// Build (encode) the row-sliced protocol for each image at its current display size, once
/// per size. Two passes so `&app.picker` and `&mut app.images` never overlap.
fn ensure_slices(app: &mut App, segments: &[Segment]) {
    // (key, target size) for every image that needs (re)slicing at its current display size.
    let mut wanted: Vec<(&str, Size)> = Vec::new();
    for seg in segments {
        match seg {
            Segment::Image {
                key, width, height, ..
            } => wanted.push((key, Size::new(*width, *height))),
            Segment::Grid { cells, .. } => {
                wanted.extend(cells.iter().map(|c| (c.key.as_str(), Size::new(c.w, c.h))));
            }
            _ => {}
        }
    }

    let pending: Vec<(String, DynamicImage, Size)> = wanted
        .into_iter()
        .filter_map(|(key, size)| {
            let li = app.images.get(key)?;
            (li.sliced.is_none() || li.sliced_size != (size.width, size.height))
                .then(|| (key.to_string(), li.image.clone(), size))
        })
        .collect();

    for (key, image, size) in pending {
        // Encodes once per size; `new` returns owned, releasing the picker borrow before
        // we take `&mut app.images`.
        if let Ok(sliced) = SlicedProtocol::new(&app.picker, image, Some(size))
            && let Some(li) = app.images.get_mut(&key)
        {
            li.sliced = Some(sliced);
            li.sliced_size = (size.width, size.height);
        }
    }
}

/// Build measured, positioned segments for the current document (+ its cover). Borrows
/// `app` only to read; returns owned segments so rendering can borrow `app.images` freely.
fn build(app: &App, theme: &Theme, width: u16, vh: u16) -> (Vec<Segment>, u16) {
    let mut segs: Vec<Segment> = Vec::new();
    let mut y: u16 = 0;

    let push_image = |segs: &mut Vec<Segment>, y: &mut u16, key: String, alt: String| {
        let (cols, rows) = image_display_size(app, &key, width, vh);
        if !segs.is_empty() {
            *y += GAP;
        }
        segs.push(Segment::Image {
            key,
            alt,
            top: *y,
            height: rows,
            width: cols,
        });
        *y += rows;
    };

    if app.show_images
        && let Some(src) = &app.reading_cover
    {
        push_image(&mut segs, &mut y, image_key(src), "cover".into());
    }

    if let Some(body) = &app.reading {
        let mut run: Vec<&DocBlock> = Vec::new();
        for block in &body.blocks {
            match block {
                DocBlock::Image(img) if app.show_images => {
                    flush_text(&mut run, theme, width, &mut segs, &mut y);
                    push_image(&mut segs, &mut y, image_key(&img.source), img.alt.clone());
                }
                DocBlock::Callout {
                    emoji,
                    tint,
                    content,
                } => {
                    flush_text(&mut run, theme, width, &mut segs, &mut y);
                    let (text, height) = callout_segment(content, emoji.as_deref(), theme, width);
                    if !segs.is_empty() {
                        y += GAP;
                    }
                    segs.push(Segment::Callout {
                        text,
                        tint: *tint,
                        top: y,
                        height,
                    });
                    y += height;
                }
                DocBlock::ImageGrid(images) if app.show_images => {
                    flush_text(&mut run, theme, width, &mut segs, &mut y);
                    let (cells, height) = grid_layout(app, images, width, vh);
                    if !cells.is_empty() {
                        if !segs.is_empty() {
                            y += GAP;
                        }
                        segs.push(Segment::Grid {
                            cells,
                            top: y,
                            height,
                        });
                        y += height;
                    }
                }
                _ => run.push(block),
            }
        }
        flush_text(&mut run, theme, width, &mut segs, &mut y);
    }

    (segs, y)
}

/// Build a callout's body text (emoji prefixed) and its height for the available width.
/// Leaves 1 col for the accent bar + 1 of padding each side.
fn callout_segment(
    content: &[Inline],
    emoji: Option<&str>,
    theme: &Theme,
    width: u16,
) -> (Text<'static>, u16) {
    let inner_w = width.saturating_sub(4).max(1);
    let mut text = doc::inline_paragraph(content, theme);
    if let Some(e) = emoji {
        let badge = Span::styled(format!("{e} "), theme.body());
        match text.lines.first_mut() {
            Some(first) => first.spans.insert(0, badge),
            None => text.lines.push(Line::from(badge)),
        }
    }
    let height = Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .line_count(inner_w) as u16;
    (text, height.max(1))
}

fn flush_text(
    run: &mut Vec<&DocBlock>,
    theme: &Theme,
    width: u16,
    segs: &mut Vec<Segment>,
    y: &mut u16,
) {
    if run.is_empty() {
        return;
    }
    let text = doc::blocks_to_text(run.iter().copied(), theme);
    let height = Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .line_count(width) as u16;
    if !segs.is_empty() {
        *y += GAP;
    }
    segs.push(Segment::Text {
        text,
        top: *y,
        height,
    });
    *y += height;
    run.clear();
}

/// Display size (cols, rows) for an image: scaled to fit the available width using the
/// terminal's real font-cell aspect, never upscaled past its natural size, and capped in
/// height so a tall image stays reasonable. Cell metrics come from the `Picker`.
fn image_display_size(app: &App, key: &str, avail_w: u16, vh: u16) -> (u16, u16) {
    let cap_h = vh.saturating_sub(2).clamp(1, MAX_IMAGE_ROWS) as f32;
    let avail_w = avail_w.max(1) as f32;

    let Some(loaded) = app.images.get(key) else {
        // Not decoded yet — reserve a modest placeholder slot.
        let w = avail_w.min(40.0);
        let h = (w * 0.5).min(cap_h);
        return (w as u16, h.max(1.0) as u16);
    };

    let fs = app.picker.font_size();
    let (cw, ch) = (fs.width.max(1) as f32, fs.height.max(1) as f32);
    let natural_w = (loaded.width as f32 / cw).max(1.0); // image size in cells
    let natural_h = (loaded.height as f32 / ch).max(1.0);

    // Fit within (avail_w × cap_h), preserving aspect, without upscaling beyond natural.
    let mut w = natural_w.min(avail_w);
    let mut h = w * natural_h / natural_w;
    if h > cap_h {
        h = cap_h;
        w = h * natural_w / natural_h;
    }
    (
        w.round().clamp(1.0, avail_w) as u16,
        h.round().clamp(1.0, cap_h) as u16,
    )
}

/// Column count for an image grid, chosen to keep rows balanced: among 1..=max (max set by
/// pane width), pick the count whose last row is fullest, tie-breaking toward more columns.
/// e.g. 3→3-up, 4→2+2 (not 3+1), 5→3+2, 6→3+3.
fn grid_cols(n: usize, width: u16) -> usize {
    let max = if width >= 90 {
        3
    } else if width >= 40 {
        2
    } else {
        1
    }
    .min(n)
    .max(1);
    (1..=max)
        .max_by_key(|&cols| {
            let last_fill = if n.is_multiple_of(cols) {
                cols
            } else {
                n % cols
            };
            (last_fill, cols) // higher fill wins; ties → more columns
        })
        .unwrap_or(1)
}

/// Lay images out in a balanced grid: size each cell to its column, centre each *row*
/// (so a short last row sits in the middle), and stack rows. Returns positioned cells and
/// the grid's total height.
fn grid_layout(app: &App, images: &[Image], width: u16, vh: u16) -> (Vec<GridCell>, u16) {
    const GAP_X: u16 = 1;
    const GAP_Y: u16 = 1;
    if images.is_empty() {
        return (Vec::new(), 0);
    }
    let n = images.len();
    let cols = grid_cols(n, width);
    let cell_w = (width.saturating_sub((cols as u16 - 1) * GAP_X) / cols as u16).max(1);

    // Size every image to the column width first.
    let sized: Vec<(String, u16, u16)> = images
        .iter()
        .map(|img| {
            let key = image_key(&img.source);
            let (w, h) = image_display_size(app, &key, cell_w, vh);
            (key, w, h)
        })
        .collect();

    let mut cells = Vec::with_capacity(n);
    let mut row_top = 0u16;
    for row in sized.chunks(cols) {
        let in_row = row.len() as u16;
        let row_width = in_row * cell_w + in_row.saturating_sub(1) * GAP_X;
        let row_off = width.saturating_sub(row_width) / 2; // centre the row in the pane
        let mut row_h = 0u16;
        for (k, (key, w, h)) in row.iter().enumerate() {
            let dx = row_off + k as u16 * (cell_w + GAP_X) + cell_w.saturating_sub(*w) / 2;
            cells.push(GridCell {
                key: key.clone(),
                dx,
                dy: row_top,
                w: *w,
                h: *h,
            });
            row_h = row_h.max(*h);
        }
        row_top += row_h + GAP_Y;
    }
    (cells, row_top.saturating_sub(GAP_Y))
}

fn render(f: &mut Frame, app: &App, theme: &Theme, inner: Rect, segments: &[Segment], scroll: u16) {
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
            Segment::Image {
                key,
                alt,
                width: cols,
                ..
            } => {
                // Pre-encoded slices: render the rows that fall within the reader, at a
                // signed vertical offset, so scrolling never re-encodes (no lag, no resize)
                // and a partly-visible image shows correctly whether its top or bottom is cut.
                if let Some(sliced) = app.images.get(key).and_then(|li| li.sliced.as_ref()) {
                    let x = (inner.width.saturating_sub(*cols) / 2) as i16;
                    let y =
                        (top as i32 - scroll as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                    f.render_widget(
                        SlicedImage::new(sliced, SignedPosition::from((x, y))),
                        inner,
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
            Segment::Callout { text, tint, .. } => {
                // A filled, subtly-tinted box with a full-saturation left bar — clips
                // cleanly under scroll (no borders to misplace).
                let fill = tint
                    .map(|t| blend(theme.bg, t, 0.20))
                    .unwrap_or(theme.panel);
                let bar = tint
                    .map(|(r, g, b)| Color::Rgb(r, g, b))
                    .unwrap_or(theme.accent2);
                f.render_widget(Block::new().style(Style::default().bg(fill)), rect);
                f.render_widget(
                    Block::new().style(Style::default().bg(bar)),
                    Rect { width: 1, ..rect },
                );
                let text_rect = Rect {
                    x: rect.x + 2,
                    y: rect.y,
                    width: rect.width.saturating_sub(3),
                    height: rect.height,
                };
                let skip = vis_top - top;
                let para = Paragraph::new(text.clone())
                    .wrap(Wrap { trim: false })
                    .scroll((skip, 0))
                    .style(Style::default().fg(theme.fg).bg(fill));
                f.render_widget(para, text_rect);
            }
            Segment::Grid { cells, .. } => {
                // Each cell is a pre-sliced image positioned at its (dx, dy) within the grid,
                // offset by scroll; SlicedImage clips each to the reader.
                for cell in cells {
                    if let Some(sliced) =
                        app.images.get(&cell.key).and_then(|li| li.sliced.as_ref())
                    {
                        let x = cell.dx as i16;
                        let y = ((top + cell.dy) as i32 - scroll as i32)
                            .clamp(i16::MIN as i32, i16::MAX as i32)
                            as i16;
                        f.render_widget(
                            SlicedImage::new(sliced, SignedPosition::from((x, y))),
                            inner,
                        );
                    }
                }
            }
        }
    }
}

/// Blend a `(r,g,b)` tint over a base colour at the given opacity.
fn blend(base: Color, tint: (u8, u8, u8), alpha: f32) -> Color {
    let (br, bg, bb) = rgb_of(base);
    let mix = |b: u8, t: u8| (b as f32 * (1.0 - alpha) + t as f32 * alpha).round() as u8;
    Color::Rgb(mix(br, tint.0), mix(bg, tint.1), mix(bb, tint.2))
}

fn rgb_of(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::ToWorker;
    use ratatui_image::picker::Picker;
    use standard_core::model::{Block as DocBlock, Image, ImageSource, Inline, RichDoc};
    use std::sync::mpsc::channel;

    #[test]
    fn grid_columns_balance_rows() {
        // Wide pane (max 3 cols): rows stay as even as possible, 3-up kept where it fits.
        assert_eq!(grid_cols(1, 100), 1);
        assert_eq!(grid_cols(2, 100), 2);
        assert_eq!(grid_cols(3, 100), 3);
        assert_eq!(grid_cols(4, 100), 2); // 2+2, not 3+1
        assert_eq!(grid_cols(5, 100), 3); // 3+2
        assert_eq!(grid_cols(6, 100), 3); // 3+3
        // Narrow pane caps at 1 column.
        assert_eq!(grid_cols(4, 30), 1);
    }

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
        use ratatui::{Terminal, backend::TestBackend};

        let (tx, _rx) = channel::<ToWorker>();
        let mut app = App::new(tx, Picker::halfblocks());
        let source = ImageSource::Url("https://i.test/a.png".into());
        let key = image_key(&source);
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4));
        app.images.insert(
            key,
            LoadedImage {
                image,
                width: 4,
                height: 4,
                sliced: None,
                sliced_size: (0, 0),
            },
        );
        app.reading = Some(RichDoc {
            blocks: vec![
                DocBlock::Paragraph(vec![Inline::Text("before".into())]),
                DocBlock::Image(Image {
                    alt: "pic".into(),
                    source,
                }),
                DocBlock::Paragraph(vec![Inline::Text("after".into())]),
            ],
        });
        app.loading = false;

        let theme = Theme::modern_dark();
        let mut terminal = Terminal::new(TestBackend::new(60, 30)).unwrap();
        // The block-flow reader (incl. the StatefulImage path) must render without panicking.
        terminal
            .draw(|f| crate::ui::draw(f, &mut app, &theme))
            .unwrap();
    }
}
