//! The command surface.
//!
//! Two audiences share it. A person gets tables, colour and a terminal browser; an agent
//! gets `--json` on every command, ids on stdout and everything else on stderr, so the
//! output of one command is the input of the next without any parsing in between.

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "imogen",
    version,
    about = "Your photo library, from the command line",
    long_about = "A client for an imogen photo library: search it, upload to it, download \
from it, edit what a photograph says about itself, and administer the server.\n\n\
Run with no arguments to browse the library in the terminal.\n\n\
Every command takes --json, which prints the API's own payload. Ids go to stdout and \
progress goes to stderr, so `imogen ls --json | jq -r '.items[].id'` works and so does \
piping ids into the next command.",
    disable_help_subcommand = true,
    propagate_version = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Args, Clone)]
pub struct GlobalArgs {
    /// The library to talk to, e.g. https://photos.example.com
    #[arg(long, short = 's', global = true, env = "IMOGEN_SERVER")]
    pub server: Option<String>,

    /// Which saved login to use
    #[arg(long, short = 'p', global = true, env = "IMOGEN_PROFILE")]
    pub profile: Option<String>,

    /// A bearer token to use instead of a saved login. Not written to disk.
    #[arg(long, global = true, env = "IMOGEN_TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// Print the API's own JSON rather than a table
    #[arg(long, global = true)]
    pub json: bool,

    /// Print only the data, no headings or commentary
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,

    /// Never colour the output
    #[arg(long, global = true)]
    pub no_color: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Authorize this machine against a library
    Login(LoginArgs),

    /// Forget a saved login
    Logout {
        /// Also tell the server to revoke the token
        #[arg(long)]
        revoke: bool,
    },

    /// Who this profile is signed in as
    Whoami,

    /// Whether the server is reachable, and what this profile can reach
    Status,

    /// Every saved login
    Profiles(ProfilesArgs),

    /// List photographs and videos
    #[command(visible_alias = "ls")]
    List(ListArgs),

    /// Free-text search over filenames, descriptions, places and camera metadata
    Search(SearchArgs),

    /// Everything the library knows about one photograph
    Show(ShowArgs),

    /// Counts and storage for the whole library
    Stats,

    /// How many photographs were taken on each day
    Timeline {
        /// Only days at or after this date, e.g. 2024-06
        #[arg(long)]
        after: Option<String>,
        /// Only days at or before this date
        #[arg(long)]
        before: Option<String>,
    },

    /// Upload files or whole folders
    #[command(visible_alias = "up")]
    Upload(UploadArgs),

    /// Download originals, previews or thumbnails
    #[command(visible_alias = "dl")]
    Download(DownloadArgs),

    /// Change what a photograph says about itself
    Edit(EditArgs),

    /// Move photographs to the trash
    #[command(visible_alias = "rm")]
    Trash(TrashArgs),

    /// Bring photographs back out of the trash
    Restore(RestoreArgs),

    /// Albums, and the links that publish them
    #[command(subcommand, visible_alias = "albums")]
    Album(AlbumCommand),

    /// Public links to one photograph or one album
    #[command(subcommand)]
    Share(ShareCommand),

    /// People, as grouped by face recognition
    #[command(subcommand)]
    People(PeopleCommand),

    /// Your own account
    #[command(subcommand)]
    Account(AccountCommand),

    /// Server administration
    #[command(subcommand)]
    Admin(AdminCommand),

    /// Browse the library in the terminal
    Tui,

    /// Print a shell completion script
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// The library to sign in to
    #[arg(long, short = 'S')]
    pub server: Option<String>,

    /// Save this login under a name, so several libraries can coexist
    #[arg(long, default_value = "default")]
    pub name: String,

    /// Use a token you already have instead of opening a browser
    #[arg(long, hide_env_values = true)]
    pub with_token: Option<String>,

    /// Print the authorization URL rather than opening a browser. For a machine with no
    /// display, or an agent driving the terminal.
    #[arg(long)]
    pub no_browser: bool,

    /// Ask for a narrower set of scopes than the default
    #[arg(long, value_delimiter = ',')]
    pub scope: Vec<String>,

