//! The desktop [`ImageSink`]: `ratatui-image` row-sliced terminal graphics.
//!
//! This is the only place `ratatui-image` survives the Milestone-0 refactor. It owns the
//! `Picker` (terminal graphics-protocol + font-size detection) and a per-image cache of encoded
//! row slices, keyed by `image_key`, rebuilt only when an image's display size changes — so
//! scrolling never re-encodes. The encode/paint logic moved here verbatim from the reader.

use std::collections::HashMap;

use image::DynamicImage;
use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui_image::picker::Picker;
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};

use standard_frontend::image_sink::ImageSink;

pub struct TerminalImageSink {
    picker: Picker,
    /// Per-image row-sliced protocol + the `(cols, rows)` it was built for, keyed by `image_key`.
    slices: HashMap<String, (SlicedProtocol, (u16, u16))>,
}

impl TerminalImageSink {
    pub fn new(picker: Picker) -> Self {
        Self {
            picker,
            slices: HashMap::new(),
        }
    }
}

impl ImageSink for TerminalImageSink {
    fn cell_size(&self) -> (u16, u16) {
        let fs = self.picker.font_size();
        (fs.width, fs.height)
    }

    fn ensure(&mut self, key: &str, image: &DynamicImage, cols: u16, rows: u16) {
        // Cache hit: already sliced at this exact size → nothing to do.
        if self
            .slices
            .get(key)
            .is_some_and(|(_, size)| *size == (cols, rows))
        {
            return;
        }
        // Miss: (re)encode once for this size. `SlicedProtocol::new` consumes a `DynamicImage`,
        // so clone here — only on a real miss (matches the reader's old per-size clone frequency).
        if let Ok(sliced) = SlicedProtocol::new(&self.picker, image.clone(), Some(Size::new(cols, rows))) {
            self.slices.insert(key.to_string(), (sliced, (cols, rows)));
        }
    }

    fn paint(&mut self, f: &mut Frame, key: &str, area: Rect, x: i16, y: i16) -> bool {
        match self.slices.get(key) {
            Some((sliced, _)) => {
                f.render_widget(SlicedImage::new(sliced, SignedPosition::from((x, y))), area);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    // The real `ratatui-image` encode + paint path (the desktop side of the `ImageSink` seam) must
    // run without panicking; an unprepared key paints nothing. (The frontend reader test only
    // exercises the layout/placeholder path via a no-op sink.)
    #[test]
    fn ensure_then_paint_a_tiny_image_without_panic() {
        let mut sink = TerminalImageSink::new(Picker::halfblocks());
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4));
        sink.ensure("k", &img, 4, 2);
        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();
        terminal
            .draw(|f| {
                let _ = sink.paint(f, "k", Rect::new(0, 0, 20, 10), 0, 0);
                assert!(
                    !sink.paint(f, "missing", Rect::new(0, 0, 20, 10), 0, 0),
                    "an unprepared key paints nothing"
                );
            })
            .unwrap();
    }
}
