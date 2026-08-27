//! Drawing a picture where the Kitty protocol is not available.
//!
//! Each character cell holds two pixels: the upper half block is painted in the top
//! pixel's colour and its background in the bottom pixel's, which doubles the vertical
//! resolution a terminal can show. It needs 24-bit colour, which every terminal that is
//! not a serial console has had for years.

use image::DynamicImage;

pub fn draw(image: &DynamicImage, cols: u16, rows: u16) -> String {
    let width = cols.max(1) as u32;
    // Two pixels per row, so the picture is sampled at twice the height it occupies.
    let height = (rows.max(1) as u32) * 2;
    let scaled = image.thumbnail_exact(width, height).to_rgba8();

    let mut out = String::new();
    for y in (0..scaled.height()).step_by(2) {
        for x in 0..scaled.width() {
            let top = scaled.get_pixel(x, y).0;
            let bottom = if y + 1 < scaled.height() {
                scaled.get_pixel(x, y + 1).0
            } else {
                top
            };
            out.push_str(&format!(
                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m▀",
                top[0], top[1], top[2], bottom[0], bottom[1], bottom[2]
            ));
        }
        out.push_str("\x1b[0m\n");
    }
    out.pop();
    out
}

/// The shape of the picture in cells, given how many rows it may use.
pub fn size_for(image: &DynamicImage, rows: u16) -> (u16, u16) {
    let aspect = image.width() as f64 / image.height().max(1) as f64;
    // A cell is about twice as tall as it is wide and holds two pixels, so a cell is
    // roughly square in sampled pixels: columns follow the aspect ratio directly.
    let cols = ((rows as f64 * 2.0) * aspect / 2.0 * 2.0).round() as u16;
    (cols.max(1), rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    #[test]
    fn every_row_ends_by_putting_the_colour_back() {
        let image = DynamicImage::ImageRgba8(RgbaImage::new(4, 4));
        let drawn = draw(&image, 4, 2);
        assert_eq!(drawn.lines().count(), 2);
        for line in drawn.lines() {
            assert!(line.ends_with("\x1b[0m"));
        }
    }
}
