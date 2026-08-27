//! Drawing the terminal browser.
//!
//! The layout leaves holes: a tile draws its border and its caption and nothing in the
//! middle, and the picture is placed into that middle afterwards by the Kitty protocol.
//! Nothing here writes a pixel.

use imogen_sdk::{AssetStatus, AssetType};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::output;
use crate::tui::app::{App, Mode, Tile};

const ACCENT: Color = Color::Rgb(0xE3, 0x9B, 0x5C);
const MUTED: Color = Color::Rgb(0x90, 0x96, 0xA0);
const BORDER: Color = Color::Rgb(0x26, 0x2A, 0x2F);

/// The gutter down the right of the grid: four cells for a year and one for the marker.
const RAIL: u16 = 5;

/// Works out where everything goes, including the holes the photographs are placed into.
/// Called before drawing so the placement pass and the draw pass agree.
pub fn layout(app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    let body = chunks[1];
    let content = if app.show_info && app.mode != Mode::Viewer {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20), Constraint::Length(34)])
            .split(body);
        split[0]
    } else {
        body
    };

    app.grid_area = content;
    app.rail_area = Rect::default();
    app.tiles.clear();

    if matches!(app.mode, Mode::Picker | Mode::PickerPath(_)) {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(52), Constraint::Min(20)])
            .split(body);
        app.picker_list_area = split[0];
        // One cell of border all round the preview pane.
        app.picker_preview_area = Rect {
            x: split[1].x + 1,
            y: split[1].y + 1,
            width: split[1].width.saturating_sub(2),
            height: split[1].height.saturating_sub(2),
        };
        return;
    }

    if app.mode == Mode::Viewer {
        if let Some(id) = app.selected_id() {
            let inner = Rect {
                x: content.x + 1,
                y: content.y + 1,
                width: content.width.saturating_sub(2),
                height: content.height.saturating_sub(2),
            };
            app.tiles.push(Tile {
                id,
                inner,
                index: app.selected,
            });
        }
        return;
    }
    if app.mode == Mode::Albums || app.mode == Mode::Help {
        return;
    }

    // The year rail only earns its gutter if what is left still fits a tile.
    let content = if content.width > RAIL + app.tile_width {
        app.rail_area = Rect {
            x: content.x + content.width - RAIL,
            y: content.y,
            width: RAIL,
            height: content.height,
        };
        Rect {
            width: content.width - RAIL,
            ..content
        }
    } else {
        content
    };
    app.grid_area = content;

    let columns = (content.width / app.tile_width.max(1)).max(1) as usize;
    app.columns = columns;
    let visible = (content.height / app.tile_height.max(1)).max(1) as usize;

    for row in 0..visible {
        for column in 0..columns {
            let index = (app.scroll + row) * columns + column;
            let Some(tile) = app.window.get(index) else {
                continue;
            };
            let x = content.x + column as u16 * app.tile_width;
            let y = content.y + row as u16 * app.tile_height;
            app.tiles.push(Tile {
                id: tile.id.clone(),
                // One cell of border all round, and the last row of the tile is the caption.
                inner: Rect {
                    x: x + 1,
                    y: y + 1,
                    width: app.tile_width.saturating_sub(2),
                    height: app.tile_height.saturating_sub(3),
                },
                index,
            });
        }
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    draw_title(frame, app, chunks[0]);
    draw_footer(frame, app, chunks[2]);

    match &app.mode {
        Mode::Picker | Mode::PickerPath(_) => draw_picker(frame, app),
        Mode::Albums => draw_albums(frame, app, chunks[1]),
        Mode::Help => draw_help(frame, chunks[1], app.help_shows_picker),
        Mode::Viewer => draw_viewer(frame, app, chunks[1]),
        _ => {
            draw_grid(frame, app);
            draw_rail(frame, app);
            if app.show_info {
                let split = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Min(20), Constraint::Length(34)])
                    .split(chunks[1]);
                draw_info(frame, app, split[1]);
            }
        }
    }
}

