//! The image-render seam: how the reader paints an image into a reserved cell rect.
//!
//! The block-flow reader is image-protocol-agnostic — it sizes images during layout using the
//! sink's cell metrics, then asks the sink to paint. The desktop sink ([`crate::terminal_image_sink::TerminalImageSink`])
//! builds row-sliced terminal-graphics protocols; a future web frontend records each image's
//! rect for a native browser (DOM/canvas) overlay. This trait deliberately carries **no
//! `ratatui-image` types** (only `image::DynamicImage` + `ratatui::{Frame, Rect}`), so it moves
//! cleanly into the platform-agnostic `standard-frontend` crate (Milestone 0).

use image::DynamicImage;
use ratatui::Frame;
use ratatui::layout::Rect;

/// The frontend's image painter. The per-image encode cache lives in the sink, not in `App`
/// (which holds only the decoded pixels) — so the platform-specific protocol/overlay state
/// stays out of the shared state machine.
pub trait ImageSink {
    /// Show or hide the shell's native image layer for this frame. Browser images live in DOM
    /// elements above the text grid, so modal dialogs cannot cover them with terminal cells alone.
    /// Shells whose images participate directly in the terminal buffer may leave the default
    /// implementation in place.
    fn set_overlays_visible(&mut self, _visible: bool) {}

    /// Pixels per terminal cell `(width, height)`, used to convert an image's pixel size to a
    /// cell size during layout. Desktop returns the `Picker`'s detected font size.
    fn cell_size(&self) -> (u16, u16);

    /// Prepare `key`'s image for painting at `cols`×`rows` cells. Idempotent and cheap on a hit
    /// (same key + same size already prepared). Called once per visible image per frame, before
    /// any paint. Desktop (re)builds + caches the row-sliced protocol; a web sink would prepare
    /// the overlay element.
    fn ensure(&mut self, key: &str, image: &DynamicImage, cols: u16, rows: u16);

    /// Paint `key` into `area`, with the image's top-left at signed cell offset `(x, y)` relative
    /// to `area` (so a partly-scrolled image clips correctly). Returns `true` if it painted,
    /// `false` if nothing was ready — in which case the reader draws its text placeholder.
    fn paint(&mut self, f: &mut Frame, key: &str, area: Rect, x: i16, y: i16) -> bool;
}

/// A do-nothing sink for tests (and any frontend that renders no images): finite cell metrics,
/// no encode, and `paint` always reports "nothing painted" so the reader shows text placeholders.
#[cfg(test)]
pub struct NoopImageSink;

#[cfg(test)]
impl ImageSink for NoopImageSink {
    fn cell_size(&self) -> (u16, u16) {
        (8, 16) // a typical terminal cell, so image sizing stays finite
    }
    fn ensure(&mut self, _key: &str, _image: &DynamicImage, _cols: u16, _rows: u16) {}
    fn paint(&mut self, _f: &mut Frame, _key: &str, _area: Rect, _x: i16, _y: i16) -> bool {
        false
    }
}
