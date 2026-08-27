//! The terminal browser.
//!
//! Photographs are drawn with the Kitty graphics protocol where the terminal has it, and
//! with half-block characters where it does not. Either way the pictures are written
//! straight to the terminal after the layout has been drawn, into the holes the layout
//! left: a picture is not something a cell grid can hold, so the two are composed rather
//! than mixed.

mod app;
mod picker;
mod ui;

use std::io::{stdout, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, queue};
use futures::stream::{FuturesUnordered, StreamExt};
use image::DynamicImage;
use imogen_sdk::{AssetUpdate, AssetVariant, UploadOptions};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::context::Context;
use crate::media;
use crate::tui::app::{Action, App, Mode, Scope};
use crate::tui::picker::Picker;

/// A job the loop is waiting on. Boxed because the loop waits on several different kinds
/// of request at once, and they are only the same type once they are behind a pointer.
type Job<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Loaded> + 'a>>;

/// What the loop is waiting on besides a keypress.
enum Loaded {
    Thumbnail(String, Result<Vec<u8>>),
    Preview(String, Result<Vec<u8>>),
    Page(Result<imogen_sdk::AssetPage>),
    Albums(Result<Vec<imogen_sdk::Album>>),
    /// Boxed: an upload result carries a whole `Asset`, which would otherwise make
    /// every variant of this enum as large as the largest one.
    Uploaded(PathBuf, Box<Result<imogen_sdk::AssetUploadResult>>),
    Filed(Result<u64>),
    /// One asset re-read, to see whether the server has finished with it yet.
    Refreshed(String, Box<Result<imogen_sdk::Asset>>),
    /// A picture decoded from the local filesystem, for the picker's preview pane.
    /// `None` means it could not be decoded, which is recorded so it is not retried.
    LocalPreview(PathBuf, Option<Arc<DynamicImage>>),
}

/// How many files to send at once from the browser. Lower than the command line's six:
/// the same connection is also fetching the thumbnails being looked at.
const UPLOAD_CONCURRENCY: usize = 4;

/// How often to ask the server whether a photograph it was still working on is done. Slow
/// enough to be nothing on a library, quick enough that an upload appears while you are
/// still looking at the place it landed.
const PROCESSING_POLL: Duration = Duration::from_millis(1200);

pub async fn run(ctx: &Context) -> Result<()> {
    if !stdout().is_terminal() {
        bail!(
            "There is no terminal to draw in. Try a command — `imogen ls`, `imogen --help` — \
or run this from a terminal."
        );
    }

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, crossterm::cursor::Hide)?;

    let result = event_loop(ctx).await;

    let mut out = stdout();
    let _ = write!(out, "{}", media::kitty::clear_all());
    let _ = execute!(out, LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = disable_raw_mode();
    result
}

