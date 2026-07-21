//! The web [`ImageSink`]: native `<img>` elements overlaid on the terminal grid.
//!
//! ratzilla renders only text cells, so images are real `<img>`s positioned absolutely over the
//! terminal (the M0 design: the reader reserves blank cells + records the rect; here we composite
//! pixels on top — exactly what a terminal graphics protocol does, with the browser as compositor).
//! A clip container sized to the reader pane clips images that scroll out of view. Image bytes are
//! still decoded in Rust (the worker hands us a `DynamicImage`); we PNG-encode + hand the browser a
//! Blob object URL.

use std::collections::HashMap;
use std::io::Cursor;

use image::{DynamicImage, ImageFormat};
use ratzilla::ratatui::Frame;
use ratzilla::ratatui::layout::Rect;
use standard_frontend::image_sink::ImageSink;
use wasm_bindgen::JsCast;
use web_sys::{Blob, Document, HtmlElement, HtmlImageElement, Url};

struct Overlay {
    img: HtmlImageElement,
    object_url: String,
    /// The (cols, rows) the `<img>` is currently sized for — only restyle when it changes.
    sized: (u16, u16),
}

impl Drop for Overlay {
    fn drop(&mut self) {
        let _ = Url::revoke_object_url(&self.object_url);
        self.img.remove();
    }
}

pub struct OverlayImageSink {
    doc: Document,
    /// Clip box positioned over the reader pane each frame (overflow:hidden), so a partly-scrolled
    /// image shows only its in-pane part.
    container: HtmlElement,
    /// Viewport pixel position of terminal cell (0,0) — the grid's top-left. Read from ratzilla's
    /// real DOM each frame so overlays stay aligned even if the grid isn't at the page origin.
    origin: (f64, f64),
    /// Precise pixels per terminal cell `(w, h)` — kept as floats so the overlay aligns with
    /// ratzilla's sub-pixel cell advance even far from the origin. Measured from the live grid
    /// (a row `<pre>` is a hardcoded 15px tall in ratzilla 0.3, *not* the font line-height — so a
    /// font-probe overestimates height and images drift past the pane bottom; see `refresh_geom`).
    cell: (f64, f64),
    images: HashMap<String, Overlay>,
}

impl OverlayImageSink {
    pub fn new() -> Self {
        let window = web_sys::window().expect("no window");
        let doc = window.document().expect("no document");
        let body = doc.body().expect("no body");
        // Bootstrap before the grid exists (frame 1): a font probe gives a good *width*, and we
        // seed the height to ratzilla's known 15px row pitch. `refresh_geom` locks in the real
        // grid geometry from frame 2 on.
        let cell = (probe_char_width(&doc, &body), 15.0);

        let container: HtmlElement = doc.create_element("div").unwrap().dyn_into().unwrap();
        set(&container, "position", "absolute");
        set(&container, "overflow", "hidden");
        set(&container, "pointer-events", "none");
        set(&container, "top", "0");
        set(&container, "left", "0");
        set(&container, "z-index", "10");
        let _ = body.append_child(&container);

        Self {
            doc,
            container,
            origin: (0.0, 0.0),
            cell,
            images: HashMap::new(),
        }
    }

    /// Re-read the true cell geometry from ratzilla's live grid (`#grid` → first row `<pre>` →
    /// first cell `<span>`): the cell `<span>` gives the per-char width *and* the (0,0) origin; the
    /// row `<pre>` gives the real 15px row pitch. Cheap (two `getBoundingClientRect`s) and run each
    /// frame, so it tracks zoom/resize and corrects the font-probe bootstrap once the grid renders.
    fn refresh_geom(&mut self) {
        let Some(grid) = self.doc.get_element_by_id("grid") else {
            return;
        };
        let Some(row) = grid.first_element_child() else {
            return;
        };
        let Some(span) = row.first_element_child() else {
            return;
        };
        let sr = span.get_bounding_client_rect();
        let rr = row.get_bounding_client_rect();
        let (cw, ch) = (sr.width(), rr.height());
        if cw > 0.0 && ch > 0.0 {
            self.origin = (sr.left(), sr.top());
            self.cell = (cw, ch);
        }
    }

    /// Hide every overlay at the start of a frame; [`ImageSink::paint`] re-shows the visible ones,
    /// so an image that scrolled out (and isn't painted this frame) vanishes. The shell calls this
    /// before `ui::draw` — also a good point to re-lock the grid geometry for the frame.
    pub fn before_frame(&mut self) {
        self.refresh_geom();
        for ov in self.images.values() {
            set(&ov.img, "display", "none");
        }
    }
}