fn draw_title(frame: &mut Frame, app: &App, area: Rect) {
    let scope = match (&app.album, app.scope) {
        (Some(album), _) => format!("album · {}", album.name),
        (None, scope) => scope.label().to_string(),
    };
    // The buckets know the whole count before a single picture has been fetched, so this
    // never has to say "so many loaded so far".
    let counted = format!("{} photographs", app.total);
    let mut left = vec![
        Span::styled(
            " imogen ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("· {scope}"), Style::default().fg(MUTED)),
    ];
    if !app.query.is_empty() {
        left.push(Span::styled(
            format!(" · “{}”", app.query),
            Style::default().fg(Color::White),
        ));
    }
    if app.loading {
        left.push(Span::styled(" · loading…", Style::default().fg(MUTED)));
    }

    frame.render_widget(Paragraph::new(Line::from(left)), area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{counted} "),
            Style::default().fg(MUTED),
        )))
        .alignment(Alignment::Right),
        area,
    );
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let text = match &app.mode {
        Mode::Search(input) => format!(" search: {input}▏"),
        Mode::JumpDate(input) => format!(" jump to: {input}▏"),
        Mode::PickerPath(input) => format!(" go to: {input}▏"),
        Mode::Picker => {
            let picker = app.picker.as_ref();
            let chosen = picker.map(|p| p.chosen.len()).unwrap_or(0);
            format!(
                " {}  ·  space pick · enter open · u upload · esc back",
                if chosen == 0 {
                    "nothing picked".to_string()
                } else {
                    output::plural(chosen, "item")
                }
            )
        }
        Mode::Confirm { prompt, .. } => format!(" {prompt}  [y/N]"),
        // A run in progress outranks whatever the last message was: it is the only thing
        // on screen that is still changing.
        _ if app.uploading() => format!(
            " uploading {}/{}{}",
            app.upload_done + app.upload_failed,
            app.upload_total,
            if app.upload_failed > 0 {
                format!("  ({} failed)", app.upload_failed)
            } else {
                String::new()
            }
        ),
        _ => match &app.status {
            Some(status) => format!(" {status}"),
            // The filename and the size come from the whole record, which is only read
            // for the photograph being looked at; the day comes from the tile, which is
            // always there. Waiting for the record before saying anything would leave the
            // footer blank for every photograph somebody merely scrolled past.
            None => match app.selected_tile() {
                Some(tile) => {
                    let named = match app.detail() {
                        Some(asset) => format!(
                            "{}  ·  {}  ·  ",
                            output::truncate(&asset.original_filename, 40),
                            output::bytes(asset.size_bytes)
                        ),
                        None => String::new(),
                    };
                    format!(
                        " {named}{}  ·  {} of {}{}",
                        output::date(&tile.captured_at),
                        app.selected + 1,
                        app.total,
                        if tile.favorite { "  ★" } else { "" }
                    )
                }
                // The buckets know what day this is even before its tiles arrive, so a
                // stretch still on its way says where it is rather than "nothing here" —
                // which is what an empty library says, and means something else.
                None => match app.date_at_index(app.selected) {
                    Some(date) => format!(
                        " {}  ·  {} of {}",
                        output::date(&format!("{date}T00:00:00.000Z")),
                        app.selected + 1,
                        app.total
                    ),
                    None if app.loading => " loading…".to_string(),
                    None => " nothing here".to_string(),
                },
            },
        },
    };

    let style = match &app.mode {
        Mode::Search(_) | Mode::PickerPath(_) | Mode::JumpDate(_) => Style::default().fg(ACCENT),
        Mode::Picker => Style::default().fg(MUTED),
        Mode::Confirm { .. } => Style::default().fg(Color::Rgb(0xE0, 0x7A, 0x5F)),
        _ if app.uploading() => Style::default().fg(ACCENT),
        _ => Style::default().fg(MUTED),
    };
    // The hint is right-aligned over the same row, so the left text is cut to fit rather
    // than being drawn underneath it.
    const HINT: &str = "? help  q quit ";
    let room = (area.width as usize).saturating_sub(HINT.width());
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            output::truncate(&text, room),
            style,
        ))),
        area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(HINT, Style::default().fg(MUTED))))
            .alignment(Alignment::Right),
        area,
    );
}

