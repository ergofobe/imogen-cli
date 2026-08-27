//! The Kitty terminal graphics protocol.
//!
//! A picture is handed to the terminal as base64 PNG inside an APC escape, with a cell box
//! to scale it into. The terminal does the scaling, which is why this looks right on a
//! HiDPI screen where a character cell is not the size the program guessed.
//!
//! Chunks are capped at 4096 base64 bytes because that is what the protocol allows, and
//! `q=2` asks the terminal not to answer: an unread reply would otherwise arrive on stdin
//! and be typed into whatever comes next.

use anyhow::Result;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use image::DynamicImage;

const CHUNK: usize = 4096;

/// Which way of drawing a picture this terminal will understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Kitty,
    /// Two pixels per character cell, using the upper-half block and 24-bit colour.
    HalfBlocks,
    /// Nothing worth trying.
    None,
}

pub fn detect() -> Protocol {
    if std::env::var_os("IMOGEN_NO_IMAGES").is_some() {
        return Protocol::None;
    }
    match std::env::var("IMOGEN_IMAGE_PROTOCOL").as_deref() {
        Ok("kitty") => return Protocol::Kitty,
        Ok("blocks") => return Protocol::HalfBlocks,
        Ok("none") => return Protocol::None,
        _ => {}
    }

    let term = std::env::var("TERM").unwrap_or_default();
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let supports_kitty = std::env::var_os("KITTY_WINDOW_ID").is_some()
        || term.contains("kitty")
        || program.eq_ignore_ascii_case("ghostty")
        || std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some()
        || program.eq_ignore_ascii_case("WezTerm")
        || std::env::var_os("WEZTERM_EXECUTABLE").is_some()
        || std::env::var_os("KONSOLE_VERSION").is_some();

    if supports_kitty {
        return Protocol::Kitty;
    }
    if term == "dumb" || term.is_empty() {
        return Protocol::None;
    }
    Protocol::HalfBlocks
}

/// How large a character cell is, in pixels. Terminals that will not say are assumed to
/// use a cell twice as tall as it is wide, which is close enough for every monospace font
/// worth the name.
pub fn cell_size() -> (u16, u16) {
    if let Ok(size) = crossterm::terminal::window_size() {
        if size.width > 0 && size.height > 0 && size.columns > 0 && size.rows > 0 {
            return (size.width / size.columns, size.height / size.rows);
        }
    }
    (8, 16)
}

/// The number of columns a picture of this shape needs to fill `rows` rows.
pub fn columns_for(image: &DynamicImage, rows: u16) -> u16 {
    let (cell_width, cell_height) = cell_size();
    let aspect = image.width() as f64 / image.height().max(1) as f64;
    let pixels_tall = rows as f64 * cell_height as f64;
    let pixels_wide = pixels_tall * aspect;
    ((pixels_wide / cell_width.max(1) as f64).round() as u16).max(1)
}

/// The escape sequence that draws this picture in a box of `cols` × `rows` cells.
pub fn draw(image: &DynamicImage, cols: u16, rows: u16) -> Result<String> {
    let (cell_width, cell_height) = cell_size();
    // Resizing here rather than sending the full-size original keeps a 4000-pixel
    // photograph from becoming a megabyte of base64 for a thumbnail-sized box.
    let target_width = (cols as u32 * cell_width as u32).max(1);
    let target_height = (rows as u32 * cell_height as u32).max(1);
    let scaled = image.thumbnail(target_width, target_height);

    let mut png = Vec::new();
    scaled.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    let encoded = STANDARD.encode(&png);

    let mut out = String::with_capacity(encoded.len() + 256);
    let chunks: Vec<&str> = encoded
        .as_bytes()
        .chunks(CHUNK)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect();

    for (index, chunk) in chunks.iter().enumerate() {
        let more = if index + 1 < chunks.len() { 1 } else { 0 };
        if index == 0 {
            out.push_str(&format!(
                "\x1b_Ga=T,f=100,q=2,c={cols},r={rows},m={more};{chunk}\x1b\\"
            ));
        } else {
            out.push_str(&format!("\x1b_Gm={more};{chunk}\x1b\\"));
        }
    }
    Ok(out)
}

/// Removes every picture the terminal is holding for us. The TUI calls this before each
/// redraw, because a picture placed at a cell stays there until it is deleted.
pub fn clear_all() -> String {
    "\x1b_Ga=d,d=A,q=2;\x1b\\".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    #[test]
    fn a_picture_is_chunked_with_the_control_data_on_the_first_chunk_only() {
        let image = DynamicImage::ImageRgba8(RgbaImage::new(64, 64));
        let escape = draw(&image, 10, 5).unwrap();
        assert!(escape.starts_with("\x1b_Ga=T,f=100,q=2,c=10,r=5,m="));
        assert!(escape.ends_with("\x1b\\"));
        assert_eq!(escape.matches("a=T").count(), 1);
    }

    #[test]
    fn columns_follow_the_shape_of_the_picture() {
        // A cell is twice as tall as it is wide, so a square picture is twice as wide in
        // cells as it is tall.
        let square = DynamicImage::ImageRgba8(RgbaImage::new(100, 100));
        assert!(columns_for(&square, 10) >= 10);
    }
}
