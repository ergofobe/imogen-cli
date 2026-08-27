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

use crate::output;
use crate::tui::app::{App, Mode, Tile};

const ACCENT: Color = Color::Rgb(0xE0, 0xA1, 0x62);
const MUTED: Color = Color::Rgb(0x90, 0x96, 0xA0);

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
    app.tiles.clear();

    if app.mode == Mode::Viewer {
        if let Some(asset) = app.selected_asset() {
            let inner = Rect {
                x: content.x + 1,
                y: content.y + 1,
                width: content.width.saturating_sub(2),
                height: content.height.saturating_sub(2),
            };
            app.tiles.push(Tile {
                id: asset.id.clone(),
                inner,
                index: app.selected,
            });
        }
        return;
    }
    if app.mode == Mode::Albums || app.mode == Mode::Help {
        return;
    }

    let columns = (content.width / app.tile_width.max(1)).max(1) as usize;
    app.columns = columns;
    let visible = (content.height / app.tile_height.max(1)).max(1) as usize;

    for row in 0..visible {
        for column in 0..columns {
            let index = (app.scroll + row) * columns + column;
            let Some(asset) = app.assets.get(index) else {
                continue;
            };
            let x = content.x + column as u16 * app.tile_width;
            let y = content.y + row as u16 * app.tile_height;
            app.tiles.push(Tile {
                id: asset.id.clone(),
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
        Mode::Albums => draw_albums(frame, app, chunks[1]),
        Mode::Help => draw_help(frame, chunks[1]),
        Mode::Viewer => draw_viewer(frame, app, chunks[1]),
        _ => {
            draw_grid(frame, app);
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
    let counted = match app.total {
        Some(total) => format!("{total} photographs"),
        None if app.cursor.is_some() => format!("{}+ loaded", app.assets.len()),
        None => format!("{} photographs", app.assets.len()),
    };
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
        Mode::Confirm { prompt, .. } => format!(" {prompt}  [y/N]"),
        _ => match &app.status {
            Some(status) => format!(" {status}"),
            None => match app.selected_asset() {
                Some(asset) => format!(
                    " {}  ·  {}  ·  {}{}",
                    output::truncate(&asset.original_filename, 40),
                    output::date(&asset.captured_at),
                    output::bytes(asset.size_bytes),
                    if asset.favorite { "  ★" } else { "" }
                ),
                None => " nothing here".to_string(),
            },
        },
    };

    let style = match &app.mode {
        Mode::Search(_) => Style::default().fg(ACCENT),
        Mode::Confirm { .. } => Style::default().fg(Color::Rgb(0xE0, 0x7A, 0x5F)),
        _ => Style::default().fg(MUTED),
    };
    frame.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "? help  q quit ",
            Style::default().fg(MUTED),
        )))
        .alignment(Alignment::Right),
        area,
    );
}

fn draw_grid(frame: &mut Frame, app: &App) {
    if app.assets.is_empty() {
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

    for tile in &app.tiles {
        let Some(asset) = app.assets.get(tile.index) else {
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
            Style::default().fg(Color::Rgb(0x2A, 0x2D, 0x32))
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
        if asset.favorite {
            marks.push('★');
        }
        if asset.r#type == AssetType::Video {
            marks.push('▶');
        }
        if asset.status != AssetStatus::Ready {
            marks.push('·');
        }
        let label = format!(
            "{}{}",
            output::date(&asset.captured_at),
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

fn draw_viewer(frame: &mut Frame, app: &App, area: Rect) {
    let title = app
        .selected_asset()
        .map(|asset| format!(" {} ", asset.original_filename))
        .unwrap_or_default();
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(0x2A, 0x2D, 0x32)))
            .title(Span::styled(title, Style::default().fg(ACCENT))),
        area,
    );
    if app.preview.is_none() {
        frame.render_widget(
            Paragraph::new("Loading…")
                .style(Style::default().fg(MUTED))
                .alignment(Alignment::Center),
            centred(area, 20, 1),
        );
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
                    .border_style(Style::default().fg(Color::Rgb(0x2A, 0x2D, 0x32)))
                    .title(Span::styled(" albums ", Style::default().fg(ACCENT))),
            )
            .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            .highlight_symbol("› "),
        area,
        &mut state,
    );
}

fn draw_info(frame: &mut Frame, app: &App, area: Rect) {
    let Some(asset) = app.selected_asset() else {
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
                .border_style(Style::default().fg(Color::Rgb(0x2A, 0x2D, 0x32)))
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

fn draw_help(frame: &mut Frame, area: Rect) {
    let rows = [
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
        ("1 2 3 4", "library · favourites · archive · trash"),
        ("g  G", "first · last"),
        ("R", "reload"),
        ("?", "this"),
        ("q", "quit"),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(keys, what)| {
            Line::from(vec![
                Span::styled(format!("  {keys:<18}"), Style::default().fg(ACCENT)),
                Span::styled(*what, Style::default().fg(Color::White)),
            ])
        })
        .collect();

    let area = centred(area, 52, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .title(Span::styled(" keys ", Style::default().fg(ACCENT))),
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