    /// How this machine names itself in the library's connected-applications list
    #[arg(long, default_value = "imogen CLI")]
    pub app_name: String,
}

#[derive(Debug, Args)]
pub struct ProfilesArgs {
    /// Make this profile the one used when --profile is not given
    #[arg(long)]
    pub set_default: Option<String>,
}

/// The filters every command that selects photographs shares.
#[derive(Debug, Args, Clone, Default)]
pub struct QueryArgs {
    /// Free-text over filename, description, camera and place
    #[arg(long, short = 'Q')]
    pub query: Option<String>,

    /// Only images, or only videos
    #[arg(long, short = 't', value_enum)]
    pub r#type: Option<MediaType>,

    /// Only photographs in this album
    #[arg(long)]
    pub album: Option<String>,

    /// Only favourites
    #[arg(long)]
    pub favorite: bool,

    /// Only archived photographs
    #[arg(long)]
    pub archived: bool,

    /// Only photographs in the trash
    #[arg(long)]
    pub trashed: bool,

    /// Taken on or after this date, e.g. 2024-06-01
    #[arg(long)]
    pub after: Option<String>,

    /// Taken on or before this date
    #[arg(long)]
    pub before: Option<String>,

    /// Taken inside this box: minLat,minLon,maxLat,maxLon
    #[arg(long)]
    pub bbox: Option<String>,

    /// What to sort by
    #[arg(long, value_enum)]
    pub sort: Option<SortField>,

    /// Which way to sort
    #[arg(long, value_enum)]
    pub order: Option<SortOrder>,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub query: QueryArgs,

    /// How many to return. Ignored with --all.
    #[arg(long, short = 'n', default_value_t = 50)]
    pub limit: u32,

    /// Walk every page rather than stopping at one
    #[arg(long)]
    pub all: bool,

    /// Print only ids, one per line, for piping into another command
    #[arg(long)]
    pub ids: bool,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// What to look for
    pub text: String,

    #[command(flatten)]
    pub query: QueryArgs,

    #[arg(long, short = 'n', default_value_t = 50)]
    pub limit: u32,

    #[arg(long)]
    pub all: bool,

    #[arg(long)]
    pub ids: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// The asset id
    pub id: String,

    /// Draw the photograph in the terminal
    #[arg(long, short = 'i')]
    pub image: bool,

    /// How many terminal rows the picture may use
    #[arg(long, default_value_t = 20)]
    pub rows: u16,

    /// Also list the faces found in it
    #[arg(long)]
    pub faces: bool,
}

#[derive(Debug, Args)]
#[command(long_about = "Upload files or whole folders.

