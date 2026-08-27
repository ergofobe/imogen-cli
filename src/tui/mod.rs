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
use imogen_sdk::{AssetUpdate, AssetVariant, TimelineBucketQuery, TimelineQuery, UploadOptions};
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
    /// The shape of the whole library: one entry a day. Small enough to hold for twenty
    /// years, which is the whole point of it.
    Spine(Result<Vec<imogen_sdk::TimelineBucket>>),
    /// One `YYYY-MM` of tiles, named so a slow answer is filed under the period it was
    /// asked for rather than under wherever the viewport has since moved.
    Bucket(String, Result<imogen_sdk::TilePage>),
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

    work.push(Box::pin(load_spine(ctx, spine_query(&app))));
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
        if let Some(id) = viewer_preview_wanted(&mut app) {
            work.push(Box::pin(load_bytes(ctx, id, AssetVariant::Preview, true)));
        }
        // Which periods the viewport covers, rather than how close the cursor is to the
        // end of what has been paged in. Moving the viewport is the only thing that
        // changes the answer, so the window is only rebuilt when it does.
        let periods = app.periods_for_viewport();
        if periods != app.held {
            app.held = periods.clone();
            app.forget_periods_outside(&periods);
            app.rebuild_window();
            app.images_dirty = true;
        }
        for period in &periods {
            if let Some(cursor) = app.period_wanted(period) {
                app.period_inflight.insert(period.clone());
                work.push(Box::pin(load_bucket(
                    ctx,
                    bucket_query(period, &app, cursor),
                )));
            }
        }
        if let Some(id) = detail_wanted(&mut app) {
            work.push(Box::pin(load_asset(ctx, id)));
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

/// The whole timeline's shape, under the current scope's filter.
fn spine_query(app: &App) -> TimelineQuery {
    TimelineQuery {
        covers: None,
        filter: app.to_filter(),
    }
}

/// One period of tiles, under the same filter the spine was counted with. A window that
/// fetched a different filter from the one it was sized by would show the wrong
/// photographs at the right indices, with nothing on screen to say so.
fn bucket_query(period: &str, app: &App, cursor: Option<String>) -> TimelineBucketQuery {
    TimelineBucketQuery {
        period: period.to_string(),
        cursor,
        // Left unset so the server's own default applies rather than a client-side guess.
        limit: None,
        filter: app.to_filter(),
    }
}

async fn load_spine(ctx: &Context, query: TimelineQuery) -> Loaded {
    Loaded::Spine(
        ctx.client
            .assets
            .timeline(&query)
            .await
            .map(|timeline| timeline.buckets)
            .map_err(Into::into),
    )
}

async fn load_bucket(ctx: &Context, query: TimelineBucketQuery) -> Loaded {
    let period = query.period.clone();
    let page = ctx
        .client
        .assets
        .timeline_bucket(&query)
        .await
        .map_err(Into::into);
    Loaded::Bucket(period, page)
}

async fn load_asset(ctx: &Context, id: String) -> Loaded {
    let asset = ctx.client.assets.get(&id).await.map_err(Into::into);
    Loaded::Refreshed(id, Box::new(asset))
}

async fn load_albums(ctx: &Context) -> Loaded {
    Loaded::Albums(ctx.client.albums.list().await.map_err(Into::into))
}

/// The preview the viewer needs: the selected photograph's, when it is not already held
/// and not already on its way. Marks it as on its way, so a caller asks exactly once.
fn viewer_preview_wanted(app: &mut App) -> Option<String> {
    if app.mode != Mode::Viewer {
        return None;
    }
    let id = app.selected_id()?;
    if app.preview_for(&id).is_some() {
        return None;
    }
    if !app.preview_inflight.insert(id.clone()) {
        return None;
    }
    Some(id)
}

/// The whole record the viewer's title and the details panel need, when what is held is
/// not the photograph being looked at. A tile deliberately carries only what the grid
/// draws, so the exif, the place and the filename are read once, on the one being looked
/// at, rather than for every tile that scrolls past.
fn detail_wanted(app: &mut App) -> Option<String> {
    if app.mode != Mode::Viewer && !app.show_info {
        return None;
    }
    let id = app.selected_id()?;
    if app.detail().is_some() || app.detail_asked.as_deref() == Some(id.as_str()) {
        return None;
    }
    app.detail_asked = Some(id.clone());
    app.refreshing.insert(id.clone());
    Some(id)
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
        match ctx
            .client
            .albums
            .add_assets(&album_id, &imogen_sdk::AssetSelection::ids(chunk))
            .await
        {
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
                app.thumbnails.insert(id.clone(), Arc::new(image));
                app.remember_thumbnail(id);
                app.images_dirty = true;
            }
        }
        // A photograph that will not decode is left without a picture rather than
        // retried: whatever is wrong with it will still be wrong next time.
        Loaded::Thumbnail(_, Err(_)) => {}
        Loaded::Preview(id, Ok(bytes)) => {
            app.preview_inflight.remove(&id);
            match media::decode(&bytes) {
                Ok(image) => {
                    app.remember_preview(id, Arc::new(image));
                    app.images_dirty = true;
                }
                // Saying so beats a "Loading…" that never finishes.
                Err(error) => app.note(format!("Could not draw it: {error}")),
            }
        }
        Loaded::Preview(id, Err(error)) => {
            app.preview_inflight.remove(&id);
            app.note(format!("Could not load: {error}"));
        }
        Loaded::Spine(Ok(buckets)) => {
            app.loading = false;
            app.buckets = buckets;
            app.recount();
            // The window was sized against the old spine; whatever is held may now sit at
            // different indices, so it is laid out again before anything draws.
            app.held.clear();
            app.rebuild_window();
            app.images_dirty = true;
        }
        Loaded::Spine(Err(error)) => {
            app.loading = false;
            app.note(format!("Could not load the timeline: {error}"));
        }
        Loaded::Bucket(period, Ok(page)) => {
            app.period_inflight.remove(&period);
            match page.next_cursor {
                Some(cursor) => {
                    app.period_more.insert(period.clone(), cursor);
                }
                None => {
                    app.period_more.remove(&period);
                }
            }
            app.periods.entry(period).or_default().extend(page.items);
            app.rebuild_window();
            app.images_dirty = true;
        }
        Loaded::Bucket(period, Err(error)) => {
            app.period_inflight.remove(&period);
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
        // Matched against the asset on screen: a preview that arrives after you have moved
        // on belongs to the photograph it was asked for, not to whichever one is showing.
        if let (Some(id), Some(tile)) = (app.selected_id(), app.tiles.first()) {
            if let Some(image) = app.preview_for(&id) {
                let (cols, rows) = fit(image, tile.inner.width, tile.inner.height);
                // Centre it in the pane rather than pinning it to the corner.
                let x = tile.inner.x + (tile.inner.width.saturating_sub(cols)) / 2;
                let y = tile.inner.y + (tile.inner.height.saturating_sub(rows)) / 2;
                if let Ok(escape) = media::place_at(image, x, y, cols, rows) {
                    write!(out, "{escape}")?;
                }
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

    // Typing a date to jump the timeline to.
    if let Mode::JumpDate(current) = &app.mode {
        let mut input = current.clone();
        match key.code {
            KeyCode::Esc => app.mode = Mode::Grid,
            KeyCode::Enter => {
                app.mode = Mode::Grid;
                jump_to(app, &input);
            }
            KeyCode::Backspace => {
                input.pop();
                app.mode = Mode::JumpDate(input);
            }
            KeyCode::Char(c) => {
                input.push(c);
                app.mode = Mode::JumpDate(input);
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
                    match ctx
                        .client
                        .assets
                        .trash(&imogen_sdk::AssetSelection::ids(&ids))
                        .await
                    {
                        Ok(result) => {
                            app.note(format!("{} moved to the trash.", result.count));
                            reload(ctx, app, work);
                        }
                        Err(error) => app.note(format!("Could not trash: {error}")),
                    };
                }
                Action::Restore(ids) => {
                    match ctx
                        .client
                        .assets
                        .restore(&imogen_sdk::AssetSelection::ids(&ids))
                        .await
                    {
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
            if app.selected_tile().is_some() {
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
        // `g` used to mean "the top", which on a twenty-year library is the one place you
        // can already reach. It now asks where you want to be; Home still means the top.
        KeyCode::Char('g') => app.mode = Mode::JumpDate(String::new()),
        KeyCode::Home => app.go_to(0),
        KeyCode::End | KeyCode::Char('G') => app.go_to(app.total.saturating_sub(1)),
        // Whole years, through the years the library has rather than through the calendar.
        KeyCode::Char('[') => app.step_year(1),
        KeyCode::Char(']') => app.step_year(-1),
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
            if let Some(id) = app.selected_id() {
                // Named where the name is known, dated otherwise: a tile does not carry a
                // filename, and a prompt about something unnamed is worse than one about
                // a day.
                let what = match app.detail() {
                    Some(asset) => format!(
                        "“{}”",
                        crate::output::truncate(&asset.original_filename, 40)
                    ),
                    None => match app.date_at_index(app.selected) {
                        Some(date) => format!("the photograph from {date}"),
                        None => "it".to_string(),
                    },
                };
                app.mode = Mode::Confirm {
                    prompt: format!("Move {what} to the trash?"),
                    action: Action::Trash(vec![id]),
                };
            }
        }
        // The trash is a place, not a flag, and being in it is what the scope says.
        KeyCode::Char('r') if app.scope == Scope::Trash => {
            if let Some(id) = app.selected_id() {
                app.mode = Mode::Confirm {
                    prompt: "Restore it from the trash?".into(),
                    action: Action::Restore(vec![id]),
                };
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
    app.wanted.clear();
    app.loading = true;
    work.push(Box::pin(load_spine(ctx, spine_query(app))));
}

/// Puts the cursor on the day somebody typed, or on the nearest day that has photographs
/// when that day has none — and says so when it has landed somewhere else, because a
/// silent landing looks like a jump that did not work.
fn jump_to(app: &mut App, input: &str) {
    let Some(day) = crate::dates::to_day(input) else {
        app.note(format!(
            "“{}” is not a date. Try 2011, aug 2011, or 2011-08-14.",
            input.trim()
        ));
        return;
    };
    let Some(index) = app.index_for_date(&day.date) else {
        app.note("There is nothing here to jump to.");
        return;
    };
    app.go_to(index);

    // Only a landing outside what was actually asked for is a surprise. Somebody who
    // typed "march 2000" and arrived on the second of March asked for that.
    let Some(landed) = app.date_at_index(index) else {
        return;
    };
    if landed.get(..day.named) != day.date.get(..day.named) {
        app.note(format!("Nothing on {}. This is {landed}.", day.date));
    }
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
    let Some(id) = app.selected_id() else {
        return;
    };
    // A patch that inverts a flag needs to know what the flag is, and a tile does not
    // carry all of them. Read the record first when it is not already held — one request,
    // on a key somebody pressed deliberately.
    let asset = match app.detail().cloned() {
        Some(held) => held,
        None => match ctx.client.assets.get(&id).await {
            Ok(asset) => asset,
            Err(error) => {
                app.note(format!("Could not read it: {error}"));
                return;
            }
        },
    };
    let (patch, message) = which.patch(&asset);
    match ctx.client.assets.update(&id, &patch).await {
        Ok(updated) => {
            app.apply_refreshed(updated);
            app.note(message);
        }
        Err(error) => app.note(format!("Could not change it: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{Tile, THUMBNAIL_CACHE};
    use imogen_sdk::{AssetStatus, AssetType, TimelineBucket, TimelineTile};
    use ratatui::layout::Rect;

    fn png_bytes() -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4));
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn test_image() -> DynamicImage {
        DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2))
    }

    fn bucket(date: &str, count: u64) -> TimelineBucket {
        TimelineBucket {
            date: date.into(),
            count,
            cover_asset_id: None,
        }
    }

    fn tile(id: &str, captured_at: &str) -> TimelineTile {
        TimelineTile {
            id: id.into(),
            captured_at: captured_at.into(),
            width: None,
            height: None,
            r#type: AssetType::Image,
            status: AssetStatus::Ready,
            favorite: false,
            duration: None,
            placeholder_color: None,
            live_photo_video_id: None,
        }
    }

    /// An app as the loop leaves it once one day's bucket has arrived: a spine that knows
    /// the shape of the library, a window of tiles, and a layout over them.
    fn viewing(ids: &[&str]) -> App {
        let mut app = App::new();
        if !ids.is_empty() {
            app.buckets = vec![bucket("2024-06-01", ids.len() as u64)];
            app.recount();
            app.periods.insert(
                "2024-06".into(),
                ids.iter()
                    .map(|id| tile(id, "2024-06-01T09:30:00.000Z"))
                    .collect(),
            );
            app.rebuild_window();
        }
        app.tiles = ids
            .iter()
            .enumerate()
            .map(|(index, id)| Tile {
                id: (*id).into(),
                inner: Rect::default(),
                index,
            })
            .collect();
        app.mode = Mode::Viewer;
        app
    }

    /// A library with an eleven-year hole in the middle, which is what an ordinary
    /// twenty-year library looks like: a burst, a gap, another burst.
    fn library_with_a_gap() -> App {
        let mut app = App::new();
        app.buckets = vec![
            bucket("2011-08-15", 10),
            bucket("2011-08-14", 10),
            bucket("2000-03-02", 10),
            bucket("2000-03-01", 10),
        ];
        app.recount();
        app
    }

    #[test]
    fn thumbnails_are_evicted_once_the_cap_is_reached() {
        let mut app = viewing(&[]);
        app.grid_area = Rect::new(0, 0, 80, 24);
        for n in 0..(THUMBNAIL_CACHE + 10) {
            let id = format!("asset-{n}");
            app.thumbnails.insert(id.clone(), Arc::new(test_image()));
            app.remember_thumbnail(id);
        }
        assert!(app.thumbnails.len() <= THUMBNAIL_CACHE);
        assert!(app.thumb_order.len() <= THUMBNAIL_CACHE);
    }

    /// The other half of eviction. `wanted` is the record of "a request has gone out for
    /// this"; dropping the picture without dropping that record leaves a tile that can
    /// never be filled again, however far back you scroll.
    #[test]
    fn an_evicted_thumbnail_is_asked_for_again() {
        let mut app = viewing(&[]);
        for n in 0..(THUMBNAIL_CACHE + 10) {
            let id = format!("asset-{n}");
            app.wanted.insert(id.clone());
            app.thumbnails.insert(id.clone(), Arc::new(test_image()));
            app.remember_thumbnail(id);
        }
        assert!(!app.thumbnails.contains_key("asset-0"), "evicted");
        assert!(
            !app.wanted.contains("asset-0"),
            "and so must be asked for again rather than left blank for ever"
        );
    }

    #[test]
    fn the_thumbnail_on_screen_is_never_the_one_evicted() {
        // "keep" is both the oldest thing in the cache and the one the grid is drawing.
        let mut app = viewing(&["keep"]);
        app.thumbnails.insert("keep".into(), Arc::new(test_image()));
        app.remember_thumbnail("keep".into());
        for n in 0..(THUMBNAIL_CACHE + 10) {
            let id = format!("asset-{n}");
            app.thumbnails.insert(id.clone(), Arc::new(test_image()));
            app.remember_thumbnail(id);
        }
        assert!(
            app.thumbnails.contains_key("keep"),
            "the picture on screen must survive its own eviction"
        );
    }

    #[test]
    fn jumping_to_a_date_lands_on_it() {
        let mut app = viewing(&[]);
        app.buckets = vec![bucket("2011-08-15", 10), bucket("2011-08-14", 10)];
        app.recount();
        assert_eq!(app.index_for_date("2011-08-14"), Some(10));
    }

    #[test]
    fn jumping_to_a_day_with_no_photographs_lands_on_the_nearest_older_one() {
        let mut app = viewing(&[]);
        app.buckets = vec![bucket("2011-08-15", 10), bucket("2011-08-10", 10)];
        app.recount();
        assert_eq!(app.index_for_date("2011-08-12"), Some(10));
    }

    /// The bug the web shipped: the search for the nearest day gave up after a bounded
    /// number of steps and threw the reader at the end of the library. A gap of eleven
    /// years must cost exactly what a gap of two days costs.
    #[test]
    fn a_gap_of_years_costs_no_more_than_a_gap_of_days() {
        let app = library_with_a_gap();
        // Into the hole: the nearest day with photographs, going older, is 2000-03-02.
        assert_eq!(app.index_for_date("2006-06-15"), Some(20));
        // And not the end of the library, which is where giving up would land.
        assert_ne!(app.index_for_date("2006-06-15"), Some(app.total - 1));
    }

    #[test]
    fn jumping_ahead_of_the_library_lands_on_the_newest_photograph() {
        let app = library_with_a_gap();
        assert_eq!(app.index_for_date("2030-01-01"), Some(0));
    }

    #[test]
    fn jumping_past_the_end_lands_on_the_oldest_photograph() {
        let mut app = viewing(&[]);
        app.buckets = vec![bucket("2011-08-15", 10)];
        app.recount();
        assert_eq!(app.index_for_date("1999-01-01"), Some(9));
    }

    #[test]
    fn an_empty_library_has_nowhere_to_jump_to() {
        let app = viewing(&[]);
        assert_eq!(app.index_for_date("2011-08-14"), None);
    }

    #[test]
    fn the_day_at_an_index_is_the_day_the_index_was_found_for() {
        let app = library_with_a_gap();
        for (index, expected) in [
            (0, "2011-08-15"),
            (9, "2011-08-15"),
            (10, "2011-08-14"),
            (20, "2000-03-02"),
            (39, "2000-03-01"),
        ] {
            assert_eq!(app.date_at_index(index).as_deref(), Some(expected));
        }
        assert_eq!(app.date_at_index(40), None, "past the end of the library");
    }

    /// Stepping through the years the library has, not through the calendar: one press
    /// crosses the whole hole.
    #[test]
    fn stepping_a_year_crosses_a_gap_of_years_in_one_press() {
        let mut app = library_with_a_gap();
        app.step_year(1);
        assert_eq!(
            app.date_at_index(app.selected).as_deref(),
            Some("2000-03-02")
        );
        app.step_year(-1);
        assert_eq!(
            app.date_at_index(app.selected).as_deref(),
            Some("2011-08-15")
        );
        // And neither end runs off.
        app.step_year(-1);
        assert_eq!(app.selected, 0);
        app.step_year(1);
        app.step_year(1);
        assert_eq!(
            app.date_at_index(app.selected).as_deref(),
            Some("2000-03-02")
        );
    }

    #[test]
    fn the_viewport_asks_only_for_the_periods_it_covers() {
        let mut app = viewing(&[]);
        app.buckets = vec![
            bucket("2011-09-01", 200),
            bucket("2011-08-14", 200),
            bucket("2011-07-01", 200),
        ];
        app.recount();
        app.columns = 4;
        app.grid_area = Rect::new(0, 0, 80, 24);
        app.selected = 0;
        let periods = app.periods_for_viewport();
        assert!(periods.contains(&"2011-09".to_string()));
        assert!(!periods.contains(&"2011-07".to_string()));
    }

    /// Nothing here may wait on a drawn frame to become useful. The web shipped a grid
    /// that refused to render until it had been measured, so there was never anything to
    /// measure; an unmeasured App must still be able to say what to fetch.
    #[test]
    fn the_periods_wanted_are_known_before_anything_has_been_measured() {
        let mut app = App::new();
        app.buckets = vec![bucket("2011-09-01", 200)];
        app.recount();
        assert_eq!(app.grid_area, Rect::default(), "not yet laid out");
        assert!(
            app.periods_for_viewport().contains(&"2011-09".to_string()),
            "the first fetch cannot wait for a frame that needs it to have happened"
        );
    }

    /// The trash, the archive and the favourites are separate places, and a windowed
    /// fetch carrying the wrong filter would show the wrong photographs with nothing on
    /// screen to say so.
    #[test]
    fn each_scope_asks_for_its_own_photographs() {
        let mut app = App::new();
        assert_eq!(app.to_filter(), imogen_sdk::AssetFilter::default());

        app.scope = Scope::Trash;
        assert_eq!(app.to_filter().trashed, Some(true));
        assert_eq!(app.to_filter().archived, None);

        app.scope = Scope::Archived;
        assert_eq!(app.to_filter().archived, Some(true));
        assert_eq!(app.to_filter().trashed, None);

        app.scope = Scope::Favorites;
        assert_eq!(app.to_filter().favorite, Some(true));

        app.scope = Scope::Library;
        app.query = "beach".into();
        assert_eq!(app.to_filter().q.as_deref(), Some("beach"));
    }

    /// The same filter reaches the spine and every bucket, so what is counted and what is
    /// shown cannot disagree about which place you are in.
    #[test]
    fn the_spine_and_the_bucket_are_filtered_the_same_way() {
        let mut app = App::new();
        app.scope = Scope::Trash;
        assert_eq!(
            bucket_query("2011-08", &app, None).filter,
            spine_query(&app).filter
        );
        assert_eq!(
            bucket_query("2011-08", &app, None).filter.trashed,
            Some(true)
        );
    }

    #[test]
    fn the_window_answers_for_the_indices_it_holds_and_no_others() {
        let mut app = App::new();
        app.buckets = vec![bucket("2011-09-02", 2), bucket("2011-08-14", 2)];
        app.recount();
        app.periods
            .insert("2011-08".into(), vec![tile("c", "x"), tile("d", "x")]);
        app.rebuild_window();

        assert_eq!(app.window.base, 2);
        assert!(app.window.get(1).is_none(), "not held");
        assert_eq!(app.window.get(2).map(|t| t.id.as_str()), Some("c"));
        assert_eq!(app.window.get(3).map(|t| t.id.as_str()), Some("d"));
        assert!(app.window.get(4).is_none(), "past the end");
    }

    /// Two periods with a third still on its way are not adjacent, and joining them would
    /// silently put July's photographs at August's indices.
    #[test]
    fn a_hole_in_what_has_arrived_does_not_join_tiles_across_it() {
        let mut app = App::new();
        app.buckets = vec![
            bucket("2011-09-01", 1),
            bucket("2011-08-14", 1),
            bucket("2011-07-01", 1),
        ];
        app.recount();
        app.periods.insert("2011-09".into(), vec![tile("sep", "x")]);
        app.periods.insert("2011-07".into(), vec![tile("jul", "x")]);
        app.rebuild_window();

        assert_eq!(app.window.get(0).map(|t| t.id.as_str()), Some("sep"));
        assert!(
            app.window.get(1).is_none(),
            "August has not arrived, so index 1 is nobody's"
        );
        assert_ne!(app.window.get(2).map(|t| t.id.as_str()), Some("jul"));
    }

    #[test]
    fn a_month_by_name_is_a_date_somebody_may_type() {
        let day = |input| crate::dates::to_day(input).map(|day| day.date);
        assert_eq!(day("aug 2011").as_deref(), Some("2011-08-31"));
        assert_eq!(day("August 2011").as_deref(), Some("2011-08-31"));
        assert_eq!(day("2011").as_deref(), Some("2011-12-31"));
        assert_eq!(day("2011-08-14").as_deref(), Some("2011-08-14"));
        assert_eq!(day("2011-08").as_deref(), Some("2011-08-31"));
        assert_eq!(day("not a date"), None);
    }

    /// Typed, jumped, landed — through the same path the key handler takes.
    #[test]
    fn typing_a_month_lands_in_that_month() {
        let mut app = library_with_a_gap();
        jump_to(&mut app, "march 2000");
        assert_eq!(
            app.date_at_index(app.selected).as_deref(),
            Some("2000-03-02")
        );
        assert!(
            app.status.is_none(),
            "it landed on the day it was asked for"
        );

        jump_to(&mut app, "june 2006");
        assert_eq!(
            app.date_at_index(app.selected).as_deref(),
            Some("2000-03-02")
        );
        assert!(
            app.status.is_some(),
            "and says so when it lands somewhere else"
        );
    }

    /// The year rail is a map of the whole library, and two labels in one place is worse
    /// than one.
    #[test]
    fn the_year_rail_never_stacks_two_years_on_one_row() {
        let mut app = App::new();
        app.buckets = (0..20)
            .map(|n| bucket(&format!("{}-06-01", 2024 - n), 100))
            .collect();
        app.recount();
        let marks = app.year_marks(8);
        let rows: Vec<u16> = marks.iter().map(|(row, _)| *row).collect();
        let mut sorted = rows.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(rows.len(), sorted.len(), "{marks:?}");
        assert!(marks.iter().all(|(row, _)| *row < 8));
        assert_eq!(marks.first().map(|(_, year)| year.as_str()), Some("2024"));
    }

    /// One pass of the event loop's window management, without the network: settle the
    /// window on where the viewport is, and answer every period it asks for.
    fn pass(app: &mut App, served: &mut usize) {
        let periods = app.periods_for_viewport();
        if periods != app.held {
            app.held = periods.clone();
            app.forget_periods_outside(&periods);
            app.rebuild_window();
        }
        for period in &periods {
            if app.period_wanted(period).is_none() {
                continue;
            }
            let start = app.period_start(period).unwrap();
            let count = app
                .buckets
                .iter()
                .filter(|bucket| &bucket.date[..7] == period)
                .map(|bucket| bucket.count as usize)
                .sum::<usize>();
            *served += 1;
            absorb(
                app,
                Loaded::Bucket(
                    period.clone(),
                    Ok(imogen_sdk::TilePage {
                        items: (start..start + count)
                            .map(|n| tile(&format!("a{n}"), "2011-01-01T00:00:00.000Z"))
                            .collect(),
                        next_cursor: None,
                        total: None,
                    }),
                ),
            );
        }
    }

    /// A library of twenty years, one month at a time.
    fn twenty_years() -> App {
        let mut app = App::new();
        app.buckets = (0..240)
            .map(|n| bucket(&format!("{}-{:02}-01", 2024 - n / 12, 12 - n % 12), 40))
            .collect();
        app.recount();
        app.columns = 4;
        app.grid_area = Rect::new(0, 0, 80, 27);
        app
    }

    /// The other half of the leak. Tiles were the `Vec<Asset>` that grew with every page
    /// fetched and was never trimmed; scrolling the length of a twenty-year library must
    /// now cost a fixed amount of memory, not a rising one.
    #[test]
    fn scrolling_the_whole_library_does_not_accumulate_tiles() {
        let mut app = twenty_years();
        let mut served = 0usize;
        let mut worst = 0usize;

        while app.selected + 1 < app.total {
            pass(&mut app, &mut served);
            worst = worst.max(app.periods.values().map(Vec::len).sum::<usize>());
            app.move_by(app.columns as isize * 3);
        }

        assert_eq!(app.total, 9600, "the whole library was walked");
        // Five periods of forty is the most the viewport plus one either side can cover.
        assert!(worst <= 5 * 40, "held {worst} tiles at once");
        assert!(
            app.window.tiles.len() <= 5 * 40,
            "the window is the viewport's, not the library's"
        );
        assert!(
            served > 200,
            "and every period was really fetched: {served}"
        );
    }

    /// Scrolling back does refetch — that is the trade the bound buys — but the window
    /// must land on the same tiles, not on tiles shifted by whatever was dropped.
    #[test]
    fn scrolling_back_lands_on_the_same_photographs() {
        let mut app = twenty_years();
        let mut served = 0usize;
        pass(&mut app, &mut served);
        let first = app.window.get(0).map(|tile| tile.id.clone());

        app.go_to(5000);
        pass(&mut app, &mut served);
        app.go_to(0);
        pass(&mut app, &mut served);

        assert_eq!(app.window.get(0).map(|tile| tile.id.clone()), first);
        assert_eq!(app.window.base, 0);
    }

    /// The spine arriving is what makes the library reachable at all: before it, nothing
    /// knows there is a 2009 to ask for.
    #[test]
    fn the_spine_is_what_makes_the_far_end_reachable() {
        let mut app = App::new();
        app.columns = 4;
        app.grid_area = Rect::new(0, 0, 80, 27);
        assert!(app.periods_for_viewport().is_empty(), "nothing known yet");

        absorb(
            &mut app,
            Loaded::Spine(Ok(vec![bucket("2024-06-01", 40), bucket("2009-03-01", 40)])),
        );
        assert_eq!(app.total, 80);
        assert!(!app.loading);
        // And 2009 is one jump away, with nothing paged through to get there.
        assert_eq!(app.index_for_date("2009-03-01"), Some(40));
    }

    /// A reload swaps the spine underneath tiles that were held against the old one.
    /// Their indices are not the same indices any more.
    #[test]
    fn a_new_spine_relays_the_window_rather_than_trusting_the_old_indices() {
        let mut app = App::new();
        app.buckets = vec![bucket("2024-06-01", 40)];
        app.recount();
        app.periods
            .insert("2024-06".into(), vec![tile("a", "x"), tile("b", "x")]);
        app.rebuild_window();
        assert_eq!(app.window.base, 0);

        // Something older arrived, so June is no longer at the top of the library.
        absorb(
            &mut app,
            Loaded::Spine(Ok(vec![bucket("2024-07-01", 10), bucket("2024-06-01", 40)])),
        );
        assert_eq!(
            app.window.base, 10,
            "June's tiles moved down by all of July"
        );
    }

    /// A record is read once for the photograph being looked at, not on every pass of the
    /// loop. A fetch that fails must not become a busy loop against a broken network.
    #[test]
    fn the_record_of_one_photograph_is_asked_for_once() {
        let mut app = viewing(&["a", "b"]);
        assert_eq!(detail_wanted(&mut app).as_deref(), Some("a"));
        assert_eq!(detail_wanted(&mut app), None, "not asked for twice");

        // The request fails. Still not asked for again, however many passes go by.
        absorb(
            &mut app,
            Loaded::Refreshed("a".into(), Box::new(Err(anyhow::anyhow!("no network")))),
        );
        assert_eq!(detail_wanted(&mut app), None);
        assert_eq!(detail_wanted(&mut app), None);

        // Moving on asks for the one you moved on to.
        app.move_by(1);
        assert_eq!(detail_wanted(&mut app).as_deref(), Some("b"));
    }

    /// And nothing is read for a grid nobody has asked for details about: that would be a
    /// request per tile scrolled past.
    #[test]
    fn scrolling_the_grid_reads_no_records_at_all() {
        let mut app = viewing(&["a", "b"]);
        app.mode = Mode::Grid;
        assert_eq!(detail_wanted(&mut app), None);
        app.show_info = true;
        assert_eq!(detail_wanted(&mut app).as_deref(), Some("a"));
    }

    /// The reported bug, through the real request decision and the real absorb: view one,
    /// move to the next, come back — and the first must still appear. It did not, because
    /// the note that it had been asked for outlived the picture it was asked for.
    #[test]
    fn coming_back_to_a_photograph_shows_it_again() {
        let mut app = viewing(&["a", "b"]);

        assert_eq!(viewer_preview_wanted(&mut app).as_deref(), Some("a"));
        absorb(&mut app, Loaded::Preview("a".into(), Ok(png_bytes())));
        assert!(app.preview_for("a").is_some());

        app.move_by(1);
        assert_eq!(viewer_preview_wanted(&mut app).as_deref(), Some("b"));
        absorb(&mut app, Loaded::Preview("b".into(), Ok(png_bytes())));

        app.move_by(-1);
        assert!(
            app.preview_for("a").is_some(),
            "still held, so it draws at once"
        );
        assert_eq!(
            viewer_preview_wanted(&mut app),
            None,
            "and is not asked for again"
        );
    }

    /// Stepping through faster than the answers come back must not leave every photograph
    /// marked as asked-for with nothing to show.
    #[test]
    fn stepping_through_quickly_still_ends_with_pictures() {
        let ids = ["a", "b", "c", "d"];
        let mut app = viewing(&ids);

        // Ask for each in turn without waiting, as somebody holding a key down would.
        let mut asked = Vec::new();
        for step in 0..ids.len() {
            if let Some(id) = viewer_preview_wanted(&mut app) {
                asked.push(id);
            }
            if step + 1 < ids.len() {
                app.move_by(1);
            }
        }
        assert_eq!(asked.len(), 4, "each was asked for once");

        // The answers arrive afterwards, in any order.
        for id in ["c", "a", "d", "b"] {
            absorb(&mut app, Loaded::Preview(id.into(), Ok(png_bytes())));
        }
        for id in ids {
            assert!(app.preview_for(id).is_some(), "{id} should be there");
        }
        assert!(
            app.preview_inflight.is_empty(),
            "nothing left marked as pending"
        );
    }

    #[test]
    fn a_refused_request_clears_the_mark_so_it_can_be_retried() {
        let mut app = viewing(&["a"]);
        assert_eq!(viewer_preview_wanted(&mut app).as_deref(), Some("a"));

        absorb(
            &mut app,
            Loaded::Preview("a".into(), Err(anyhow::anyhow!("network went away"))),
        );
        assert!(app.preview_inflight.is_empty());
        assert_eq!(viewer_preview_wanted(&mut app).as_deref(), Some("a"));
    }

    #[test]
    fn bytes_that_will_not_decode_say_so_rather_than_loading_for_ever() {
        let mut app = viewing(&["a"]);
        viewer_preview_wanted(&mut app);
        absorb(
            &mut app,
            Loaded::Preview("a".into(), Ok(b"not an image".to_vec())),
        );

        assert!(app.preview_for("a").is_none());
        assert!(
            app.status.is_some(),
            "the viewer must say something went wrong"
        );
    }

    #[test]
    fn nothing_is_asked_for_outside_the_viewer() {
        let mut app = viewing(&["a"]);
        app.mode = Mode::Grid;
        assert_eq!(viewer_preview_wanted(&mut app), None);
    }
}
