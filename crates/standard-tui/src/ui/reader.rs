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
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Alignment, Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Paragraph, Widget, Wrap};
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};

use standard_core::model::{Block as DocBlock, Image, Inline};

use crate::app::LinkRect;

/// Hard ceiling on image height (rows), so a tall/portrait image never dominates the pane.
const MAX_IMAGE_ROWS: u16 = 20;

use super::doc;
use super::theme::Theme;
use crate::app::{App, Focus, image_key};

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
        /// Left columns reserved for container framing (quote bar / list indent); 0 = full-width.
        indent: u16,
        /// Draw a quote bar in the reserved gutter (for images nested in a blockquote).
        bar: bool,
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

/// Cached reader layout, reused across draws when nothing affecting it changed. `build()` + the
/// per-line buffer-render link scan are expensive and scroll-independent, so caching them makes
/// scrolling and (especially) sidebar navigation cheap. Stored on [`App`] as an opaque handle.
pub(crate) struct ReaderLayout {
    key: ReaderKey,
    segments: Vec<Segment>,
    total: u16,
}

/// Everything that determines the laid-out segments + link rects. Excludes `scroll` (applied
/// per-frame in `render`); includes `focused_link` so `n`/`N` simply recompute (rare) rather than
/// us splitting the focus-highlight restyle out of the rect scan.
#[derive(PartialEq)]
struct ReaderKey {
    width: u16,
    height: u16,
    show_images: bool,
    reading_version: u64,
    images_version: u64,
    theme: Theme,
    focused_link: Option<usize>,
}

/// Draw the reader pane (bordered panel + scrolled block-flow body).
pub fn draw(f: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    let focused = app.focus == Focus::Reader;
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

    // Reuse the cached layout when nothing relevant changed (scroll/sidebar-nav are cache hits);
    // otherwise rebuild — the expensive `build` + per-line buffer-render link scan. Taken out of
    // `app` so the rest of the draw can still hold `&mut app`; `link_rects` (set on a miss) stays
    // valid across hits because the key covers everything that affects it.
    let key = ReaderKey {
        width: inner.width,
        height: inner.height,
        show_images: app.show_images,
        reading_version: app.reading_version,
        images_version: app.images_version,
        theme: *theme,
        focused_link: app.focused_link,
    };
    let layout = match app.reader_cache.take() {
        Some(cached) if cached.key == key => cached,
        _ => {
            let (mut segments, total) = build(app, theme, inner.width, inner.height);
            app.link_rects =
                locate_and_highlight_links(&mut segments, app.focused_link, theme, inner.width);
            ReaderLayout {
                key,
                segments,
                total,
            }
        }
    };
    let total = layout.total;
    // Bring the focused link into view if a keyboard focus change asked for it.
    if app.scroll_to_focused {
        if let Some(fi) = app.focused_link
            && let Some(r) = app.link_rects.iter().find(|r| r.idx == fi)
        {
            if r.row < app.scroll {
                app.scroll = r.row;
            } else if r.row >= app.scroll + inner.height {
                app.scroll = r.row.saturating_sub(inner.height / 2);
            }
        }
        app.scroll_to_focused = false;
    }
    app.scroll = app.scroll.min(total.saturating_sub(inner.height));
    let scroll = app.scroll;
    ensure_slices(app, &layout.segments);
    render(f, app, theme, inner, &layout.segments, scroll);
    app.reader_cache = Some(layout); // put the (reused or freshly built) layout back
}