async fn event_loop(ctx: &Context) -> Result<()> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let mut app = App::new();
    let mut keys = key_stream();
    let mut poll = tokio::time::interval(PROCESSING_POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut work: FuturesUnordered<Job<'_>> = FuturesUnordered::new();

    work.push(Box::pin(load_page(ctx, app.to_query(), None)));
    app.loading = true;
    work.push(Box::pin(load_albums(ctx)));

    loop {
        let area = terminal.size()?;
        ui::layout(
            &mut app,
            ratatui::layout::Rect::new(0, 0, area.width, area.height),
        );

        // Ask for the pictures the layout has just decided are on screen.
        for id in app.visible_ids() {
            if !app.thumbnails.contains_key(&id) && app.wanted.insert(id.clone()) {
                work.push(Box::pin(load_bytes(
                    ctx,
                    id,
                    AssetVariant::Thumbnail,
                    false,
                )));
            }
        }
        if app.mode == Mode::Viewer {
            if let Some(asset) = app.selected_asset() {
                let id = asset.id.clone();
                let have = app
                    .preview
                    .as_ref()
                    .map(|(held, _)| held == &id)
                    .unwrap_or(false);
                if !have && app.wanted.insert(format!("preview:{id}")) {
                    work.push(Box::pin(load_bytes(ctx, id, AssetVariant::Preview, true)));
                }
            }
        }
        if app.wants_more() {
            app.loading = true;
            work.push(Box::pin(load_page(ctx, app.to_query(), app.cursor.clone())));
        }
        if let Some(path) = app.picker.as_ref().and_then(wants_preview) {
            if !app.local_previews.contains_key(&path) && app.local_wanted.insert(path.clone()) {
                work.push(Box::pin(load_local_preview(path)));
            }
        }
        while app.upload_inflight < UPLOAD_CONCURRENCY {
            let Some(path) = app.upload_queue.pop_front() else {
                break;
            };
            app.upload_inflight += 1;
            work.push(Box::pin(upload_one(ctx, path)));
        }
        if app.upload_finished() {
            finish_upload(ctx, &mut app, &mut work);
        }

        terminal.draw(|frame| ui::draw(frame, &app))?;
        if app.images_dirty {
            place_images(&app)?;
            app.images_dirty = false;
        }
        if app.should_quit {
            return Ok(());
        }

        tokio::select! {
            key = keys.recv() => match key {
                Some(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    handle_key(ctx, &mut app, key, &mut work).await?;
                }
                Some(Event::Resize(_, _)) => {
                    app.images_dirty = true;
                    terminal.clear()?;
                }
                Some(_) => {}
                None => return Ok(()),
            },
            Some(loaded) = work.next() => absorb(&mut app, loaded),
            // Only while something is actually being processed: an idle library has no
            // reason to be woken up.
            _ = poll.tick(), if !app.pending_on_screen().is_empty() => {
                for id in app.pending_on_screen() {
                    if app.refreshing.insert(id.clone()) {
                        work.push(Box::pin(load_asset(ctx, id)));
                    }
                }
            }
        }
    }
}

/// Keypresses arrive on a channel so the loop can wait on them and on the network at the
/// same time. Reading them blocks, which is why it happens on its own thread.
fn key_stream() -> tokio::sync::mpsc::Receiver<Event> {
    let (sender, receiver) = tokio::sync::mpsc::channel(32);
    std::thread::spawn(move || loop {
        match event::poll(Duration::from_millis(250)) {
            Ok(true) => match event::read() {
                Ok(event) => {
                    if sender.blocking_send(event).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            },
            Ok(false) => {
                if sender.is_closed() {
                    return;
                }
            }
            Err(_) => return,
        }
    });
    receiver
}

fn load_page(
    ctx: &Context,
    mut query: imogen_sdk::AssetQuery,
    cursor: Option<String>,
) -> impl std::future::Future<Output = Loaded> + '_ {
    query.cursor = cursor;
    async move { Loaded::Page(ctx.client.assets.list(&query).await.map_err(Into::into)) }
}

async fn load_asset(ctx: &Context, id: String) -> Loaded {
    let asset = ctx.client.assets.get(&id).await.map_err(Into::into);
    Loaded::Refreshed(id, Box::new(asset))
}

async fn load_albums(ctx: &Context) -> Loaded {
    Loaded::Albums(ctx.client.albums.list().await.map_err(Into::into))
}

/// The file under the picker's cursor, when it is one worth trying to draw.
fn wants_preview(picker: &Picker) -> Option<PathBuf> {
    let entry = picker.current()?;
    if entry.is_dir
        || !crate::tui::picker::is_previewable(&entry.path)
        || entry.size > crate::tui::picker::PREVIEW_SIZE_LIMIT
    {
        return None;
    }
    Some(entry.path.clone())
}

/// Reading and decoding a photograph off disk is slow enough to stutter the interface, so
/// it happens on the blocking pool rather than on the loop that draws.
async fn load_local_preview(path: PathBuf) -> Loaded {
    let decoded = tokio::task::spawn_blocking({
        let path = path.clone();
        move || {
            let bytes = std::fs::read(&path).ok()?;
            crate::media::decode(&bytes).ok()
        }
    })
    .await
    .ok()
    .flatten()
    .map(Arc::new);
    Loaded::LocalPreview(path, decoded)
}