Metadata given as flags applies to everything in the run. To give each file its own metadata — which is what a script importing from somewhere else needs — pass a manifest instead: one JSON object per line, with a `path` and any of `capturedAt`, `description`, `location`, `favorite`, `filename`, `deviceAssetId` and `album`.

  {\"path\": \"IMG_1234.JPG\", \"capturedAt\": 1717233000, \"favorite\": true,
   \"location\": {\"latitude\": 50.1109, \"longitude\": -5.5372},
   \"description\": \"Fishing boats at dawn\", \"album\": \"Cornwall\"}

`capturedAt` may be ISO-8601, a plain date, or seconds since the epoch. `filename` is what the photograph will be called in the library, whatever the file on disk is named. Uploads are idempotent by content, so re-running a manifest costs a checksum rather than a duplicate.")]
pub struct UploadArgs {
    /// Files or folders to upload
    pub paths: Vec<std::path::PathBuf>,

    /// Read what to upload, and each file's own metadata, from JSON Lines. `-` is stdin.
    #[arg(long, short = 'm', conflicts_with = "paths")]
    pub manifest: Option<std::path::PathBuf>,

    /// Descend into folders
    #[arg(long, short = 'r')]
    pub recursive: bool,

    /// Put everything uploaded into this album, creating it if the name is new
    #[arg(long, short = 'a')]
    pub album: Option<String>,

    /// Mark everything uploaded as a favourite
    #[arg(long)]
    pub favorite: bool,

    /// Attach this description to everything uploaded
    #[arg(long)]
    pub description: Option<String>,

    /// The capture time, as ISO-8601, a plain date, or seconds since the epoch
    #[arg(long)]
    pub captured_at: Option<String>,

    /// Where it was taken: lat,lon[,altitude]
    #[arg(long)]
    pub location: Option<String>,

    /// What to call it in the library, whatever the file on disk is named. One file only.
    #[arg(long)]
    pub filename: Option<String>,

    /// A stable id of your own, so sending the same file again is recognised rather than
    /// stored twice. One file only.
    #[arg(long)]
    pub device_id: Option<String>,

    /// Use each file's path, relative to the folder given, as its device asset id
    #[arg(long, conflicts_with = "device_id")]
    pub device_ids: bool,

    /// How many files to send at once
    #[arg(long, short = 'j', default_value_t = 6)]
    pub concurrency: usize,

    /// Append one JSON object per file to this path as each settles, so a long run can be
    /// resumed and audited without waiting for the summary
    #[arg(long)]
    pub report: Option<std::path::PathBuf>,

    /// List what would be uploaded and stop
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct DownloadArgs {
    /// Asset ids. Omit them and every match for the filters is downloaded.
    pub ids: Vec<String>,

    #[command(flatten)]
    pub query: QueryArgs,

    /// Where to write. Created if it does not exist.
    #[arg(long, short = 'o', default_value = ".")]
    pub out: std::path::PathBuf,

    /// Which rendition to fetch
    #[arg(long, value_enum, default_value_t = Variant::Original)]
    pub variant: Variant,

    /// How to lay the files out. {yyyy} {mm} {dd} {id} {name} {ext} {album} are replaced.
    #[arg(long, default_value = "{yyyy}/{mm}/{name}")]
    pub layout: String,

    /// Overwrite a file that is already there rather than skipping it
    #[arg(long)]
    pub overwrite: bool,

    /// How many to fetch at once
    #[arg(long, short = 'j', default_value_t = 4)]
    pub concurrency: usize,

    /// Stop after this many
    #[arg(long, short = 'n')]
    pub limit: Option<u32>,

    /// List what would be written and stop
    #[arg(long)]
    pub dry_run: bool,
}

/// The filters a command may use to choose photographs when it also sets `--favorite` or
/// `--archive`. Those two words cannot mean "find these" and "make them this" in the same
/// command, so selecting by them is done by piping ids in:
///
/// ```text
/// imogen ls --favorite --ids | xargs imogen edit --archive
/// ```
#[derive(Debug, Args, Clone, Default)]
pub struct SelectArgs {
    /// Free-text over filename, description, camera and place
    #[arg(long, short = 'Q')]
    pub query: Option<String>,

    /// Only images, or only videos
    #[arg(long, short = 't', value_enum)]
    pub r#type: Option<MediaType>,

    /// Only photographs in this album
    #[arg(long)]
    pub album: Option<String>,

    /// Only photographs in the trash
    #[arg(long)]
    pub trashed: bool,

    /// Taken on or after this date, e.g. 2024-06-01
    #[arg(long)]
    pub after: Option<String>,

    /// Taken on or before this date
    #[arg(long)]
    pub before: Option<String>,

    /// Taken inside this box: minLat,minLon,maxLat,maxLon
    #[arg(long)]
    pub bbox: Option<String>,
}

impl SelectArgs {
    pub fn to_query(&self) -> QueryArgs {
        QueryArgs {
            query: self.query.clone(),
            r#type: self.r#type,
            album: self.album.clone(),
            favorite: false,
            archived: false,
            trashed: self.trashed,
            after: self.after.clone(),
            before: self.before.clone(),
            bbox: self.bbox.clone(),
            sort: None,
            order: None,
        }
    }
}

#[derive(Debug, Args)]
pub struct EditArgs {
    /// Asset ids. Omit them and every match for the filters is edited.
    pub ids: Vec<String>,

    #[command(flatten)]
    pub select: SelectArgs,

    /// Mark as a favourite
    #[arg(long, overrides_with = "no_favorite")]
    pub favorite: bool,

    /// Stop being a favourite
    #[arg(long)]
    pub no_favorite: bool,

    /// Hide from the timeline without deleting
    #[arg(long, overrides_with = "unarchive")]
    pub archive: bool,

    /// Put back on the timeline
    #[arg(long)]
    pub unarchive: bool,

    /// Set the description
    #[arg(long, short = 'd')]
    pub description: Option<String>,

    /// Remove the description
    #[arg(long)]
    pub clear_description: bool,

    /// Correct the capture time, as ISO-8601 or YYYY-MM-DD
    #[arg(long)]
    pub captured_at: Option<String>,

    /// Discard a capture-time correction and go back to the imported date
    #[arg(long)]
    pub reset_captured_at: bool,

    /// Set where it was taken: lat,lon[,altitude]
    #[arg(long)]
    pub location: Option<String>,

    /// Forget where it was taken
    #[arg(long)]
    pub clear_location: bool,

    /// Apply to every match without asking, when selecting by filter
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct TrashArgs {
    /// Asset ids. Omit them and every match for the filters is trashed.
    pub ids: Vec<String>,

    #[command(flatten)]
    pub query: QueryArgs,

    /// Trash every match without asking
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// Asset ids. Omit them and everything in the trash is restored.
    pub ids: Vec<String>,

    /// Restore everything without asking
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Subcommand)]
pub enum AlbumCommand {
    /// Every album
    #[command(visible_alias = "ls")]
    List,

    /// One album and what is in it
    Show {
        /// The album id, or enough of its name to be unambiguous
        album: String,
        /// Print only asset ids
        #[arg(long)]
        ids: bool,
    },

    /// Make an album
    Create {
        name: String,
        #[arg(long, short = 'd')]
        description: Option<String>,
        /// Asset ids to put in it
        #[arg(long = "asset")]
        assets: Vec<String>,
    },

    /// Rename an album, or change its description or cover
    Update {
        album: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, short = 'd')]
        description: Option<String>,
        #[arg(long)]
        clear_description: bool,
        /// The asset id to use as the cover
        #[arg(long)]
        cover: Option<String>,
    },

    /// Delete an album. The photographs in it are not touched.
    Delete {
        album: String,
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Put photographs into an album
    Add {
        /// The album to add to, by id or name
        target: String,
        /// Asset ids. Omit them and every match for the filters is added.
        assets: Vec<String>,
        /// `--album` here selects what to copy from, so one album can be poured into
        /// another: `imogen album add "Best of 2019" --album "Cornwall 2019"`.
        #[command(flatten)]
        query: QueryArgs,
    },

    /// Take photographs out of an album
    Remove { album: String, assets: Vec<String> },
}

#[derive(Debug, Subcommand)]
pub enum ShareCommand {
    /// Publish a photograph or an album
    Create {
        #[arg(value_enum)]
        kind: ShareKind,
        /// The asset or album id
        id: String,
        /// When the link should stop working, as ISO-8601 or YYYY-MM-DD
        #[arg(long)]
        expires: Option<String>,
        /// Require this password to open it
        #[arg(long)]
        password: Option<String>,
        /// Do not offer a download button
        #[arg(long)]
        no_download: bool,
    },

    /// The live link for a photograph or an album, if there is one
    Show {
        #[arg(value_enum)]
        kind: ShareKind,
        id: String,
    },

    /// Stop publishing
    Revoke {
        #[arg(value_enum)]
        kind: ShareKind,
        id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PeopleCommand {
    /// Everyone the library has grouped
    #[command(visible_alias = "ls")]
    List {
        /// Include the people who have been hidden
        #[arg(long)]
        hidden: bool,
    },

    /// One person and the photographs they are in
    Show {
        /// The person id, or their name
        person: String,
        #[arg(long)]
        ids: bool,
    },

    /// Give a grouping a name
    Name { person: String, name: String },

    /// Hide a grouping, or show it again
    Hide {
        person: String,
        #[arg(long)]
        undo: bool,
    },

    /// Fold several groupings into one, when the same person was split in two
    Merge {
        /// The grouping to keep
        keep: String,
        /// The groupings to fold into it
        #[arg(required = true)]
        merge: Vec<String>,
    },

    /// The faces found in one photograph
    Faces { asset: String },

    /// Whether face grouping is on, and how far it has got
    Status,

    /// Turn face grouping on or off. Administrators only.
    Enable {
        #[arg(long)]
        off: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Your account, and what it is using
    Show,

    /// Change your name or email address
    Update {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        email: Option<String>,
        /// Your current password, which changing an email address requires
        #[arg(long)]
        current_password: Option<String>,
    },

    /// Change your password
    Password {
        #[arg(long)]
        current: Option<String>,
        #[arg(long)]
        new: String,
    },

    /// End every session this account has, everywhere
    LogoutEverywhere {
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum AdminCommand {
    /// Every account on the server
    Users,

    /// Change a role, or suspend and restore an account
    User {
        id: String,
        #[arg(long, value_enum)]
        role: Option<Role>,
        #[arg(long)]
        disable: bool,
        #[arg(long)]
        enable: bool,
    },

    /// Remove an account. Its photographs go to the trash, not the incinerator.
    DeleteUser {
        id: String,
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Set somebody's password and end every session they had
    ResetPassword { id: String, password: String },

    /// Invitations, live and spent
    Invites,

    /// Make an invitation. The token is shown once.
    Invite {
        /// Only this address may use the link
        #[arg(long)]
        email: Option<String>,
        #[arg(long, value_enum, default_value_t = Role::User)]
        role: Role,
        #[arg(long, default_value_t = 7)]
        days: u32,
    },

    /// Withdraw an invitation
    RevokeInvite { id: String },

    /// Queue depth, what is running, and what the pipeline gave up on
    Queue,

    /// Put failed work back in the queue
    Retry {
        /// One job id. Omit it to retry everything that failed.
        id: Option<String>,
    },

    /// Throw away one failed job
    Discard { id: String },

    /// Applications allowed to act on somebody's behalf
    Clients,

    /// Remove an application. Its tokens go with it.
    RevokeClient { id: String },

    /// Live sessions
    Sessions,

    /// End a session
    RevokeSession { id: String },

    /// Where the bytes are
    Storage,

    /// Settings that take effect without a restart
    Settings {
        #[arg(long)]
        allow_signup: Option<bool>,
        #[arg(long)]
        trash_retention_days: Option<u32>,
        #[arg(long)]
        faces_enabled: Option<bool>,
    },

    /// Every link that is public right now, across all accounts
    Shares,

    /// Close a link, whoever made it
    RevokeShare { id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MediaType {
    Image,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SortField {
    CapturedAt,
    CreatedAt,
    Filename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SortOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Variant {
    Original,
    Preview,
    Thumbnail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ShareKind {
    Photo,
    Album,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Role {
    Admin,
    User,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap's own consistency check, over every subcommand. It catches a flattened filter
    /// whose flag collides with a flag the command already has — which is a panic in a
    /// debug build and, worse, silently the wrong argument in a release one.
    #[test]
    fn every_command_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_command_that_both_selects_and_sets_keeps_the_two_apart() {
        // `edit --favorite` means make it a favourite. Selecting favourites is `ls
        // --favorite --ids` piped in, so the word is never ambiguous.
        let edit = Cli::try_parse_from(["imogen", "edit", "abc", "--favorite"]).unwrap();
        match edit.command {
            Some(Command::Edit(args)) => {
                assert!(args.favorite);
                assert_eq!(args.ids, vec!["abc".to_string()]);
            }
            other => panic!("parsed as {other:?}"),
        }
        assert!(Cli::try_parse_from(["imogen", "ls", "--favorite"]).is_ok());
    }

    #[test]
    fn a_manifest_and_paths_are_not_both_accepted() {
        // Taking both would leave it unclear which metadata applied to what.
        assert!(
            Cli::try_parse_from(["imogen", "upload", "a.jpg", "--manifest", "m.jsonl"]).is_err()
        );
    }
}
