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
        Mode::Picker | Mode::PickerPath(_) => draw_picker(frame, app),
        Mode::Albums => draw_albums(frame, app, chunks[1]),
        Mode::Help => draw_help(frame, chunks[1], app.help_shows_picker),
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
        Mode::Search(_) | Mode::PickerPath(_) => Style::default().fg(ACCENT),
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
                    .border_style(Style::default().fg(Color::Rgb(0x2A, 0x2D, 0x32)))
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
            .border_style(Style::default().fg(Color::Rgb(0x2A, 0x2D, 0x32))),
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
        ("g  G", "first · last"),
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