async fn upload_one(ctx: &Context, path: PathBuf) -> Loaded {
    let result = ctx
        .client
        .assets
        .upload(&path, &UploadOptions::default())
        .await
        .map_err(anyhow::Error::from);
    Loaded::Uploaded(path, Box::new(result))
}

async fn fill_album(ctx: &Context, album_id: String, ids: Vec<String>) -> Loaded {
    let mut added = 0u64;
    for chunk in ids.chunks(500) {
        match ctx.client.albums.add_assets(&album_id, chunk).await {
            Ok(result) => added += result.added,
            Err(error) => return Loaded::Filed(Err(error.into())),
        }
    }
    Loaded::Filed(Ok(added))
}

/// Reports the run, files what landed into the album being browsed, and reloads so the new
/// photographs appear where they belong rather than at the end.
fn finish_upload<'a>(ctx: &'a Context, app: &mut App, work: &mut FuturesUnordered<Job<'a>>) {
    let done = app.upload_done;
    let failed = app.upload_failed;
    let ids = std::mem::take(&mut app.uploaded_ids);
    let album = app.album.as_ref().map(|album| album.id.clone());

    app.upload_total = 0;
    app.upload_done = 0;
    app.upload_failed = 0;
    app.note(format!(
        "Uploaded {}{}.",
        crate::output::plural(done, "file"),
        if failed > 0 {
            format!(", {failed} failed")
        } else {
            String::new()
        }
    ));

    if let Some(album) = album {
        if !ids.is_empty() {
            work.push(Box::pin(fill_album(ctx, album, ids)));
        }
    }
    reload(ctx, app, work);
}

async fn load_bytes(ctx: &Context, id: String, variant: AssetVariant, preview: bool) -> Loaded {
    let bytes = ctx
        .client
        .assets
        .bytes(&id, variant)
        .await
        .map_err(anyhow::Error::from);
    if preview {
        Loaded::Preview(id, bytes)
    } else {
        Loaded::Thumbnail(id, bytes)
    }
}

fn absorb(app: &mut App, loaded: Loaded) {
    match loaded {
        Loaded::Thumbnail(id, Ok(bytes)) => {
            if let Ok(image) = media::decode(&bytes) {
                app.thumbnails.insert(id, Arc::new(image));
                app.images_dirty = true;
            }
        }
        // A photograph that will not decode is left without a picture rather than
        // retried: whatever is wrong with it will still be wrong next time.
        Loaded::Thumbnail(_, Err(_)) => {}
        Loaded::Preview(id, Ok(bytes)) => {
            if let Ok(image) = media::decode(&bytes) {
                app.preview = Some((id, Arc::new(image)));
                app.images_dirty = true;
            }
        }
        Loaded::Preview(_, Err(error)) => app.note(format!("Could not load: {error}")),
        Loaded::Page(Ok(page)) => {
            app.loading = false;
            if app.total.is_none() {
                app.total = page.total;
            }
            app.cursor = page.next_cursor;
            app.assets.extend(page.items);
            app.images_dirty = true;
        }
        Loaded::Page(Err(error)) => {
            app.loading = false;
            app.note(format!("Could not load photographs: {error}"));
        }
        Loaded::Albums(Ok(albums)) => app.albums = albums,
        Loaded::Albums(Err(_)) => {}
        Loaded::Uploaded(path, result) => {
            app.upload_inflight = app.upload_inflight.saturating_sub(1);
            match *result {
                Ok(outcome) => {
                    app.upload_done += 1;
                    app.uploaded_ids.push(outcome.asset.id);
                }
                Err(error) => {
                    app.upload_failed += 1;
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    app.note(format!("{name}: {error}"));
                }
            }
        }
        Loaded::Refreshed(id, result) => match *result {
            Ok(asset) => app.apply_refreshed(asset),
            // Leave it marked as still being worked on; the next tick asks again.
            Err(_) => {
                app.refreshing.remove(&id);
            }
        },
        Loaded::LocalPreview(path, image) => {
            app.local_previews.insert(path, image);
            app.images_dirty = true;
        }
        Loaded::Filed(Ok(_)) => {}
        Loaded::Filed(Err(error)) => app.note(format!("Could not fill the album: {error}")),
    }
}

