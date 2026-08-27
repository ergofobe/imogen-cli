//! What the terminal browser is looking at.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use image::DynamicImage;
use imogen_sdk::{Album, Asset, AssetQuery, AssetSort, SortOrder};
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

pub struct App {
    pub assets: Vec<Asset>,
    pub cursor: Option<String>,
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
    pub wanted: HashSet<String>,
    /// The picture shown in the viewer, which is a larger rendition than the grid's.
    pub preview: Option<(String, Arc<DynamicImage>)>,

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
    pub total: Option<u64>,
    pub show_info: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            assets: Vec::new(),
            cursor: None,
            selected: 0,
            scroll: 0,
            mode: Mode::Grid,
            scope: Scope::Library,
            query: String::new(),
            album: None,
            albums: Vec::new(),
            album_selected: 0,
            thumbnails: HashMap::new(),
            wanted: HashSet::new(),
            preview: None,
            grid_area: Rect::default(),
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
            total: None,
            show_info: false,
        }
    }

    pub fn selected_asset(&self) -> Option<&Asset> {
        self.assets.get(self.selected)
    }

    /// The query the current scope means.
    pub fn to_query(&self) -> AssetQuery {
        AssetQuery {
            cursor: None,
            limit: Some(100),
            q: (!self.query.is_empty()).then(|| self.query.clone()),
            r#type: None,
            album_id: self.album.as_ref().map(|album| album.id.clone()),
            favorite: (self.scope == Scope::Favorites).then_some(true),
            archived: (self.scope == Scope::Archived).then_some(true),
            trashed: (self.scope == Scope::Trash).then_some(true),
            taken_after: None,
            taken_before: None,
            bbox: None,
            sort: Some(AssetSort::CapturedAt),
            order: Some(SortOrder::Desc),
        }
    }

    /// Forgets the current results without forgetting the pictures already decoded: the
    /// same photograph in a different scope does not need fetching twice.
    pub fn reset_results(&mut self) {
        self.assets.clear();
        self.cursor = None;
        self.selected = 0;
        self.scroll = 0;
        self.preview = None;
        self.images_dirty = true;
    }

    pub fn visible_rows(&self) -> usize {
        (self.grid_area.height / self.tile_height.max(1)).max(1) as usize
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.assets.is_empty() {
            return;
        }
        let last = self.assets.len() as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, last) as usize;
        if next != self.selected {
            self.selected = next;
            self.preview = None;
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

    /// True when the grid is close enough to the end of what has been fetched that the
    /// next page should be asked for.
    pub fn wants_more(&self) -> bool {
        if self.cursor.is_none() || self.loading {
            return false;
        }
        let visible_end = (self.scroll + self.visible_rows() + 2) * self.columns.max(1);
        visible_end >= self.assets.len()
    }

    /// The photographs on screen, which are the ones worth fetching a thumbnail for.
    pub fn visible_ids(&self) -> Vec<String> {
        self.tiles.iter().map(|tile| tile.id.clone()).collect()
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
