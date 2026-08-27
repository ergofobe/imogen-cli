//! What the terminal browser is looking at.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use image::DynamicImage;
use imogen_sdk::{Album, Asset, AssetFilter, AssetStatus, TimelineBucket, TimelineTile};
use ratatui::layout::Rect;

/// Which set of photographs is on screen. The trash and the archive are not filters
/// somebody stumbles into: each is a deliberate place to go and back out of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Library,
    Favorites,
    Archived,
    Trash,
    Album,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Library => "library",
            Scope::Favorites => "favourites",
            Scope::Archived => "archived",
            Scope::Trash => "trash",
            Scope::Album => "album",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Grid,
    /// One photograph, as large as the window allows.
    Viewer,
    /// Typing a search. The text is applied on Enter and abandoned on Escape.
    Search(String),
    Albums,
    Help,
    /// Walking the filesystem to choose what to upload.
    Picker,
    /// Typing a path to jump the picker to. Escape abandons it.
    PickerPath(String),
    /// Typing a date to jump the timeline to. Applied on Enter, abandoned on Escape.
    JumpDate(String),
    /// Waiting for a yes or a no before doing something that cannot be undone.
    Confirm {
        prompt: String,
        action: Action,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Trash(Vec<String>),
    Restore(Vec<String>),
}

/// One photograph's place on screen, so the picture can be drawn into the hole the layout
/// left for it.
#[derive(Debug, Clone)]
pub struct Tile {
    pub id: String,
    /// The area inside the border, where the picture goes.
    pub inner: Rect,
    pub index: usize,
}

/// How many viewer-sized pictures to keep. Enough that stepping back and forth over a few
/// photographs is instant, few enough that the memory stays bounded.
pub const PREVIEW_CACHE: usize = 8;

/// How many decoded grid thumbnails to keep. Enough that scrolling back a screen or two is
/// instant; few enough that a twenty-year library does not become a memory leak. The map
/// used to grow without bound, and it holds decoded images.
pub const THUMBNAIL_CACHE: usize = 256;

/// The stretch of the timeline actually held in memory: the tiles the viewport covers, and
/// the global index the first of them sits at.
///
/// Everything outside it is described by the day buckets alone, which cost a few dozen
/// bytes a day rather than a decoded photograph apiece. Twenty years of buckets is a list
/// small enough to hold whole; twenty years of tiles is not.
#[derive(Debug, Default)]
pub struct TileWindow {
    pub base: usize,
    pub tiles: Vec<TimelineTile>,
}

impl TileWindow {
    /// The tile at a global index, or `None` when the window does not reach that far.
    pub fn get(&self, index: usize) -> Option<&TimelineTile> {
        self.tiles.get(index.checked_sub(self.base)?)
    }
}

pub struct App {
    /// The shape of the whole library: one entry a day, newest first, as the server
    /// orders it. This is what makes 2009 reachable without paging through 2010.
    pub buckets: Vec<TimelineBucket>,
    /// Every photograph the buckets account for. Recomputed by [`App::recount`].
    pub total: usize,
    /// Tiles keyed by the `YYYY-MM` period they came in, which is the unit the server
    /// serves and so the unit this keeps and drops.
    pub periods: HashMap<String, Vec<TimelineTile>>,
    /// Periods with more tiles still to come, and the cursor to ask for them with. A month
    /// of five thousand photographs arrives in more than one answer.
    pub period_more: HashMap<String, String>,
    /// Periods already asked for, so a slow answer is not asked for twice.
    pub period_inflight: HashSet<String>,
    /// The contiguous run of tiles the grid indexes into, rebuilt as periods arrive.
    pub window: TileWindow,
    /// What the viewport wanted last pass, so the window is only rebuilt when it moves.
    pub held: Vec<String>,
    /// Which set of results is the current one. Every request is stamped with the epoch it
    /// was issued under, and an answer that arrives after a reload carries a stale stamp.
    /// Nothing drains the in-flight requests when the scope changes, so without this a
    /// bucket fetched under the library's filter can be filed under the trash's spine —
    /// the wrong photographs, at plausible indices, with nothing on screen to say so.
    pub epoch: u64,
    /// The full record of the selected photograph — everything a tile deliberately does
    /// not carry. Fetched when the viewer or the details panel needs it, not per keypress.
    pub detail: Option<Asset>,
    /// Whose record was last asked for. A request that fails would otherwise be made again
    /// on the very next pass of the loop, turning a broken network into a busy loop.
    pub detail_asked: Option<String>,
    pub selected: usize,
    /// The first grid row on screen.
    pub scroll: usize,
    pub mode: Mode,
    pub scope: Scope,
    pub query: String,
    pub album: Option<Album>,
    pub albums: Vec<Album>,
    pub album_selected: usize,

