//! M1a stub image sink: reserves space but paints nothing (the reader falls back to text
//! placeholders). The real native-`<img>`-overlay sink lands in M1c.

use image::DynamicImage;
use ratzilla::ratatui::Frame;
use ratzilla::ratatui::layout::Rect;
use standard_frontend::image_sink::ImageSink;

#[derive(Default)]
pub struct StubSink;

impl ImageSink for StubSink {
    fn cell_size(&self) -> (u16, u16) {
        (8, 16) // a typical browser monospace cell; image sizing only needs it finite
    }
    fn ensure(&mut self, _key: &str, _image: &DynamicImage, _cols: u16, _rows: u16) {}
    fn paint(&mut self, _f: &mut Frame, _key: &str, _area: Rect, _x: i16, _y: i16) -> bool {
        false // nothing painted → the reader draws its "🖼 …" placeholder
    }
}