/// Puts the pictures on screen. Everything already placed is removed first: a placement
/// stays where it was put, so a grid that has scrolled would otherwise show both.
fn place_images(app: &App) -> Result<()> {
    let mut out = stdout();
    queue!(out, crossterm::cursor::SavePosition)?;
    write!(out, "{}", media::kitty::clear_all())?;

    if app.mode == Mode::Picker || app.mode == Mode::PickerPath(String::new()) {
        if let Some(image) = app
            .picker
            .as_ref()
            .and_then(wants_preview)
            .and_then(|path| app.local_previews.get(&path))
            .and_then(Option::as_ref)
        {
            let pane = app.picker_preview_area;
            let (cols, rows) = fit(image, pane.width, pane.height);
            let x = pane.x + (pane.width.saturating_sub(cols)) / 2;
            let y = pane.y + (pane.height.saturating_sub(rows)) / 2;
            if let Ok(escape) = media::place_at(image, x, y, cols, rows) {
                write!(out, "{escape}")?;
            }
        }
    } else if app.mode == Mode::Viewer {
        if let (Some((_, image)), Some(tile)) = (&app.preview, app.tiles.first()) {
            let (cols, rows) = fit(image, tile.inner.width, tile.inner.height);
            // Centre it in the pane rather than pinning it to the corner.
            let x = tile.inner.x + (tile.inner.width.saturating_sub(cols)) / 2;
            let y = tile.inner.y + (tile.inner.height.saturating_sub(rows)) / 2;
            if let Ok(escape) = media::place_at(image, x, y, cols, rows) {
                write!(out, "{escape}")?;
            }
        }
    } else {
        for tile in &app.tiles {
            let Some(image) = app.thumbnails.get(&tile.id) else {
                continue;
            };
            let (cols, rows) = fit(image, tile.inner.width, tile.inner.height);
            let x = tile.inner.x + (tile.inner.width.saturating_sub(cols)) / 2;
            let y = tile.inner.y + (tile.inner.height.saturating_sub(rows)) / 2;
            if let Ok(escape) = media::place_at(image, x, y, cols, rows) {
                write!(out, "{escape}")?;
            }
        }
    }

    queue!(out, crossterm::cursor::RestorePosition)?;
    out.flush()?;
    Ok(())
}

/// The largest box of cells with the picture's shape that fits inside the space allowed.
fn fit(image: &DynamicImage, max_cols: u16, max_rows: u16) -> (u16, u16) {
    if max_cols == 0 || max_rows == 0 {
        return (0, 0);
    }
    let (cell_width, cell_height) = media::kitty::cell_size();
    let aspect = image.width() as f64 / image.height().max(1) as f64;
    // Cells are taller than they are wide, so a shape in pixels is a different shape in
    // cells; the ratio between the two is what converts one to the other.
    let cell_ratio = cell_height as f64 / cell_width.max(1) as f64;

    let by_height = ((max_rows as f64 * cell_ratio) * aspect).round() as u16;
    if by_height <= max_cols {
        (by_height.max(1), max_rows)
    } else {
        let rows = ((max_cols as f64 / aspect) / cell_ratio).round() as u16;
        (max_cols, rows.clamp(1, max_rows))
    }
}