    pub thumbnails: HashMap<String, Arc<DynamicImage>>,
    /// Insertion order for the thumbnails, for evicting the least recently wanted.
    pub thumb_order: VecDeque<String>,
    pub wanted: HashSet<String>,
    /// Assets whose status is being re-checked, so the same one is not asked for twice
    /// while an answer is still on its way.
    pub refreshing: HashSet<String>,
    /// Previews for the viewer — a larger rendition than the grid's, so they are kept a
    /// handful at a time rather than for the whole library. Keyed by asset, because a
    /// single slot cannot answer "have I already got this one?" once you have moved on.
    pub previews: HashMap<String, Arc<DynamicImage>>,
    /// Insertion order, for evicting the least recently added.
    pub preview_order: VecDeque<String>,
    /// Previews currently being fetched. Emptied as each answer arrives, so a second look
    /// at the same photograph asks again rather than waiting for a request that is over.
    pub preview_inflight: HashSet<String>,

    /// The file picker, alive only while it is open.
    pub picker: Option<crate::tui::picker::Picker>,
    /// Where the picker last was, so reopening it does not start over at the top.
    pub picker_start: PathBuf,
    /// Locally decoded previews, keyed by path. `None` records a file that cannot be
    /// shown, so it is not decoded again on every keypress.
    pub local_previews: HashMap<PathBuf, Option<Arc<DynamicImage>>>,
    pub local_wanted: HashSet<PathBuf>,
    pub picker_list_area: Rect,
    pub picker_preview_area: Rect,

    pub grid_area: Rect,
    /// The gutter down the right of the grid where the years are drawn. Empty in every
    /// mode that is not the grid.
    pub rail_area: Rect,
    pub tiles: Vec<Tile>,
    pub columns: usize,
    pub tile_width: u16,
    pub tile_height: u16,

    /// Files still to send, and how the run is going. The queue is drained a few at a
    /// time by the event loop rather than all at once, so choosing a folder of three
    /// thousand photographs does not open three thousand connections.
    pub upload_queue: VecDeque<PathBuf>,
    pub upload_inflight: usize,
    pub upload_done: usize,
    pub upload_failed: usize,
    pub upload_total: usize,
    /// What has landed, so it can be filed into the album being browsed once the run ends.
    pub uploaded_ids: Vec<String>,

    /// Whether the keys overlay should describe the picker. Kept separate so neither
    /// list is padded with keys that do nothing where you are — an overlay taller than
    /// the window is worse than a short one.
    pub help_shows_picker: bool,