impl ImageSink for OverlayImageSink {
    fn set_overlays_visible(&mut self, visible: bool) {
        set(
            &self.container,
            "display",
            if visible { "block" } else { "none" },
        );
    }

    fn cell_size(&self) -> (u16, u16) {
        (
            self.cell.0.round().max(1.0) as u16,
            self.cell.1.round().max(1.0) as u16,
        )
    }

    fn ensure(&mut self, key: &str, image: &DynamicImage, cols: u16, rows: u16) {
        let (cw, ch) = self.cell;
        if !self.images.contains_key(key) {
            let Some(url) = encode_object_url(image) else {
                return;
            };
            let Some(img) = self
                .doc
                .create_element("img")
                .ok()
                .and_then(|e| e.dyn_into::<HtmlImageElement>().ok())
            else {
                let _ = Url::revoke_object_url(&url);
                return;
            };
            img.set_src(&url);
            set(&img, "position", "absolute");
            set(&img, "display", "none");
            set(&img, "object-fit", "contain");
            let _ = self.container.append_child(&img);
            self.images.insert(
                key.to_string(),
                Overlay {
                    img,
                    object_url: url,
                    sized: (0, 0),
                },
            );
        }
        if let Some(ov) = self.images.get_mut(key) {
            if ov.sized != (cols, rows) {
                set(&ov.img, "width", &format!("{}px", cols as f64 * cw));
                set(&ov.img, "height", &format!("{}px", rows as f64 * ch));
                ov.sized = (cols, rows);
            }
        }
    }

    fn paint(&mut self, _f: &mut Frame, key: &str, area: Rect, x: i16, y: i16) -> bool {
        if !self.images.contains_key(key) {
            return false;
        }
        let (cw, ch) = self.cell;
        let (ox, oy) = self.origin;
        // Position the clip box over the reader pane (shared by every image this frame), anchored
        // to the grid's real origin so it lines up with the text cells beneath it.
        set(
            &self.container,
            "left",
            &format!("{}px", ox + area.x as f64 * cw),
        );
        set(
            &self.container,
            "top",
            &format!("{}px", oy + area.y as f64 * ch),
        );
        set(
            &self.container,
            "width",
            &format!("{}px", area.width as f64 * cw),
        );
        set(
            &self.container,
            "height",
            &format!("{}px", area.height as f64 * ch),
        );
        // Position + show the image relative to the clip box (signed → clips when scrolled).
        if let Some(ov) = self.images.get(key) {
            set(&ov.img, "left", &format!("{}px", x as f64 * cw));
            set(&ov.img, "top", &format!("{}px", y as f64 * ch));
            set(&ov.img, "display", "block");
        }
        true
    }
}

/// Set a CSS property on an element (best-effort).
fn set(el: &HtmlElement, prop: &str, value: &str) {
    let _ = el.style().set_property(prop, value);
}

/// Bootstrap the per-char *width* from a probe `<pre>` (same CSS ratzilla's rows use), for frame 1
/// before the real grid exists. Height is seeded separately to ratzilla's 15px row pitch and the
/// whole geometry is re-locked from the live grid in [`OverlayImageSink::refresh_geom`].
fn probe_char_width(doc: &Document, body: &HtmlElement) -> f64 {
    let Some(probe) = doc
        .create_element("pre")
        .ok()
        .and_then(|e| e.dyn_into::<HtmlElement>().ok())
    else {
        return 9.6; // sane fallback (16px monospace ≈ 9.6 wide)
    };
    set(&probe, "position", "absolute");
    set(&probe, "visibility", "hidden");
    set(&probe, "margin", "0");
    set(&probe, "padding", "0");
    probe.set_text_content(Some(&"M".repeat(100)));
    let _ = body.append_child(&probe);
    let w = probe.get_bounding_client_rect().width() / 100.0;
    probe.remove();
    if w > 0.0 { w } else { 9.6 }
}

/// PNG-encode a decoded image and hand the browser a Blob object URL (the browser does the final
/// scale/draw). `None` if encoding or the blob/URL creation fails.
fn encode_object_url(image: &DynamicImage) -> Option<String> {
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .ok()?;
    let array = js_sys::Array::new();
    array.push(&js_sys::Uint8Array::from(&bytes[..]));
    // No explicit type — the browser sniffs PNG from the bytes.
    let blob = Blob::new_with_u8_array_sequence(&array).ok()?;
    Url::create_object_url_with_blob(&blob).ok()
}