/// Find each hyperlink's on-screen rectangle(s) (virtual doc coordinates) by scanning the built
/// segments for link-styled spans — and restyle the focused link so it stands out. Link order
/// matches [`crate::app::collect_links`] (document order; tables excluded). A link that wraps
/// across rows emits one rect per row (all sharing its `idx`), so a click on the wrapped tail
/// still maps to the link. Positions are taken from a temp-buffer render of each line, so they
/// reflect ratatui's actual word-wrap (not a char-wrap approximation) — exact for every link,
/// including ones whose visible text wraps across rows.
fn locate_and_highlight_links(
    segments: &mut [Segment],
    focused: Option<usize>,
    theme: &Theme,
    width: u16,
) -> Vec<LinkRect> {
    let mut rects = Vec::new();
    let mut idx = 0usize;
    for seg in segments.iter_mut() {
        match seg {
            Segment::Text { text, top, .. } => {
                scan_text_links(text, *top, width, 0, theme, focused, &mut idx, &mut rects);
            }
            Segment::Callout { text, top, .. } => {
                // Callout text renders inset by the bar + a pad column, wrapped narrower.
                let w = width.saturating_sub(3).max(1);
                scan_text_links(text, *top, w, 2, theme, focused, &mut idx, &mut rects);
            }
            _ => {}
        }
    }
    rects
}

/// Scan one segment's wrapped text for runs of link-styled spans, recording each link's rect and
/// brightening the focused one. `col_off` shifts columns for inset segments (callouts).
///
/// Two passes per logical line:
/// 1. **Span walk** identifies link occurrences (consecutive link-styled spans) and applies the
///    focus highlight in-place to the focused one's spans, so the *actual* reader render shows it.
/// 2. **Cell scan** renders the (now possibly highlight-mutated) line to a temp buffer and reads
///    the cells back, producing rects at the exact cells the link occupies after ratatui's word-
///    wrap — including wrap continuations. A multi-row link contributes one rect per row, all
///    sharing its idx, so a click on any visible part resolves to the link.
#[allow(clippy::too_many_arguments)]
fn scan_text_links(
    text: &mut Text<'static>,
    seg_top: u16,
    width: u16,
    col_off: u16,
    theme: &Theme,
    focused: Option<usize>,
    idx: &mut usize,
    rects: &mut Vec<LinkRect>,
) {
    let w = width.max(1);
    let mut vrow = seg_top;
    for line in text.lines.iter_mut() {
        let line_rows = Paragraph::new(line.clone())
            .wrap(Wrap { trim: false })
            .line_count(w) as u16;
        if line_rows == 0 {
            continue;
        }

        // Pass 1: walk spans to apply the focus highlight to the focused occurrence.
        // The span walk's occurrence count matches the cell scan's in the common case
        // (each Inline::Link → one contiguous run of link-styled spans → one contiguous
        // cell run after render). Both walk in document order so their idx agrees.
        let mut span_occ: usize = 0;
        let mut i = 0;
        while i < line.spans.len() {
            if !is_link_span(&line.spans[i], theme) {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < line.spans.len() && is_link_span(&line.spans[j], theme) {
                j += 1;
            }
            if focused == Some(*idx + span_occ) {
                for s in &mut line.spans[i..j] {
                    s.style = s
                        .style
                        .remove_modifier(Modifier::UNDERLINED)
                        .add_modifier(Modifier::REVERSED | Modifier::BOLD);
                }
            }
            span_occ += 1;
            i = j;
        }

        // Pass 2: render this line to a private buffer and scan the resulting cells.
        // This gives the *true* word-wrap positions of every link glyph, instead of
        // approximating where they should land.
        let area = Rect::new(0, 0, w, line_rows);
        let mut buf = Buffer::empty(area);
        Paragraph::new(line.clone())
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);

        // State machine. `in_run = Some((idx, start_col))` means we're inside a link
        // occurrence. A run that ends on the trailing edge of a row (link is the last
        // visible content, ratatui left the rest of the row blank) is held open into
        // the next row's first cell to merge wrap continuations under one idx. Cells
        // ratatui never wrote to (trailing whitespace after wrap) are skipped so they
        // don't break the run.
        let mut in_run: Option<(usize, u16)> = None;
        let mut cell_occ: usize = 0;
        for r in 0..line_rows {
            // Reconcile state from the previous row.
            if r > 0 && in_run.is_some() {
                if is_link_cell(&buf[(0, r)], theme) {
                    // Wrap continuation: re-anchor at col 0 of this row.
                    if let Some((occ, _)) = in_run {
                        in_run = Some((occ, 0));
                    }
                } else {
                    // The run from the previous row didn't continue.
                    in_run = None;
                    cell_occ += 1;
                }
            }

            // Find the last *touched* cell on this row, so trailing empty cells
            // (whitespace ratatui left blank after wrap) don't close a run prematurely.
            let mut row_end = w;
            while row_end > 0 && is_untouched_cell(&buf[(row_end - 1, r)]) {
                row_end -= 1;
            }

            for c in 0..row_end {
                let is_link = is_link_cell(&buf[(c, r)], theme);
                if is_link {
                    if in_run.is_none() {
                        in_run = Some((*idx + cell_occ, c));
                    }
                } else if let Some((occ, start)) = in_run.take() {
                    rects.push(LinkRect {
                        idx: occ,
                        row: vrow + r,
                        col: start + col_off,
                        width: c - start,
                    });
                    cell_occ += 1;
                }
            }
            if let Some((occ, start)) = in_run {
                // Row ended mid-run: emit this row's slice (clipped to the last touched
                // cell) and keep `in_run` alive so the next row can extend the run.
                rects.push(LinkRect {
                    idx: occ,
                    row: vrow + r,
                    col: start + col_off,
                    width: row_end - start,
                });
            }
        }
        if in_run.is_some() {
            cell_occ += 1;
        }

        // Advance the outer counter by the larger of the two walks — so an empty/invisible
        // link (counted by span walk but not by cell scan, or vice versa) doesn't desync
        // downstream lines' link indices.
        *idx += span_occ.max(cell_occ);
        vrow += line_rows;
    }
}

