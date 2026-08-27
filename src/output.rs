//! How results reach whoever asked for them.
//!
//! Every command can answer twice: as a table meant for a person, and as JSON meant for a
//! program. The JSON is the API's own payload, unrenamed and uncollapsed, so an agent
//! reading `imogen ls --json` sees exactly what the server said and never has to parse a
//! table that was designed to be readable rather than stable.

use std::io::{IsTerminal, Write};

use anyhow::Result;
use serde::Serialize;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Human,
    Json,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub mode: Mode,
    pub color: bool,
    pub quiet: bool,
}

impl Output {
    pub fn new(json: bool, no_color: bool, quiet: bool) -> Self {
        let color = !no_color
            && std::env::var_os("NO_COLOR").is_none()
            && std::io::stdout().is_terminal()
            && !json;
        Self {
            mode: if json { Mode::Json } else { Mode::Human },
            color,
            quiet,
        }
    }

    pub fn is_json(&self) -> bool {
        self.mode == Mode::Json
    }

    /// The machine-readable answer. Commands call this and then return, so nothing else
    /// lands on stdout to break a `| jq`.
    pub fn json<T: Serialize>(&self, value: &T) -> Result<()> {
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, value)?;
        stdout.write_all(b"\n")?;
        Ok(())
    }

    /// A line of ordinary output. Suppressed by `--quiet`, which leaves only data behind.
    pub fn line(&self, text: impl AsRef<str>) {
        if !self.quiet {
            println!("{}", text.as_ref());
        }
    }

    /// A line that is the answer rather than commentary — an id, a URL — so it survives
    /// `--quiet` and can be piped straight into the next command.
    pub fn value(&self, text: impl AsRef<str>) {
        println!("{}", text.as_ref());
    }

    /// Progress and warnings go to stderr, where they do not contaminate a pipe.
    pub fn note(&self, text: impl AsRef<str>) {
        if !self.quiet {
            eprintln!("{}", text.as_ref());
        }
    }

    pub fn warn(&self, text: impl AsRef<str>) {
        eprintln!(
            "{}",
            self.paint(&format!("warning: {}", text.as_ref()), YELLOW)
        );
    }

    pub fn heading(&self, text: impl AsRef<str>) {
        if !self.quiet {
            println!("{}", self.paint(text.as_ref(), BOLD));
        }
    }

    pub fn paint(&self, text: &str, code: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint(text, DIM)
    }

    /// A key/value block, for the detail views where a table would be one long row.
    pub fn fields(&self, rows: &[(&str, String)]) {
        let width = rows
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, _)| key.width())
            .max()
            .unwrap_or(0);
        for (key, value) in rows {
            if value.is_empty() {
                continue;
            }
            println!("{:width$}  {}", self.dim(key), value, width = width);
        }
    }

    pub fn table(&self, headers: &[&str], rows: &[Vec<String>]) {
        if rows.is_empty() {
            return;
        }
        let mut widths: Vec<usize> = headers.iter().map(|h| h.width()).collect();
        for row in rows {
            for (index, cell) in row.iter().enumerate() {
                if index < widths.len() {
                    widths[index] = widths[index].max(cell.width());
                }
            }
        }

        if !self.quiet {
            let header: Vec<String> = headers
                .iter()
                .enumerate()
                .map(|(index, text)| pad(text, widths[index]))
                .collect();
            println!("{}", self.paint(header.join("  ").trim_end(), BOLD));
        }

        for row in rows {
            let line: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(index, cell)| pad(cell, *widths.get(index).unwrap_or(&0)))
                .collect();
            println!("{}", line.join("  ").trim_end());
        }
    }
}

fn pad(text: &str, width: usize) -> String {
    let current = text.width();
    if current >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - current))
    }
}

pub const BOLD: &str = "1";
pub const DIM: &str = "2";
pub const YELLOW: &str = "33";
pub const GREEN: &str = "32";
pub const RED: &str = "31";

/// Bytes as something a person reads at a glance. Binary units, because a photo library's
/// disk usage is reported in them everywhere else the owner will look.
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else if size >= 100.0 {
        format!("{size:.0} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// The date half of an ISO-8601 timestamp, which is all a table has room for. Anything
/// that is not the shape the contract promises is passed through untouched rather than
/// silently becoming a wrong date.
pub fn date(timestamp: &str) -> String {
    timestamp.split('T').next().unwrap_or(timestamp).to_string()
}

/// Date and time to the minute, for detail views.
pub fn datetime(timestamp: &str) -> String {
    match timestamp.split_once('T') {
        Some((day, rest)) => {
            let clock: String = rest.chars().take(5).collect();
            format!("{day} {clock}")
        }
        None => timestamp.to_string(),
    }
}

/// "1 photograph", "3 photographs". Worth the six lines: a summary that says
/// "1 photographs" reads as a program that was not finished.
pub fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

pub fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for character in text.chars() {
        let w = character.to_string().width();
        if used + w > width.saturating_sub(1) {
            break;
        }
        out.push(character);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_reads_as_a_person_would_say_it() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(2048), "2.0 KiB");
        assert_eq!(bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
        assert_eq!(bytes(200 * 1024 * 1024), "200 MiB");
    }

    #[test]
    fn timestamps_that_are_not_iso_pass_through_unchanged() {
        assert_eq!(date("2024-06-01T09:30:00.000Z"), "2024-06-01");
        assert_eq!(datetime("2024-06-01T09:30:00.000Z"), "2024-06-01 09:30");
        assert_eq!(date("sometime"), "sometime");
    }

    #[test]
    fn counts_agree_with_their_nouns() {
        assert_eq!(plural(1, "photograph"), "1 photograph");
        assert_eq!(plural(0, "photograph"), "0 photographs");
        assert_eq!(plural(3, "file"), "3 files");
    }

    #[test]
    fn truncate_leaves_room_for_the_ellipsis() {
        assert_eq!(truncate("harbour.jpg", 20), "harbour.jpg");
        assert_eq!(truncate("a-very-long-filename.jpg", 10), "a-very-lo…");
    }
}