fn draw_grid(frame: &mut Frame, app: &App) {
    if app.total == 0 {
        let message = if app.loading {
            "Loading…"
        } else if app.query.is_empty() {
            "Nothing here yet. Upload something, or press / to search."
        } else {
            "Nothing matched. Press / to search for something else."
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(MUTED))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            centred(app.grid_area, 60, 1),
        );
        return;
    }

    if app.tiles.is_empty() {
        // The spine says there are photographs here; their tiles are still on their way.
        let where_at = app
            .date_at_index(app.selected)
            .map(|date| format!("Fetching {date}…"))
            .unwrap_or_else(|| "Fetching…".to_string());
        frame.render_widget(
            Paragraph::new(where_at)
                .style(Style::default().fg(MUTED))
                .alignment(Alignment::Center),
            centred(app.grid_area, 30, 1),
        );
        return;
    }

    for tile in &app.tiles {
        let Some(held) = app.window.get(tile.index) else {
            continue;
        };
        let selected = tile.index == app.selected;
        let outer = Rect {
            x: tile.inner.x.saturating_sub(1),
            y: tile.inner.y.saturating_sub(1),
            width: tile.inner.width + 2,
            height: tile.inner.height + 3,
        };

        let border = if selected {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(BORDER)
        };
        frame.render_widget(
            Block::default().borders(Borders::ALL).border_style(border),
            outer,
        );

        // The caption sits on the tile's last row, inside the border.
        let caption = Rect {
            x: tile.inner.x,
            y: tile.inner.y + tile.inner.height,
            width: tile.inner.width,
            height: 1,
        };
        let mut marks = String::new();
        if held.favorite {
            marks.push('★');
        }
        if held.r#type == AssetType::Video {
            marks.push('▶');
        }
        if held.status != AssetStatus::Ready {
            marks.push('·');
        }
        let label = format!(
            "{}{}",
            output::date(&held.captured_at),
            if marks.is_empty() {
                String::new()
            } else {
                format!(" {marks}")
            }
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                output::truncate(&label, caption.width as usize),
                if selected {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(MUTED)
                },
            ))),
            caption,
        );
    }
}

/// The year rail: where you are in twenty years, in five cells.
///
/// The grid itself cannot say this — a screen of tiles looks the same in 2009 as in 2024 —
/// so the rail is the only thing on screen that answers "how far in am I".
fn draw_rail(frame: &mut Frame, app: &App) {
    let area = app.rail_area;
    if area.width == 0 || area.height == 0 || app.total == 0 {
        return;
    }
    let here = app.rail_row(area.height);
    let marks = app.year_marks(area.height);

    let lines: Vec<Line> = (0..area.height)
        .map(|row| {
            let year = marks
                .iter()
                .find(|(at, _)| *at == row)
                .map(|(_, year)| year.as_str())
                .unwrap_or("");
            let style = if row == here {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(MUTED)
            };
            Line::from(Span::styled(
                format!("{}{year:>4}", if row == here { "\u{203a}" } else { " " }),
                style,
            ))
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_viewer(frame: &mut Frame, app: &App, area: Rect) {
    // The filename once the record has arrived, the day until then: a viewer with no
    // title at all reads as a viewer that has failed.
    let title = match (app.detail(), app.selected_tile()) {
        (Some(asset), _) => format!(" {} ", asset.original_filename),
        (None, Some(tile)) => format!(" {} ", output::date(&tile.captured_at)),
        (None, None) => String::new(),
    };
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER))
            .title(Span::styled(title, Style::default().fg(ACCENT))),
        area,
    );
    let showing = app
        .selected_id()
        .map(|id| app.preview_for(&id).is_some())
        .unwrap_or(false);
    if !showing {
        frame.render_widget(
            Paragraph::new("Loading…")
                .style(Style::default().fg(MUTED))
                .alignment(Alignment::Center),
            centred(area, 20, 1),
        );
    }
}