/// A link-styled span: the reader's link colour (accent) plus underline (normal) or reverse
/// (already focused). Distinct from headings (bold, no underline) and inline code (accent2).
fn is_link_span(span: &Span, theme: &Theme) -> bool {
    span.style.fg == Some(theme.accent)
        && (span.style.add_modifier.contains(Modifier::UNDERLINED)
            || span.style.add_modifier.contains(Modifier::REVERSED))
}

/// Same idea as `is_link_span`, but for a rendered buffer cell.
fn is_link_cell(cell: &Cell, theme: &Theme) -> bool {
    cell.fg == theme.accent
        && (cell.modifier.contains(Modifier::UNDERLINED)
            || cell.modifier.contains(Modifier::REVERSED))
}

/// A cell ratatui's paragraph render never wrote to — the trailing whitespace at the right
/// edge of a wrapped row. Distinguishes "link was the last visible content on this row, the
/// rest is blank" (where the run should be held open across the row boundary) from "link
/// ended at a real non-link span" (where it's actually over).
fn is_untouched_cell(cell: &Cell) -> bool {
    cell.fg == Color::Reset && cell.bg == Color::Reset && cell.modifier.is_empty()
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

    // `indent` reserves left columns for container framing; `bar` draws a quote bar there.
    // `gap` adds the usual blank row before the image (suppressed to keep a framed image flush
    // against the quote text above it).
    let push_image = |segs: &mut Vec<Segment>,
                      y: &mut u16,
                      key: String,
                      alt: String,
                      indent: u16,
                      bar: bool,
                      gap: bool| {
        let (cols, rows) = image_display_size(app, &key, width.saturating_sub(indent), vh);
        if gap && !segs.is_empty() {
            *y += GAP;
        }
        segs.push(Segment::Image {
            key,
            alt,
            top: *y,
            height: rows,
            width: cols,
            indent,
            bar,
        });
        *y += rows;
    };

    if app.show_images
        && let Some(src) = &app.reading_cover
    {
        push_image(
            &mut segs,
            &mut y,
            image_key(src),
            "cover".into(),
            0,
            false,
            true,
        );
    }

    if let Some(body) = &app.reading {
        let mut run: Vec<&DocBlock> = Vec::new();
        for block in &body.blocks {
            match block {
                DocBlock::Image(img) if app.show_images => {
                    flush_text(&mut run, theme, width, &mut segs, &mut y);
                    push_image(
                        &mut segs,
                        &mut y,
                        image_key(&img.source),
                        img.alt.clone(),
                        0,
                        false,
                        true,
                    );
                }
                // A quote/list with image(s) nested in it: render the de-imaged container as text
                // (its bar/markers via doc.rs), then the images framed in place — so a photo
                // lazy-continued into a `> quote` shows inside the quote, matching the source.
                DocBlock::Quote(_) | DocBlock::List { .. }
                    if app.show_images && contains_image(block) =>
                {
                    flush_text(&mut run, theme, width, &mut segs, &mut y);
                    let is_quote = matches!(block, DocBlock::Quote(_));
                    let (stripped, images) = strip_container_images(block.clone());
                    if container_has_text(&stripped) {
                        let text = doc::blocks_to_text(std::iter::once(&stripped), theme);
                        let height = Paragraph::new(text.clone())
                            .wrap(Wrap { trim: false })
                            .line_count(width) as u16;
                        if !segs.is_empty() {
                            y += GAP;
                        }
                        segs.push(Segment::Text {
                            text,
                            top: y,
                            height,
                        });
                        y += height;
                    }
                    for img in images {
                        push_image(
                            &mut segs,
                            &mut y,
                            image_key(&img.source),
                            img.alt,
                            2, // align under the `▍ ` quote bar / list marker
                            is_quote,
                            false,
                        );
                    }
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
                DocBlock::Rule => {
                    // A full-width divider (doc.rs renders a short fixed rule only for the rare
                    // nested case, since it has no width to span).
                    flush_text(&mut run, theme, width, &mut segs, &mut y);
                    if !segs.is_empty() {
                        y += GAP;
                    }
                    let line = Line::styled(
                        "─".repeat(width as usize),
                        Style::default().fg(theme.border),
                    );
                    segs.push(Segment::Text {
                        text: Text::from(line),
                        top: y,
                        height: 1,
                    });
                    y += 1;
                }
                _ => run.push(block),
            }
        }
        flush_text(&mut run, theme, width, &mut segs, &mut y);
    }

    (segs, y)
}

/// Whether a block contains an image anywhere (recursing into quotes/lists + inline content).
fn contains_image(block: &DocBlock) -> bool {
    match block {
        DocBlock::Image(_) | DocBlock::ImageGrid(_) => true,
        DocBlock::Paragraph(c) | DocBlock::Heading { content: c, .. } => {
            c.iter().any(|i| matches!(i, Inline::Image(_)))
        }
        DocBlock::Quote(blocks) => blocks.iter().any(contains_image),
        DocBlock::List { items, .. } => items.iter().flatten().any(contains_image),
        _ => false,
    }
}

/// Remove every image from a container block (for layout), returning the de-imaged block plus
/// the images in document order. Used to render a quote/list's text and its images separately.
fn strip_container_images(block: DocBlock) -> (DocBlock, Vec<Image>) {
    let mut images = Vec::new();
    let stripped = strip_block(block, &mut images);
    (stripped, images)
}

fn strip_block(block: DocBlock, images: &mut Vec<Image>) -> DocBlock {
    fn drain(inlines: Vec<Inline>, images: &mut Vec<Image>) -> Vec<Inline> {
        inlines
            .into_iter()
            .filter_map(|i| match i {
                Inline::Image(img) => {
                    images.push(img);
                    None
                }
                other => Some(other),
            })
            .collect()
    }
    match block {
        DocBlock::Paragraph(inlines) => DocBlock::Paragraph(drain(inlines, images)),
        DocBlock::Heading { level, content } => DocBlock::Heading {
            level,
            content: drain(content, images),
        },
        DocBlock::Quote(blocks) => {
            DocBlock::Quote(blocks.into_iter().map(|b| strip_block(b, images)).collect())
        }
        DocBlock::List { ordered, items } => DocBlock::List {
            ordered,
            items: items
                .into_iter()
                .map(|item| item.into_iter().map(|b| strip_block(b, images)).collect())
                .collect(),
        },
        DocBlock::Image(img) => {
            images.push(img);
            DocBlock::Paragraph(Vec::new())
        }
        DocBlock::ImageGrid(grid) => {
            images.extend(grid);
            DocBlock::Paragraph(Vec::new())
        }
        other => other,
    }
}

/// Whether a de-imaged container still has text worth rendering (else only its images remain).
fn container_has_text(block: &DocBlock) -> bool {
    match block {
        DocBlock::Paragraph(c) | DocBlock::Heading { content: c, .. } => c
            .iter()
            .any(|i| !matches!(i, Inline::Text(t) if t.trim().is_empty())),
        DocBlock::Quote(blocks) => blocks.iter().any(container_has_text),
        DocBlock::List { items, .. } => items.iter().flatten().any(container_has_text),
        _ => true,
    }
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
        // Pack the row's images edge-to-edge (one-column gutter) and centre the whole row,
        // so height-capped images sit side by side instead of floating in wide cells.
        let row_width: u16 = row.iter().map(|(_, w, _)| *w).sum::<u16>()
            + (row.len() as u16).saturating_sub(1) * GAP_X;
        let mut x = width.saturating_sub(row_width) / 2;
        let mut row_h = 0u16;
        for (key, w, h) in row {
            cells.push(GridCell {
                key: key.clone(),
                dx: x,
                dy: row_top,
                w: *w,
                h: *h,
            });
            x += w + GAP_X;
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
                indent,
                bar,
                ..
            } => {
                // Quote framing: a thin bar down the reserved gutter (matches doc.rs's `▍`).
                if *bar {
                    let bar_col = Paragraph::new(
                        (0..rect.height)
                            .map(|_| Line::styled("▍", Style::default().fg(theme.accent2)))
                            .collect::<Vec<_>>(),
                    );
                    f.render_widget(bar_col, Rect { width: 1, ..rect });
                }
                // Pre-encoded slices: render the rows that fall within the reader, at a
                // signed vertical offset, so scrolling never re-encodes (no lag, no resize)
                // and a partly-visible image shows correctly whether its top or bottom is cut.
                if let Some(sliced) = app.images.get(key).and_then(|li| li.sliced.as_ref()) {
                    // Framed images sit left-aligned after the gutter; standalone ones center.
                    let x = if *indent > 0 {
                        *indent as i16
                    } else {
                        (inner.width.saturating_sub(*cols) / 2) as i16
                    };
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
                let (prect, align) = if *indent > 0 {
                    (
                        Rect {
                            x: rect.x + *indent,
                            width: rect.width.saturating_sub(*indent),
                            ..rect
                        },
                        Alignment::Left,
                    )
                } else {
                    (rect, Alignment::Center)
                };
                f.render_widget(
                    Paragraph::new(label.trim().to_string())
                        .style(theme.dim_style())
                        .alignment(align),
                    prect,
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

    #[test]
    fn reader_cache_key_hits_on_scroll_and_misses_on_change() {
        // The key deliberately excludes `scroll` (applied per-frame in `render`), so scrolling and
        // sidebar navigation reuse the cached layout; anything that affects the laid-out segments
        // or link rects flips the key, forcing a rebuild. (`==`/`!=` so we needn't derive Debug.)
        let base = || ReaderKey {
            width: 80,
            height: 24,
            show_images: true,
            reading_version: 3,
            images_version: 1,
            theme: Theme::modern_dark(),
            focused_link: None,
        };
        assert!(base() == base(), "identical inputs → cache hit");
        assert!(
            base()
                != ReaderKey {
                    width: 100,
                    ..base()
                },
            "pane resize is a miss"
        );
        assert!(
            base()
                != ReaderKey {
                    reading_version: 4,
                    ..base()
                },
            "a new post body is a miss"
        );
        assert!(
            base()
                != ReaderKey {
                    images_version: 2,
                    ..base()
                },
            "an arriving image reflows (miss)"
        );
        assert!(
            base()
                != ReaderKey {
                    focused_link: Some(0),
                    ..base()
                },
            "moving link focus is a miss"
        );
        assert!(
            base()
                != ReaderKey {
                    theme: Theme::from(&crate::ui::theme::ThemeColors::light()),
                    ..base()
                },
            "a theme change is a miss"
        );
    }

    fn app_with(doc: RichDoc) -> App {
        let (tx, _rx) = channel::<ToWorker>();
        let mut app = App::new(tx, Picker::halfblocks(), crate::prefs::Prefs::default());
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
    fn quote_nested_image_renders_framed_in_place() {
        // An image lazy-continued into a `> Quote`: the quote text renders, then the image as a
        // framed segment (quote bar + indent) — instead of being hoisted outside the quote.
        let app = app_with(RichDoc {
            blocks: vec![DocBlock::Quote(vec![DocBlock::Paragraph(vec![
                Inline::Text("Quote".into()),
                Inline::Image(Image {
                    alt: "shot".into(),
                    source: ImageSource::Url("https://i.test/a.avif".into()),
                }),
            ])])],
        });
        let theme = Theme::modern_dark();
        let (segs, _) = build(&app, &theme, 80, 40);

        assert!(
            matches!(segs.first(), Some(Segment::Text { .. })),
            "the quote's text comes first"
        );
        let framed = segs.iter().find_map(|s| match s {
            Segment::Image { bar, indent, .. } => Some((*bar, *indent)),
            _ => None,
        });
        assert_eq!(
            framed,
            Some((true, 2)),
            "the quote's image renders framed (bar + indent), in place"
        );
    }

    #[test]
    fn renders_a_loaded_image_without_panic() {
        use crate::app::LoadedImage;
        use ratatui::{Terminal, backend::TestBackend};

        let (tx, _rx) = channel::<ToWorker>();
        let mut app = App::new(tx, Picker::halfblocks(), crate::prefs::Prefs::for_test());
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

        let mut terminal = Terminal::new(TestBackend::new(60, 30)).unwrap();
        // The block-flow reader (incl. the StatefulImage path) must render without panicking.
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    }

    #[test]
    fn scan_locates_and_highlights_a_link() {
        let theme = Theme::modern_dark();
        let link_style = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::UNDERLINED);
        let mut text = Text::from(Line::from(vec![
            Span::styled("see ", theme.body()),
            Span::styled("here", link_style),
        ]));
        let mut idx = 0;
        let mut rects = Vec::new();
        scan_text_links(&mut text, 0, 80, 0, &theme, Some(0), &mut idx, &mut rects);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].col, 4, "starts after 'see '");
        assert_eq!(rects[0].width, 4, "'here' is 4 cols");
        // The focused link is reverse-highlighted.
        let here = text.lines[0]
            .spans
            .iter()
            .find(|s| s.content == "here")
            .unwrap();
        assert!(here.style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn scan_splits_a_wrapping_link_across_rows() {
        // A link wider than the pane should produce one rect per wrapped row, all sharing
        // the same `idx` — so a click on the tail piece still resolves to the link.
        let theme = Theme::modern_dark();
        let link_style = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::UNDERLINED);
        let mut text = Text::from(Line::from(vec![Span::styled("abcdefghijkl", link_style)]));
        let mut idx = 0;
        let mut rects = Vec::new();
        // width=8, 12-col link → row 0 cols 0..8, row 1 cols 0..4.
        scan_text_links(&mut text, 5, 8, 0, &theme, None, &mut idx, &mut rects);
        assert_eq!(rects.len(), 2, "one rect per wrapped row");
        assert!(rects.iter().all(|r| r.idx == 0), "same link idx for both");
        assert_eq!((rects[0].row, rects[0].col, rects[0].width), (5, 0, 8));
        assert_eq!((rects[1].row, rects[1].col, rects[1].width), (6, 0, 4));
        assert_eq!(idx, 1, "still counts as one link");
    }

    #[test]
    fn scan_pushes_an_unbreakable_link_to_the_next_row_on_word_wrap() {
        // Line: `▍ Visit davidlewis.xyz` — 22 cells flowed, width 20 forces a wrap.
        // Word-wrap (vs. char-wrap) pushes the whole link "davidlewis.xyz" to row 1
        // because it's one unbreakable word; the rect must follow the actual render so
        // the link text itself is clickable (not just the gap before it).
        let theme = Theme::modern_dark();
        let link_style = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::UNDERLINED);
        let mut text = Text::from(Line::from(vec![
            Span::styled("▍ ", theme.body()),
            Span::styled("Visit ", theme.body()),
            Span::styled("davidlewis.xyz", link_style),
        ]));
        let mut idx = 0;
        let mut rects = Vec::new();
        scan_text_links(&mut text, 0, 20, 0, &theme, None, &mut idx, &mut rects);
        assert_eq!(
            rects.len(),
            1,
            "unbreakable link sits on one row after wrap"
        );
        assert_eq!(rects[0].row, 1, "pushed to the wrapped row");
        assert_eq!(rects[0].col, 0, "starts at column 0 on the wrapped row");
        assert_eq!(rects[0].width, 14, "covers the full link, not a sliver");
    }

    #[test]
    fn every_link_cell_is_covered_by_a_rect_after_word_wrap() {
        // Regression for the wrapped-tail bug: in the user's doc, a multi-word link
        // ("on Threads here") had its last word wrap to its own row, but the char-wrap
        // math placed the row-N rect at a column where word-wrap had nothing, leaving
        // the visible "here" text unclickable. With the buffer-scan implementation,
        // every link-styled cell in the rendered output must be covered by some rect,
        // and every rect must point at link cells (no phantom rects).
        let theme = Theme::modern_dark();
        let link_style = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::UNDERLINED);
        let mut text = Text::from(Line::from(vec![
            Span::styled("posted ", theme.body()),
            Span::styled("X", link_style),
            Span::styled(" and ", theme.body()),
            Span::styled("on Threads here", link_style),
            Span::styled(".", theme.body()),
        ]));
        let w: u16 = 12; // narrow enough that "here." wraps onto its own row
        let line_rows = Paragraph::new(text.lines[0].clone())
            .wrap(Wrap { trim: false })
            .line_count(w) as u16;
        let mut idx = 0;
        let mut rects = Vec::new();
        scan_text_links(&mut text, 0, w, 0, &theme, None, &mut idx, &mut rects);

        let area = Rect::new(0, 0, w, line_rows);
        let mut buf = Buffer::empty(area);
        Paragraph::new(text.clone())
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);

        // Every visible link cell is inside at least one rect (no orphan link cells).
        for r in 0..line_rows {
            for c in 0..w {
                if is_link_cell(&buf[(c, r)], &theme) {
                    let covered = rects
                        .iter()
                        .any(|lr| lr.row == r && c >= lr.col && c < lr.col + lr.width);
                    assert!(covered, "link cell at ({c}, {r}) not covered by any rect");
                }
            }
        }
        // Every rect covers only link cells (no phantom rects in empty/non-link space).
        for lr in &rects {
            for dc in 0..lr.width {
                assert!(
                    is_link_cell(&buf[(lr.col + dc, lr.row)], &theme),
                    "rect cell at ({}, {}) is not link-styled",
                    lr.col + dc,
                    lr.row
                );
            }
        }
        assert_eq!(idx, 2, "two link occurrences (X and 'on Threads here')");
    }

    #[test]
    fn reader_records_a_link_rect() {
        use ratatui::{Terminal, backend::TestBackend};
        let (tx, _rx) = channel::<ToWorker>();
        let mut app = App::new(tx, Picker::halfblocks(), crate::prefs::Prefs::for_test());
        app.reading = Some(RichDoc {
            blocks: vec![DocBlock::Paragraph(vec![
                Inline::Text("go ".into()),
                Inline::Link {
                    href: "https://example.com".into(),
                    content: vec![Inline::Text("there".into())],
                },
            ])],
        });
        app.links = vec!["https://example.com".into()];
        app.loading = false;
        let mut terminal = Terminal::new(TestBackend::new(60, 30)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert_eq!(app.link_rects.len(), 1, "one link rect recorded");
        assert_eq!(app.link_rects[0].idx, 0);
    }
}
