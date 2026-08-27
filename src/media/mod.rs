//! Turning a photograph into something a terminal can show.

pub mod blocks;
pub mod kitty;

use anyhow::{bail, Result};
use image::DynamicImage;
use imogen_sdk::AssetVariant;

use crate::context::Context;

/// Fetches a rendition and returns the escape sequence, or block drawing, that puts it on
/// screen. `rows` is how tall the picture may be; the width follows its shape.
pub async fn render_asset(ctx: &Context, asset_id: &str, rows: u16) -> Result<String> {
    let variant = if rows > 12 {
        AssetVariant::Preview
    } else {
        AssetVariant::Thumbnail
    };
    let bytes = ctx.client.assets.bytes(asset_id, variant).await?;
    let image = decode(&bytes)?;
    Ok(render(&image, rows))
}

pub fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    Ok(image::load_from_memory(bytes)?)
}

/// Draws with whatever the terminal supports.
pub fn render(image: &DynamicImage, rows: u16) -> String {
    match kitty::detect() {
        kitty::Protocol::Kitty => {
            let cols = kitty::columns_for(image, rows);
            kitty::draw(image, cols, rows).unwrap_or_else(|_| fallback(image, rows))
        }
        kitty::Protocol::HalfBlocks => fallback(image, rows),
        kitty::Protocol::None => String::new(),
    }
}

fn fallback(image: &DynamicImage, rows: u16) -> String {
    let (cols, rows) = blocks::size_for(image, rows);
    blocks::draw(image, cols, rows)
}

/// The picture placed at a given cell, for the terminal browser, which draws its own
/// layout and then puts photographs into holes it has left for them.
pub fn place_at(image: &DynamicImage, col: u16, row: u16, cols: u16, rows: u16) -> Result<String> {
    let protocol = kitty::detect();
    let body = match protocol {
        kitty::Protocol::Kitty => kitty::draw(image, cols, rows)?,
        kitty::Protocol::HalfBlocks => {
            // Block drawing has no notion of position, so each line is placed itself.
            let drawing = blocks::draw(image, cols, rows);
            let mut out = String::new();
            for (index, line) in drawing.lines().enumerate() {
                out.push_str(&format!(
                    "\x1b[{};{}H{line}",
                    row + index as u16 + 1,
                    col + 1
                ));
            }
            return Ok(out);
        }
        kitty::Protocol::None => bail!("This terminal cannot show pictures"),
    };
    // Cursor addressing is one-based, and the caller counts from zero like the rest of the
    // program does.
    Ok(format!("\x1b[{};{}H{body}", row + 1, col + 1))
}