fn draw_picker(frame: &mut Frame, app: &App) {
    let Some(picker) = app.picker.as_ref() else {
        return;
    };

    let items: Vec<ListItem> = picker
        .entries
        .iter()
        .map(|entry| {
            let ticked = picker.is_chosen(entry);
            let name = if entry.is_dir {
                format!("{}/", entry.name)
            } else {
                entry.name.clone()
            };
            let style = if entry.is_dir {
                Style::default().fg(ACCENT)
            } else if entry.is_media {
                Style::default().fg(Color::White)
            } else {
                // Not something imogen usually stores. Still choosable, but not offered.
                Style::default().fg(MUTED)
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    if ticked { "✓ " } else { "  " },
                    Style::default().fg(if ticked { ACCENT } else { MUTED }),
                ),
                Span::styled(format!("{:<38}", output::truncate(&name, 38)), style),
                Span::styled(
                    if entry.is_dir {
                        String::new()
                    } else {
                        format!("{:>9}", output::bytes(entry.size))
                    },
                    Style::default().fg(MUTED),
                ),
            ]))
        })
        .collect();

    let here = compress_home(&picker.cwd);
    let mut state = ListState::default();
    state.select(Some(picker.cursor));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(BORDER))
                    .title(Span::styled(
                        format!(" {} ", output::truncate_left(&here, 46)),
                        Style::default().fg(ACCENT),
                    )),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol(""),
        app.picker_list_area,
        &mut state,
    );

    let pane = Rect {
        x: app.picker_preview_area.x.saturating_sub(1),
        y: app.picker_preview_area.y.saturating_sub(1),
        width: app.picker_preview_area.width + 2,
        height: app.picker_preview_area.height + 2,
    };
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER)),
        pane,
    );

    // The picture itself is placed after this frame is drawn. What goes here is only what
    // to say when there will not be one.
    let message = match picker.current() {
        _ if picker.error.is_some() => picker.error.clone(),
        Some(entry) if entry.is_dir => Some(format!("{}/", entry.name)),
        Some(entry) if !crate::tui::picker::is_previewable(&entry.path) => Some(format!(
            "{}

no preview — it will still upload",
            entry.name
        )),
        Some(entry) => match app.local_previews.get(&entry.path) {
            Some(Some(_)) => None,
            Some(None) => Some(format!(
                "{}

could not read it",
                entry.name
            )),
            None => Some("…".to_string()),
        },
        None => Some("empty".to_string()),
    };
    if let Some(message) = message {
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(MUTED))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            centred(
                app.picker_preview_area,
                app.picker_preview_area.width.saturating_sub(2),
                5,
            ),
        );
    }
}

/// `~/Pictures` rather than the whole path, which is how somebody would say it.
fn compress_home(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    match dirs::home_dir() {
        Some(home) => {
            let home = home.display().to_string();
            match text.strip_prefix(&home) {
                Some("") => "~".to_string(),
                Some(rest) => format!("~{rest}"),
                None => text,
            }
        }
        None => text,
    }
}

fn draw_albums(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .albums
        .iter()
        .map(|album| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<40}", output::truncate(&album.name, 40)),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{:>6}", album.asset_count),
                    Style::default().fg(MUTED),
                ),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.album_selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(BORDER))
                    .title(Span::styled(" albums ", Style::default().fg(ACCENT))),
            )
            .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            .highlight_symbol("› "),
        area,
        &mut state,
    );
}

fn draw_info(frame: &mut Frame, app: &App, area: Rect) {
    let Some(asset) = app.detail() else {
        return;
    };
    let mut lines = vec![
        field("file", &asset.original_filename),
        field("taken", &output::datetime(&asset.captured_at)),
        field("added", &output::datetime(&asset.created_at)),
        field("size", &output::bytes(asset.size_bytes)),
    ];
    if let (Some(width), Some(height)) = (asset.width, asset.height) {
        lines.push(field("pixels", &format!("{width} × {height}")));
    }
    if let Some(exif) = &asset.exif {
        let camera: Vec<&str> = [exif.make.as_deref(), exif.model.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        if !camera.is_empty() {
            lines.push(field("camera", &camera.join(" ")));
        }
        if let Some(lens) = &exif.lens {
            lines.push(field("lens", lens));
        }
    }
    if let Some(location) = &asset.location {
        lines.push(field(
            "where",
            &location
                .place
                .clone()
                .unwrap_or_else(|| format!("{:.4}, {:.4}", location.latitude, location.longitude)),
        ));
    }
    if let Some(description) = &asset.description {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            description.clone(),
            Style::default().fg(Color::White),
        )));
    }
    lines.push(Line::from(""));
    lines.push(field("id", &asset.id));

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(BORDER))
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        area,
    );
}

fn field<'a>(key: &'a str, value: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{key:<8}"), Style::default().fg(MUTED)),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
    ])
}