    pub status: Option<String>,
    pub loading: bool,
    pub images_dirty: bool,
    pub should_quit: bool,
    pub show_info: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            buckets: Vec::new(),
            total: 0,
            periods: HashMap::new(),
            period_more: HashMap::new(),
            period_inflight: HashSet::new(),
            window: TileWindow::default(),
            held: Vec::new(),
            epoch: 0,
            detail: None,
            detail_asked: None,
            selected: 0,
            scroll: 0,
            mode: Mode::Grid,
            scope: Scope::Library,
            query: String::new(),
            album: None,
            albums: Vec::new(),
            album_selected: 0,
            thumbnails: HashMap::new(),
            thumb_order: VecDeque::new(),
            wanted: HashSet::new(),
            refreshing: HashSet::new(),
            previews: HashMap::new(),
            preview_order: VecDeque::new(),
            preview_inflight: HashSet::new(),
            grid_area: Rect::default(),
            rail_area: Rect::default(),
            tiles: Vec::new(),
            columns: 1,
            tile_width: 20,
            tile_height: 9,
            picker: None,
            picker_start: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            local_previews: HashMap::new(),
            local_wanted: HashSet::new(),
            picker_list_area: Rect::default(),
            picker_preview_area: Rect::default(),
            upload_queue: VecDeque::new(),
            upload_inflight: 0,
            upload_done: 0,
            upload_failed: 0,
            upload_total: 0,
            uploaded_ids: Vec::new(),
            help_shows_picker: false,
            status: None,
            loading: false,
            images_dirty: true,
            should_quit: false,
            show_info: false,
        }
    }

    /// The tile under the cursor. A tile carries what the grid draws and nothing else; the
    /// whole record lives in [`App::detail`], and only for the one being looked at.
    pub fn selected_tile(&self) -> Option<&TimelineTile> {
        self.window.get(self.selected)
    }

    pub fn selected_id(&self) -> Option<String> {
        self.selected_tile().map(|tile| tile.id.clone())
    }

    /// The full record of the selected photograph, when the one held is still the one
    /// selected. A detail that belongs to the photograph you have moved on from is worse
    /// than none: it would draw somebody else's filename under this picture.
    pub fn detail(&self) -> Option<&Asset> {
        let held = self.detail.as_ref()?;
        (self.selected_id().as_deref() == Some(held.id.as_str())).then_some(held)
    }

    /// The filter the current scope means. The spine and every windowed fetch go through
    /// this one function, so the trash cannot be counted while the library is shown.
    pub fn to_filter(&self) -> AssetFilter {
        AssetFilter {
            q: (!self.query.is_empty()).then(|| self.query.clone()),
            album_id: self.album.as_ref().map(|album| album.id.clone()),
            favorite: (self.scope == Scope::Favorites).then_some(true),
            archived: (self.scope == Scope::Archived).then_some(true),
            trashed: (self.scope == Scope::Trash).then_some(true),
            ..Default::default()
        }
    }

    /// Forgets the current results without forgetting the pictures already decoded: the
    /// same photograph in a different scope does not need fetching twice.
    pub fn reset_results(&mut self) {
        // Everything already asked for belongs to the results being thrown away.
        self.epoch = self.epoch.wrapping_add(1);
        self.buckets.clear();
        self.total = 0;
        self.periods.clear();
        self.period_more.clear();
        self.period_inflight.clear();
        self.window = TileWindow::default();
        self.held.clear();
        self.detail = None;
        self.detail_asked = None;
        self.selected = 0;
        self.scroll = 0;
        self.images_dirty = true;
    }

    /// How many photographs the spine accounts for. Everything that maps an index to a
    /// place on the timeline is arithmetic over this and the bucket counts.
    pub fn recount(&mut self) {
        self.total = self
            .buckets
            .iter()
            .map(|bucket| bucket.count as usize)
            .sum();
        if self.selected >= self.total {
            self.selected = self.total.saturating_sub(1);
        }
    }

    pub fn visible_rows(&self) -> usize {
        (self.grid_area.height / self.tile_height.max(1)).max(1) as usize
    }

    /// The first global index of the given day, or of the nearest older day that has
    /// photographs when that one has none.
    ///
    /// One walk of the sorted spine, so a gap of a month and a gap of eleven years cost
    /// the same. The web's first attempt at this stepped through calendar months looking
    /// for one that existed, gave up after a bounded number of steps, and threw the reader
    /// at the end of the library; on a twenty-year collection a gap of years is ordinary.
    pub fn index_for_date(&self, wanted: &str) -> Option<usize> {
        if self.buckets.is_empty() {
            return None;
        }
        let mut start = 0usize;
        for bucket in &self.buckets {
            if bucket.date.as_str() <= wanted {
                return Some(start.min(self.total.saturating_sub(1)));
            }
            start += bucket.count as usize;
        }
        // Older than anything here. The oldest photograph is as far back as it goes.
        Some(self.total.saturating_sub(1))
    }

    /// The day an index falls on — the inverse of [`App::index_for_date`].
    pub fn date_at_index(&self, index: usize) -> Option<String> {
        let mut start = 0usize;
        for bucket in &self.buckets {
            let end = start + bucket.count as usize;
            if index < end {
                return Some(bucket.date.clone());
            }
            start = end;
        }
        None
    }

    /// The `YYYY-MM` an index falls in.
    pub fn period_at_index(&self, index: usize) -> Option<String> {
        let date = self.date_at_index(index)?;
        (date.len() >= 7).then(|| date[..7].to_string())
    }

    /// Every period the library has, newest first.
    pub fn periods_in_order(&self) -> Vec<String> {
        let mut periods: Vec<String> = Vec::new();
        for bucket in &self.buckets {
            if bucket.date.len() < 7 {
                continue;
            }
            let period = &bucket.date[..7];
            if periods.last().map(String::as_str) != Some(period) {
                periods.push(period.to_string());
            }
        }
        periods
    }

    /// Where a period's first photograph sits in the global order.
    pub fn period_start(&self, period: &str) -> Option<usize> {
        let mut start = 0usize;
        for bucket in &self.buckets {
            if bucket.date.len() >= 7 && &bucket.date[..7] == period {
                return Some(start);
            }
            start += bucket.count as usize;
        }
        None
    }

    /// The periods the viewport covers, plus one either side so stepping down a row does
    /// not stall on a fetch.
    ///
    /// Derived from the buckets alone, so this answers before a single tile has arrived
    /// and before a single frame has been drawn. Nothing here may wait on a measurement
    /// that only a rendered frame provides — that circle is how the web version of this
    /// shipped twice with a grid that never rendered.
    pub fn periods_for_viewport(&self) -> Vec<String> {
        if self.total == 0 {
            return Vec::new();
        }
        let columns = self.columns.max(1);
        let last_index = self.total - 1;
        let first = (self.scroll * columns).min(last_index);
        let last = ((self.scroll + self.visible_rows()) * columns)
            .saturating_sub(1)
            .min(last_index);

        let all = self.periods_in_order();
        let (Some(from), Some(to)) = (self.period_at_index(first), self.period_at_index(last))
        else {
            return Vec::new();
        };
        let (Some(from), Some(to)) = (
            all.iter().position(|held| *held == from),
            all.iter().position(|held| *held == to),
        ) else {
            return Vec::new();
        };
        let low = from.saturating_sub(1);
        let high = (to + 1).min(all.len() - 1);
        all[low..=high].to_vec()
    }

    /// What still needs asking for: `None` when the period is held whole or already on its
    /// way, otherwise the cursor to continue from (`None` for a first request).
    pub fn period_wanted(&self, period: &str) -> Option<Option<String>> {
        if self.period_inflight.contains(period) {
            return None;
        }
        match (
            self.periods.contains_key(period),
            self.period_more.get(period),
        ) {
            (true, None) => None,
            (_, more) => Some(more.cloned()),
        }
    }

    /// Drops the periods the viewport has moved away from. This is the other half of the
    /// bound on memory: the thumbnail cache caps the pictures, this caps the tiles.
    pub fn forget_periods_outside(&mut self, keep: &[String]) {
        self.periods.retain(|period, _| keep.contains(period));
        self.period_more.retain(|period, _| keep.contains(period));
    }

    /// Lays the held periods end to end into the run of tiles the grid indexes into.
    ///
    /// Stops at the first hole. Two periods with a third still on its way are not
    /// adjacent, and joining them would silently draw July's photographs at August's
    /// indices — wrong pictures, with nothing on screen to say so.
    pub fn rebuild_window(&mut self) {
        let mut ordered: Vec<(usize, String)> = self
            .periods
            .keys()
            .filter_map(|period| Some((self.period_start(period)?, period.clone())))
            .collect();
        ordered.sort();

        let mut base = None;
        let mut tiles: Vec<TimelineTile> = Vec::new();
        for (start, period) in ordered {
            let held = &self.periods[&period];
            match base {
                None => {
                    base = Some(start);
                    tiles.extend_from_slice(held);
                }
                Some(first) if first + tiles.len() == start => tiles.extend_from_slice(held),
                Some(_) => break,
            }
        }
        self.window = TileWindow {
            base: base.unwrap_or(0),
            tiles,
        };
    }

    /// Every year the library has, newest first.
    pub fn years(&self) -> Vec<String> {
        let mut years: Vec<String> = Vec::new();
        for bucket in &self.buckets {
            if bucket.date.len() < 4 {
                continue;
            }
            let year = &bucket.date[..4];
            if years.last().map(String::as_str) != Some(year) {
                years.push(year.to_string());
            }
        }
        years
    }

    pub fn index_for_year(&self, year: &str) -> Option<usize> {
        let mut start = 0usize;
        for bucket in &self.buckets {
            if bucket.date.len() >= 4 && &bucket.date[..4] == year {
                return Some(start);
            }
            start += bucket.count as usize;
        }
        None
    }

    /// Steps a whole year — positive is older, the direction the timeline runs.
    ///
    /// Through the years the library actually has, not through the calendar: that is what
    /// makes crossing an eleven-year hole cost one press rather than eleven.
    pub fn step_year(&mut self, delta: isize) {
        let years = self.years();
        if years.is_empty() {
            return;
        }
        let here = self
            .date_at_index(self.selected)
            .filter(|date| date.len() >= 4)
            .map(|date| date[..4].to_string());
        let at = here
            .and_then(|year| years.iter().position(|held| *held == year))
            .unwrap_or(0);
        let next = (at as isize + delta).clamp(0, years.len() as isize - 1) as usize;
        if next == at {
            return;
        }
        if let Some(index) = self.index_for_year(&years[next]) {
            self.go_to(index);
        }
    }

    /// Puts the cursor on a global index and the screen around it.
    pub fn go_to(&mut self, index: usize) {
        if self.total == 0 {
            return;
        }
        self.selected = index.min(self.total - 1);
        // A jump puts its day at the top of the screen rather than leaving it wherever the
        // old scroll happened to sit.
        self.scroll = self.selected / self.columns.max(1);
        self.images_dirty = true;
    }

    /// Where each year sits on a rail `height` rows tall.
    ///
    /// A year whose row a newer one already claimed is left off rather than drawn over it:
    /// a rail is a map, and two labels in one place is worse than one.
    pub fn year_marks(&self, height: u16) -> Vec<(u16, String)> {
        if self.total == 0 || height == 0 {
            return Vec::new();
        }
        let mut marks: Vec<(u16, String)> = Vec::new();
        for year in self.years() {
            let Some(index) = self.index_for_year(&year) else {
                continue;
            };
            let row = self.rail_row_for(index, height);
            if marks.iter().any(|(taken, _)| *taken == row) {
                continue;
            }
            marks.push((row, year));
        }
        marks
    }

    /// Where the cursor sits on a rail `height` rows tall.
    pub fn rail_row(&self, height: u16) -> u16 {
        self.rail_row_for(self.selected, height)
    }

    fn rail_row_for(&self, index: usize, height: u16) -> u16 {
        if self.total <= 1 || height <= 1 {
            return 0;
        }
        let along = index as f64 / (self.total - 1) as f64;
        (along * (height - 1) as f64).round() as u16
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.total == 0 {
            return;
        }
        let last = self.total as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, last) as usize;
        if next != self.selected {
            self.selected = next;
            self.keep_selection_visible();
            self.images_dirty = true;
        }
    }

    pub fn keep_selection_visible(&mut self) {
        if self.columns == 0 {
            return;
        }
        let row = self.selected / self.columns;
        let visible = self.visible_rows();
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + visible {
            self.scroll = row + 1 - visible;
        }
    }

    /// The photographs on screen, which are the ones worth fetching a thumbnail for.
    pub fn visible_ids(&self) -> Vec<String> {
        self.tiles.iter().map(|tile| tile.id.clone()).collect()
    }

    /// Photographs on screen that the server has not finished with. A freshly uploaded one
    /// has no thumbnail for a few seconds — the derivative is generated by a worker — so
    /// it has to be asked about again rather than left blank for ever.
    pub fn pending_on_screen(&self) -> Vec<String> {
        self.tiles
            .iter()
            .filter_map(|tile| self.window.get(tile.index))
            .filter(|tile| !matches!(tile.status, AssetStatus::Ready | AssetStatus::Failed))
            .map(|tile| tile.id.clone())
            .collect()
    }

    /// Takes a re-read asset: the full record for the details panel, and whatever of it
    /// the tile also carries.
    ///
    /// Becoming ready is what makes the picture worth asking for again: the first request
    /// for it was answered with a 404 and cached as "asked for", so without forgetting
    /// that the tile stays empty however long you wait.
    pub fn apply_refreshed(&mut self, asset: Asset) {
        self.refreshing.remove(&asset.id);
        if self.selected_id().as_deref() == Some(asset.id.as_str()) {
            self.detail = Some(asset.clone());
        }

        let became_ready;
        if let Some(tile) = self
            .window
            .tiles
            .iter_mut()
            .find(|held| held.id == asset.id)
        {
            became_ready = asset.status == AssetStatus::Ready && tile.status != AssetStatus::Ready;
            tile.status = asset.status;
            tile.favorite = asset.favorite;
        } else {
            // Reloaded out from under an answer that was still on its way.
            return;
        }

        if became_ready {
            self.thumbnails.remove(&asset.id);
            self.thumb_order.retain(|held| *held != asset.id);
            self.wanted.remove(&asset.id);
            self.forget_preview(&asset.id);
            self.images_dirty = true;
        }
    }

    pub fn preview_for(&self, id: &str) -> Option<&Arc<DynamicImage>> {
        self.previews.get(id)
    }

    /// Keeps a preview, dropping the oldest once there are more than a handful. A preview
    /// is a 1440-pixel rendition; holding one per photograph would grow without limit on
    /// a library worth having.
    pub fn remember_preview(&mut self, id: String, image: Arc<DynamicImage>) {
        if self.previews.insert(id.clone(), image).is_none() {
            self.preview_order.push_back(id);
        }
        while self.preview_order.len() > PREVIEW_CACHE {
            if let Some(oldest) = self.preview_order.pop_front() {
                self.previews.remove(&oldest);
            }
        }
    }

    /// Keeps a decoded grid thumbnail, dropping the oldest once the cap is reached — the
    /// same discipline `previews` has always had, applied to the map that never had it.
    /// This one holds decoded images and used to grow for the whole session; on a
    /// twenty-year library that is a memory leak with a picture in it.
    ///
    /// Two things make the eviction safe. Dropping a picture also forgets that it was ever
    /// asked for: `wanted` is the record of "a request has gone out for this", and leaving
    /// it behind would leave a tile that can never be filled again however far back you
    /// scroll. And a thumbnail the grid is drawing right now is never the one chosen,
    /// because evicting what is on screen is a hole that refills only to be evicted again.
    pub fn remember_thumbnail(&mut self, id: String) {
        // Arrival order, exactly as `preview_order` is, and for a grid that is already
        // recency: thumbnails are fetched as they scroll into view, so the order they
        // arrived in and the order they were last wanted in are the same order. An id
        // already held keeps its place rather than gaining a second one.
        if self.thumb_order.contains(&id) {
            return;
        }
        self.thumb_order.push_back(id);

        let on_screen: HashSet<String> = self.visible_ids().into_iter().collect();
        while self.thumb_order.len() > THUMBNAIL_CACHE {
            let Some(oldest) = self
                .thumb_order
                .iter()
                .position(|held| !on_screen.contains(held))
                .and_then(|at| self.thumb_order.remove(at))
            else {
                // Everything held is on screen. The cap yields rather than the grid.
                break;
            };
            self.thumbnails.remove(&oldest);
            self.wanted.remove(&oldest);
        }
    }

    pub fn forget_preview(&mut self, id: &str) {
        self.previews.remove(id);
        self.preview_order.retain(|held| held != id);
        self.preview_inflight.remove(id);
    }

    pub fn note(&mut self, message: impl Into<String>) {
        self.status = Some(message.into());
    }

    pub fn uploading(&self) -> bool {
        self.upload_total > 0
    }

    /// True once every file has settled, one way or the other.
    pub fn upload_finished(&self) -> bool {
        self.uploading() && self.upload_queue.is_empty() && self.upload_inflight == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imogen_sdk::AssetType;

    fn asset(id: &str, status: AssetStatus) -> Asset {
        Asset {
            id: id.into(),
            owner_id: "owner".into(),
            r#type: AssetType::Image,
            status,
            original_filename: "photo.jpg".into(),
            mime_type: "image/jpeg".into(),
            checksum: "c".repeat(64),
            size_bytes: 1,
            width: None,
            height: None,
            duration: None,
            captured_at: "2024-06-01T09:30:00.000Z".into(),
            captured_at_is_exact: false,
            captured_at_original: None,
            captured_at_original_is_exact: None,
            created_at: "2024-06-01T09:30:00.000Z".into(),
            updated_at: "2024-06-01T09:30:00.000Z".into(),
            deleted_at: None,
            favorite: false,
            archived: false,
            description: None,
            exif: None,
            location: None,
            placeholder_color: None,
            live_photo_video_id: None,
            device_asset_id: None,
        }
    }

    /// A one-day library with one photograph in it, laid out.
    fn holding(ids: &[(&str, AssetStatus)]) -> App {
        let mut app = App::new();
        app.buckets = vec![TimelineBucket {
            date: "2024-06-01".into(),
            count: ids.len() as u64,
            cover_asset_id: None,
        }];
        app.recount();
        app.periods.insert(
            "2024-06".into(),
            ids.iter()
                .map(|(id, status)| TimelineTile {
                    id: (*id).into(),
                    captured_at: "2024-06-01T09:30:00.000Z".into(),
                    width: None,
                    height: None,
                    r#type: AssetType::Image,
                    status: *status,
                    favorite: false,
                    duration: None,
                    placeholder_color: None,
                    live_photo_video_id: None,
                })
                .collect(),
        );
        app.rebuild_window();
        app.tiles = ids
            .iter()
            .enumerate()
            .map(|(index, (id, _))| Tile {
                id: (*id).into(),
                inner: Rect::default(),
                index,
            })
            .collect();
        app
    }

    fn waiting_on(id: &str, status: AssetStatus) -> App {
        let mut app = holding(&[(id, status)]);
        app.tiles = vec![Tile {
            id: id.into(),
            inner: Rect::default(),
            index: 0,
        }];
        // The thumbnail was asked for once and answered with a 404, because the server had
        // not generated it yet. Nothing records the failure — only that it was asked for.
        app.wanted.insert(id.to_string());
        app.refreshing.insert(id.to_string());
        app
    }

    #[test]
    fn a_photograph_still_being_processed_is_asked_about_again() {
        let app = waiting_on("a", AssetStatus::Pending);
        assert_eq!(app.pending_on_screen(), vec!["a".to_string()]);
    }

    #[test]
    fn one_that_is_finished_is_left_alone() {
        assert!(waiting_on("a", AssetStatus::Ready)
            .pending_on_screen()
            .is_empty());
        // A failed one is finished too, in the sense that asking again will not help.
        assert!(waiting_on("a", AssetStatus::Failed)
            .pending_on_screen()
            .is_empty());
    }

    #[test]
    fn becoming_ready_forgets_that_the_picture_was_already_asked_for() {
        let mut app = waiting_on("a", AssetStatus::Pending);
        app.images_dirty = false;

        app.apply_refreshed(asset("a", AssetStatus::Ready));

        // Without this the tile stays empty for ever: the id is remembered as "asked for",
        // so the request that 404'd is never made again.
        assert!(
            !app.wanted.contains("a"),
            "the picture must be asked for again"
        );
        assert!(!app.refreshing.contains("a"));
        assert!(app.images_dirty, "the grid has to be redrawn to show it");
        assert_eq!(
            app.window.get(0).map(|tile| tile.status),
            Some(AssetStatus::Ready)
        );
    }

    #[test]
    fn still_not_ready_changes_nothing_but_lets_the_next_tick_ask_again() {
        let mut app = waiting_on("a", AssetStatus::Pending);
        app.apply_refreshed(asset("a", AssetStatus::Processing));

        assert!(
            app.wanted.contains("a"),
            "no point re-fetching a picture that is not there yet"
        );
        assert!(
            !app.refreshing.contains("a"),
            "but the next tick must be free to ask"
        );
        assert_eq!(app.pending_on_screen(), vec!["a".to_string()]);
    }

    fn dummy_image() -> Arc<DynamicImage> {
        Arc::new(DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2)))
    }

    /// What the event loop does each pass: ask for the selected photograph's preview if it
    /// is not already held and not already on its way. Returns whether a request was made.
    fn tick_viewer(app: &mut App) -> bool {
        let id = app.selected_id().unwrap();
        if app.preview_for(&id).is_none() && app.preview_inflight.insert(id) {
            return true;
        }
        false
    }

    fn arrives(app: &mut App, id: &str) {
        app.preview_inflight.remove(id);
        app.remember_preview(id.to_string(), dummy_image());
    }

    #[test]
    fn looking_at_a_photograph_a_second_time_loads_it_again() {
        // The bug: the record of "already asked for" outlived the picture itself, so
        // coming back to a photograph left the viewer saying "Loading…" for ever.
        let mut app = holding(&[("a", AssetStatus::Ready), ("b", AssetStatus::Ready)]);
        app.mode = Mode::Viewer;

        assert!(tick_viewer(&mut app), "asks for the first one");
        arrives(&mut app, "a");
        assert!(app.preview_for("a").is_some());

        app.move_by(1);
        assert!(tick_viewer(&mut app), "asks for the second");
        arrives(&mut app, "b");

        app.move_by(-1);
        // Held from before, so nothing is asked for and the picture is there at once.
        assert!(!tick_viewer(&mut app));
        assert!(
            app.preview_for("a").is_some(),
            "the first one is still held"
        );
    }

    #[test]
    fn a_photograph_dropped_from_the_cache_is_asked_for_again() {
        let mut app = holding(&[("a", AssetStatus::Ready)]);
        app.mode = Mode::Viewer;

        assert!(tick_viewer(&mut app));
        arrives(&mut app, "a");

        // Enough other photographs to push it out.
        for n in 0..PREVIEW_CACHE {
            app.remember_preview(format!("other{n}"), dummy_image());
        }
        assert!(app.preview_for("a").is_none(), "evicted");
        assert!(
            tick_viewer(&mut app),
            "and so asked for again, not left blank"
        );
    }

    #[test]
    fn the_cache_stays_bounded() {
        let mut app = App::new();
        for n in 0..(PREVIEW_CACHE * 3) {
            app.remember_preview(format!("id{n}"), dummy_image());
        }
        assert_eq!(app.previews.len(), PREVIEW_CACHE);
        assert_eq!(app.preview_order.len(), PREVIEW_CACHE);
        // The newest survive; the oldest are gone.
        assert!(app
            .preview_for(&format!("id{}", PREVIEW_CACHE * 3 - 1))
            .is_some());
        assert!(app.preview_for("id0").is_none());
    }

    #[test]
    fn holding_the_same_one_twice_does_not_grow_the_cache() {
        let mut app = App::new();
        app.remember_preview("a".into(), dummy_image());
        app.remember_preview("a".into(), dummy_image());
        assert_eq!(app.previews.len(), 1);
        assert_eq!(
            app.preview_order.len(),
            1,
            "the order must not gain a duplicate"
        );
    }

    #[test]
    fn a_request_that_fails_can_be_made_again() {
        // Every answer clears the in-flight mark, including a refusal, so a photograph
        // that failed once is not written off for the rest of the session.
        let mut app = holding(&[("a", AssetStatus::Ready)]);
        app.mode = Mode::Viewer;

        assert!(tick_viewer(&mut app));
        assert!(
            !tick_viewer(&mut app),
            "not asked for twice while in flight"
        );

        app.preview_inflight.remove("a"); // the request failed
        assert!(tick_viewer(&mut app), "and may be asked for again");
    }

    #[test]
    fn a_refresh_for_something_no_longer_listed_is_dropped() {
        // The grid can be reloaded while a refresh is in flight.
        let mut app = waiting_on("a", AssetStatus::Pending);
        app.apply_refreshed(asset("gone", AssetStatus::Ready));
        assert_eq!(app.window.tiles.len(), 1);
        assert_eq!(app.window.tiles[0].id, "a");
    }
}