async fn handle_key<'a>(
    ctx: &'a Context,
    app: &mut App,
    key: KeyEvent,
    work: &mut FuturesUnordered<Job<'a>>,
) -> Result<()> {
    // A mode that is asking a question owns the keyboard until it has an answer.
    if let Mode::Search(current) = &app.mode {
        let mut input = current.clone();
        match key.code {
            KeyCode::Esc => app.mode = Mode::Grid,
            KeyCode::Enter => {
                app.query = input;
                app.mode = Mode::Grid;
                reload(ctx, app, work);
            }
            KeyCode::Backspace => {
                input.pop();
                app.mode = Mode::Search(input);
            }
            KeyCode::Char(c) => {
                input.push(c);
                app.mode = Mode::Search(input);
            }
            _ => {}
        }
        return Ok(());
    }

    // Typing a path to jump the picker to.
    if let Mode::PickerPath(current) = &app.mode {
        let mut input = current.clone();
        match key.code {
            KeyCode::Esc => app.mode = Mode::Picker,
            KeyCode::Enter => {
                let target = expand(input.trim());
                app.mode = Mode::Picker;
                if let Some(picker) = app.picker.as_mut() {
                    if target.is_dir() {
                        picker.go_to(&target);
                    } else if let Some(parent) = target.parent().filter(|p| p.is_dir()) {
                        // A file was named: go to its folder and put the cursor on it.
                        picker.go_to(parent);
                        if let Some(index) = picker.entries.iter().position(|e| e.path == target) {
                            picker.cursor = index;
                        }
                    } else {
                        app.note(format!("{} is not there.", target.display()));
                    }
                }
            }
            KeyCode::Backspace => {
                input.pop();
                app.mode = Mode::PickerPath(input);
            }
            KeyCode::Char(c) => {
                input.push(c);
                app.mode = Mode::PickerPath(input);
            }
            _ => {}
        }
        return Ok(());
    }

    if app.mode == Mode::Picker {
        handle_picker_key(app, key);
        return Ok(());
    }

    if let Mode::Confirm { action, .. } = app.mode.clone() {
        app.mode = Mode::Grid;
        if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            match action {
                Action::Trash(ids) => {
                    match ctx.client.assets.trash(&ids).await {
                        Ok(result) => {
                            app.note(format!("{} moved to the trash.", result.count));
                            reload(ctx, app, work);
                        }
                        Err(error) => app.note(format!("Could not trash: {error}")),
                    };
                }
                Action::Restore(ids) => {
                    match ctx.client.assets.restore(&ids).await {
                        Ok(result) => {
                            app.note(format!("{} restored.", result.count));
                            reload(ctx, app, work);
                        }
                        Err(error) => app.note(format!("Could not restore: {error}")),
                    };
                }
            }
        }
        return Ok(());
    }

    if app.mode == Mode::Help {
        // Back to wherever the overlay was opened from, so dismissing it does not also
        // abandon a selection somebody was part way through making.
        app.mode = if app.picker.is_some() {
            Mode::Picker
        } else {
            Mode::Grid
        };
        app.images_dirty = true;
        return Ok(());
    }

    if app.mode == Mode::Albums {
        match key.code {
            KeyCode::Esc | KeyCode::Char('a') | KeyCode::Char('q') => {
                app.mode = Mode::Grid;
                app.images_dirty = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.album_selected =
                    (app.album_selected + 1).min(app.albums.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.album_selected = app.album_selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                app.album = app.albums.get(app.album_selected).cloned();
                app.scope = Scope::Album;
                app.mode = Mode::Grid;
                reload(ctx, app, work);
            }
            _ => {}
        }
        return Ok(());
    }

    app.status = None;
    let columns = app.columns.max(1) as isize;

    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::Char('q') => {
            if app.mode == Mode::Viewer {
                app.mode = Mode::Grid;
                app.images_dirty = true;
            } else {
                app.should_quit = true;
            }
        }
        KeyCode::Esc => {
            if app.mode == Mode::Viewer {
                app.mode = Mode::Grid;
                app.images_dirty = true;
            } else if !app.query.is_empty() || app.album.is_some() {
                app.query.clear();
                app.album = None;
                app.scope = Scope::Library;
                reload(ctx, app, work);
            }
        }
        KeyCode::Char('?') => {
            app.help_shows_picker = false;
            app.mode = Mode::Help;
            app.images_dirty = true;
        }
        KeyCode::Enter => {
            if app.selected_asset().is_some() {
                app.mode = Mode::Viewer;
                app.images_dirty = true;
            }
        }
        KeyCode::Left | KeyCode::Char('h') => app.move_by(-1),
        KeyCode::Right | KeyCode::Char('l') => app.move_by(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_by(-columns),
        KeyCode::Down | KeyCode::Char('j') => app.move_by(columns),
        KeyCode::PageUp => app.move_by(-columns * app.visible_rows() as isize),
        KeyCode::PageDown => app.move_by(columns * app.visible_rows() as isize),
        KeyCode::Char('g') => {
            app.selected = 0;
            app.scroll = 0;
            app.images_dirty = true;
        }
        KeyCode::Char('G') => {
            app.selected = app.assets.len().saturating_sub(1);
            app.keep_selection_visible();
            app.images_dirty = true;
        }
        KeyCode::Char('/') => app.mode = Mode::Search(app.query.clone()),
        KeyCode::Char('i') => {
            app.show_info = !app.show_info;
            app.images_dirty = true;
        }
        KeyCode::Char('a') => {
            app.mode = Mode::Albums;
            app.images_dirty = true;
        }
        KeyCode::Char('u') => {
            if app.uploading() {
                app.note("Still uploading. Wait for this run to finish.");
            } else {
                let start = app.picker_start.clone();
                app.picker = Some(Picker::open(&start));
                app.mode = Mode::Picker;
                app.images_dirty = true;
            }
        }
        KeyCode::Char('R') => reload(ctx, app, work),
        KeyCode::Char('1') => switch(ctx, app, work, Scope::Library),
        KeyCode::Char('2') => switch(ctx, app, work, Scope::Favorites),
        KeyCode::Char('3') => switch(ctx, app, work, Scope::Archived),
        KeyCode::Char('4') => switch(ctx, app, work, Scope::Trash),
        KeyCode::Char('f') => toggle(ctx, app, Favorite).await,
        KeyCode::Char('e') => toggle(ctx, app, Archive).await,
        KeyCode::Char('d') => {
            if let Some(asset) = app.selected_asset() {
                app.mode = Mode::Confirm {
                    prompt: format!(
                        "Move “{}” to the trash?",
                        crate::output::truncate(&asset.original_filename, 40)
                    ),
                    action: Action::Trash(vec![asset.id.clone()]),
                };
            }
        }
        KeyCode::Char('r') => {
            if let Some(asset) = app.selected_asset() {
                if asset.deleted_at.is_some() {
                    app.mode = Mode::Confirm {
                        prompt: "Restore it from the trash?".into(),
                        action: Action::Restore(vec![asset.id.clone()]),
                    };
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_picker_key(app: &mut App, key: KeyEvent) {
    let Some(picker) = app.picker.as_mut() else {
        app.mode = Mode::Grid;
        return;
    };

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.picker_start = picker.cwd.clone();
            app.picker = None;
            app.mode = Mode::Grid;
            app.images_dirty = true;
        }
        KeyCode::Up | KeyCode::Char('k') => picker.move_by(-1),
        KeyCode::Down | KeyCode::Char('j') => picker.move_by(1),
        KeyCode::PageUp => picker.move_by(-10),
        KeyCode::PageDown => picker.move_by(10),
        KeyCode::Home | KeyCode::Char('g') => picker.cursor = 0,
        KeyCode::End | KeyCode::Char('G') => picker.cursor = picker.entries.len().saturating_sub(1),
        KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => picker.ascend(),
        // Enter opens a folder and ticks a file. It never uploads, so nothing is sent by
        // pressing return one time too many.
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
            if !picker.descend() {
                picker.toggle();
                picker.move_by(1);
            }
        }
        KeyCode::Char(' ') => {
            picker.toggle();
            picker.move_by(1);
        }
        KeyCode::Char('a') => picker.choose_all_media(),
        KeyCode::Char('A') => picker.chosen.clear(),
        KeyCode::Char('.') => picker.toggle_hidden(),
        KeyCode::Char('~') => {
            if let Some(home) = dirs::home_dir() {
                picker.go_to(&home);
            }
        }
        KeyCode::Char('/') => app.mode = Mode::PickerPath(String::new()),
        KeyCode::Char('?') => {
            app.help_shows_picker = true;
            app.mode = Mode::Help;
        }
        KeyCode::Char('u') => {
            let paths = picker.to_upload();
            app.picker_start = picker.cwd.clone();
            app.picker = None;
            app.mode = Mode::Grid;
            app.images_dirty = true;
            queue_upload(app, &paths);
        }
        _ => {}
    }
    app.images_dirty = true;
}

/// Turns a set of chosen paths into a queue of files. A folder is walked; a single file is taken
/// at its word, so a photograph with an extension imogen does not usually look for can
/// still be sent by naming it.
fn queue_upload(app: &mut App, paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }
    let files = match crate::commands::upload::collect(paths, true) {
        Ok(files) => files,
        Err(error) => {
            app.note(error.to_string());
            return;
        }
    };
    if files.is_empty() {
        app.note("Nothing to upload in that.");
        return;
    }

    app.upload_total = files.len();
    app.upload_done = 0;
    app.upload_failed = 0;
    app.uploaded_ids.clear();
    app.upload_queue = files.into_iter().collect();
    app.note(format!(
        "Uploading {}…",
        crate::output::plural(app.upload_total, "file")
    ));
}

/// `~` is the shell's, not the filesystem's, and there is no shell here to expand it.
fn expand(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    if input == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(input)
}

fn reload<'a>(ctx: &'a Context, app: &mut App, work: &mut FuturesUnordered<Job<'a>>) {
    app.reset_results();
    app.total = None;
    app.wanted.clear();
    app.loading = true;
    work.push(Box::pin(load_page(ctx, app.to_query(), None)));
}