fn draw_help(frame: &mut Frame, area: Rect, picking: bool) {
    let browsing = [
        ("↑ ↓ ← →  h j k l", "move"),
        ("enter", "look at it"),
        ("escape", "back"),
        ("/", "search"),
        ("f", "favourite"),
        ("e", "archive"),
        ("d", "move to the trash"),
        ("r", "restore from the trash"),
        ("i", "details"),
        ("a", "albums"),
        ("u", "pick files to upload"),
        ("1 2 3 4", "library · favourites · archive · trash"),
        ("g", "jump to a date — 2011, aug 2011, 2011-08-14"),
        ("[  ]", "a year older · a year newer"),
        ("home  end", "first · last"),
        ("R", "reload"),
        ("?", "this"),
        ("q", "quit"),
    ];
    let picking_keys = [
        ("↑ ↓  k j", "move"),
        ("space", "pick, or unpick"),
        ("enter  l  →", "open a folder · pick a file"),
        ("h  ←  backspace", "back up a folder"),
        ("a  A", "pick every photo here · pick none"),
        (".", "show hidden files"),
        ("~", "go home"),
        ("/", "go to a path"),
        ("u", "upload what is picked"),
        ("escape", "back to the library"),
    ];
    let rows: &[(&str, &str)] = if picking { &picking_keys } else { &browsing };
    let lines: Vec<Line> = rows
        .iter()
        .map(|(keys, what)| {
            Line::from(vec![
                Span::styled(format!("  {keys:<18}"), Style::default().fg(ACCENT)),
                Span::styled(*what, Style::default().fg(Color::White)),
            ])
        })
        .collect();

    let area = centred(area, 64, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .title(Span::styled(
                    if picking {
                        " keys · picking files "
                    } else {
                        " keys "
                    },
                    Style::default().fg(ACCENT),
                )),
        ),
        area,
    );
}

fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::picker::Picker;
    use imogen_sdk::{AssetStatus, AssetType};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Renders a frame and flattens it to text, so an assertion can say what somebody
    /// would actually see. Screen-scraping a real terminal cannot: ratatui writes only the
    /// cells that changed, so "1 item" becoming "2 items" never crosses the wire as a
    /// whole string.
    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        layout(app, Rect::new(0, 0, width, height));
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("photo-a.jpg"), b"x").unwrap();
        std::fs::write(dir.path().join("photo-b.jpg"), b"x").unwrap();
        dir
    }

    fn picking(dir: &std::path::Path) -> App {
        let mut app = App::new();
        app.picker = Some(Picker::open(dir));
        app.mode = Mode::Picker;
        app
    }

    /// Not an assertion — prints the screen so a change to the layout can be looked at.
    #[test]
    #[ignore = "for looking at, not for CI"]
    fn show() {
        let dir = fixture();
        let mut app = picking(dir.path());
        app.picker.as_mut().unwrap().move_by(2);
        app.picker.as_mut().unwrap().toggle();
        app.picker.as_mut().unwrap().move_by(1);
        println!("{}", render(&mut app, 100, 20));
        app.help_shows_picker = true;
        app.mode = Mode::Help;
        println!("\n{}", render(&mut app, 100, 24));
        app.help_shows_picker = false;
        println!("\n{}", render(&mut app, 100, 24));
    }

    #[test]
    fn the_picker_lists_folders_first_and_marks_what_is_not_media() {
        let dir = fixture();
        let mut app = picking(dir.path());
        let screen = render(&mut app, 100, 20);
        let listed: Vec<&str> = screen
            .lines()
            .filter(|l| l.contains("nested") || l.contains("notes") || l.contains("photo-"))
            .collect();
        assert!(
            listed[0].contains("nested/"),
            "folders come first: {listed:?}"
        );
        assert!(screen.contains("photo-a.jpg"));
        assert!(screen.contains("notes.txt"));
    }

    #[test]
    fn a_file_that_cannot_be_drawn_says_so_and_says_it_will_still_upload() {
        let dir = fixture();
        let mut app = picking(dir.path());
        // nested/, notes.txt, photo-a.jpg, photo-b.jpg — one step down is the text file.
        app.picker.as_mut().unwrap().move_by(1);
        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("no preview"), "{screen}");
        // Whether it wraps depends on the width; what matters is that both halves of the
        // reassurance are on screen somewhere.
        assert!(
            screen.contains("still") && screen.contains("upload"),
            "{screen}"
        );
    }

    #[test]
    fn the_footer_counts_what_has_been_picked() {
        let dir = fixture();
        let mut app = picking(dir.path());
        assert!(render(&mut app, 100, 20).contains("nothing picked"));

        // Ticking does not move the cursor — the key handler does that — so picking a
        // second file means stepping onto it first.
        let picker = app.picker.as_mut().unwrap();
        picker.move_by(2);
        picker.toggle();
        assert!(render(&mut app, 100, 20).contains("1 item"));

        let picker = app.picker.as_mut().unwrap();
        picker.move_by(1);
        picker.toggle();
        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("2 items"), "{screen}");

        // And ticking the same file again takes it back off.
        app.picker.as_mut().unwrap().toggle();
        assert!(render(&mut app, 100, 20).contains("1 item"));
    }

    #[test]
    fn the_folder_being_looked_at_is_named_the_way_a_person_would_say_it() {
        let dir = fixture();
        let mut app = picking(dir.path());
        let name = dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(render(&mut app, 100, 20).contains(&name));
    }

    /// Not an assertion — prints the grid so the rail can be looked at.
    #[test]
    #[ignore = "for looking at, not for CI"]
    fn show_grid() {
        let mut app = browsing();
        app.selected = 500;
        app.keep_selection_visible();
        println!("{}", render(&mut app, 90, 24));
    }

    /// A grid over twenty years, so the rail has something to say.
    fn browsing() -> App {
        let mut app = App::new();
        app.buckets = (0..20)
            .map(|n| imogen_sdk::TimelineBucket {
                date: format!("{}-06-01", 2024 - n),
                count: 40,
                cover_asset_id: None,
            })
            .collect();
        app.recount();
        app.periods.insert(
            "2024-06".into(),
            (0..40)
                .map(|n| imogen_sdk::TimelineTile {
                    id: format!("a{n}"),
                    captured_at: "2024-06-01T09:30:00.000Z".into(),
                    width: None,
                    height: None,
                    r#type: AssetType::Image,
                    status: AssetStatus::Ready,
                    favorite: false,
                    duration: None,
                    placeholder_color: None,
                    live_photo_video_id: None,
                })
                .collect(),
        );
        app.rebuild_window();
        app
    }

    /// The grid itself looks the same in 2009 as in 2024. The rail is the only thing on
    /// screen that says how far into twenty years you are.
    #[test]
    fn the_year_rail_says_where_in_the_library_you_are() {
        let mut app = browsing();
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("2024"), "{screen}");
        assert!(
            screen.contains("›"),
            "the cursor has a place on the rail: {screen}"
        );
        // And the rail is in its gutter, not over the tiles.
        assert!(app.rail_area.width > 0);
        assert_eq!(
            app.rail_area.x + app.rail_area.width,
            app.grid_area.x + app.grid_area.width + RAIL
        );
    }

    /// A narrow window keeps the photographs and loses the rail, rather than the other way
    /// round.
    #[test]
    fn a_window_too_narrow_for_both_keeps_the_photographs() {
        let mut app = browsing();
        render(&mut app, 22, 30);
        assert_eq!(app.rail_area.width, 0);
        assert!(app.columns >= 1);
    }

    /// "Nothing here" is what an empty library says. A stretch whose tiles have not
    /// arrived is a different thing, and saying the wrong one of the two reads as a
    /// browser that has broken.
    #[test]
    fn a_stretch_still_on_its_way_says_where_it_is_not_that_there_is_nothing() {
        let mut app = browsing();
        app.selected = 500;
        app.keep_selection_visible();
        let screen = render(&mut app, 90, 24);
        assert!(!screen.contains("nothing here"), "{screen}");
        assert!(screen.contains("2012"), "{screen}");
        assert!(screen.contains("501 of 800"), "{screen}");
    }

    #[test]
    fn the_jump_prompt_reads_like_the_search_prompt() {
        let mut app = browsing();
        app.mode = Mode::JumpDate("aug 2011".into());
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("jump to: aug 2011"), "{screen}");
    }

    /// The buckets know the whole count before a picture has been fetched, so the header
    /// never has to hedge with "so many loaded so far".
    #[test]
    fn the_header_counts_the_whole_library_not_what_has_arrived() {
        let mut app = browsing();
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("800 photographs"), "{screen}");
    }

    #[test]
    fn an_upload_in_progress_outranks_the_last_message() {
        let mut app = App::new();
        app.note("something that happened earlier");
        app.upload_total = 12;
        app.upload_done = 7;
        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("uploading 7/12"), "{screen}");
        assert!(!screen.contains("something that happened"));
    }

    #[test]
    fn a_failure_during_a_run_is_counted_where_it_can_be_seen() {
        let mut app = App::new();
        app.upload_total = 5;
        app.upload_done = 3;
        app.upload_failed = 1;
        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("uploading 4/5"), "{screen}");
        assert!(screen.contains("1 failed"), "{screen}");
    }
}