fn switch<'a>(ctx: &'a Context, app: &mut App, work: &mut FuturesUnordered<Job<'a>>, scope: Scope) {
    app.scope = scope;
    app.album = None;
    reload(ctx, app, work);
}

struct Favorite;
struct Archive;

trait Toggle {
    fn patch(&self, asset: &imogen_sdk::Asset) -> (AssetUpdate, String);
}

impl Toggle for Favorite {
    fn patch(&self, asset: &imogen_sdk::Asset) -> (AssetUpdate, String) {
        let next = !asset.favorite;
        (
            AssetUpdate {
                favorite: Some(next),
                ..Default::default()
            },
            if next {
                "Favourited.".into()
            } else {
                "No longer a favourite.".into()
            },
        )
    }
}

impl Toggle for Archive {
    fn patch(&self, asset: &imogen_sdk::Asset) -> (AssetUpdate, String) {
        let next = !asset.archived;
        (
            AssetUpdate {
                archived: Some(next),
                ..Default::default()
            },
            if next {
                "Archived.".into()
            } else {
                "Back on the timeline.".into()
            },
        )
    }
}

async fn toggle(ctx: &Context, app: &mut App, which: impl Toggle) {
    let Some(asset) = app.selected_asset().cloned() else {
        return;
    };
    let (patch, message) = which.patch(&asset);
    match ctx.client.assets.update(&asset.id, &patch).await {
        Ok(updated) => {
            if let Some(slot) = app.assets.iter_mut().find(|a| a.id == updated.id) {
                *slot = updated;
            }
            app.note(message);
        }
        Err(error) => app.note(format!("Could not change it: {error}")),
    }
}
