//! What every command is handed: a client, a way to print, and the few lookups that would
//! otherwise be repeated in a dozen places.

use std::io::{IsTerminal, Write};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context as _, Result};
use futures::StreamExt;
use imogen_sdk::{
    Album, Asset, AssetFilter, AssetQuery, AssetSort, AssetType, ClientOptions, ImogenClient,
    Person, SortOrder, TimelineQuery,
};

use crate::auth::ProfileTokens;
use crate::cli::{GlobalArgs, MediaType, QueryArgs, SortField, SortOrder as CliOrder, Variant};
use crate::config::{Config, Profile};
use crate::output::Output;

pub struct Context {
    pub client: ImogenClient,
    pub out: Output,
    pub profile_name: String,
    pub server: String,
    pub tokens: Arc<ProfileTokens>,
}

impl Context {
    pub fn build(global: &GlobalArgs) -> Result<Self> {
        let config = Config::load()?;
        let profile_name = global
            .profile
            .clone()
            .unwrap_or_else(|| config.default_profile_name());
        let (profile, ephemeral) = crate::auth::resolve(
            &config,
            &profile_name,
            global.server.as_deref(),
            global.token.as_deref(),
        )?;
        Ok(Self::from_profile(
            global,
            &profile_name,
            profile,
            ephemeral,
        ))
    }

    pub fn from_profile(
        global: &GlobalArgs,
        profile_name: &str,
        profile: Profile,
        ephemeral: bool,
    ) -> Self {
        let server = profile.server.clone();
        let tokens = ProfileTokens::new(profile_name, profile, ephemeral);
        let client = ImogenClient::new(
            ClientOptions::new(server.clone())
                .token_source(tokens.clone())
                .on_unauthorized(tokens.clone()),
        );
        Self {
            client,
            out: Output::new(global.json, global.no_color, global.quiet),
            profile_name: profile_name.to_string(),
            server,
            tokens,
        }
    }

    /// The set of photographs a command was pointed at: explicit ids if any were given,
    /// otherwise everything matching the filters.
    ///
    /// A command that changes things asks before acting on a filter, because "every photo
    /// matching nothing in particular" is every photo. Explicit ids never prompt: naming
    /// them is the confirmation.
    pub async fn select(
        &self,
        ids: &[String],
        query: &QueryArgs,
        limit: Option<u32>,
    ) -> Result<Vec<String>> {
        if !ids.is_empty() {
            return Ok(ids.to_vec());
        }
        if query.is_empty() {
            bail!("Name some asset ids, or give a filter such as --query or --album");
        }
        let assets = self.matching(query, limit).await?;
        Ok(assets.into_iter().map(|asset| asset.id).collect())
    }

    /// Every asset matching the filters, walking pages until they run out or `limit` is
    /// reached.
    pub async fn matching(&self, query: &QueryArgs, limit: Option<u32>) -> Result<Vec<Asset>> {
        let mut sdk_query = self.to_query(query).await?;
        sdk_query.limit = Some(limit.map(|l| l.min(200)).unwrap_or(200));

        let mut collected = Vec::new();
        let mut stream = Box::pin(self.client.assets.iterate(&sdk_query));
        while let Some(asset) = stream.next().await {
            collected.push(asset?);
            if let Some(limit) = limit {
                if collected.len() as u32 >= limit {
                    break;
                }
            }
        }
        Ok(collected)
    }

    /// Translates the command-line filters into the API's filter, resolving an album given
    /// by name into its id on the way through.
    pub async fn to_filter(&self, args: &QueryArgs) -> Result<AssetFilter> {
        let album_id = match &args.album {
            Some(reference) => Some(self.find_album(reference).await?.id),
            None => None,
        };
        Ok(AssetFilter {
            q: args.query.clone(),
            r#type: args.r#type.map(|t| match t {
                MediaType::Image => AssetType::Image,
                MediaType::Video => AssetType::Video,
            }),
            album_id,
            person_id: None,
            favorite: args.favorite.then_some(true),
            archived: args.archived.then_some(true),
            trashed: args.trashed.then_some(true),
            taken_after: args.after.as_deref().map(crate::dates::to_start_of_day),
            taken_before: args.before.as_deref().map(crate::dates::to_end_of_day),
            bbox: args.bbox.clone(),
        })
    }

    /// The same filters, in the shape a page listing wants rather than a bulk mutation.
    pub async fn to_query(&self, args: &QueryArgs) -> Result<AssetQuery> {
        let filter = self.to_filter(args).await?;
        Ok(AssetQuery {
            cursor: None,
            limit: None,
            q: filter.q,
            r#type: filter.r#type,
            album_id: filter.album_id,
            person_id: filter.person_id,
            favorite: filter.favorite,
            archived: filter.archived,
            trashed: filter.trashed,
            taken_after: filter.taken_after,
            taken_before: filter.taken_before,
            bbox: filter.bbox,
            sort: args.sort.map(|s| match s {
                SortField::CapturedAt => AssetSort::CapturedAt,
                SortField::CreatedAt => AssetSort::CreatedAt,
                SortField::Filename => AssetSort::Filename,
            }),
            order: args.order.map(|o| match o {
                CliOrder::Asc => SortOrder::Asc,
                CliOrder::Desc => SortOrder::Desc,
            }),
        })
    }

    /// How many photographs a filter matches, from the timeline's day buckets rather than
    /// a walk of every page — the number a confirmation prompt needs, not the assets
    /// themselves.
    pub async fn count(&self, filter: &AssetFilter) -> Result<u64> {
        let timeline = self
            .client
            .assets
            .timeline(&TimelineQuery {
                covers: None,
                filter: filter.clone(),
            })
            .await?;
        Ok(timeline.buckets.iter().map(|bucket| bucket.count).sum())
    }

    /// An album by id, by its whole name when exactly one album carries it, or by enough
    /// of its name to be unambiguous. `Ok(None)` is the one case a caller may answer by
    /// making an album: an empty reference and an ambiguous one are errors, because
    /// neither says which album was meant and creating one is not the answer to either.
    ///
    /// The reference arrives trimmed, so the guard and the search agree on what it is.
    async fn look_up_album(&self, reference: &str) -> Result<Option<Album>> {
        // `contains("")` is true of every name, so `imogen trash --album "$ALBUM"` with
        // the variable unset would otherwise resolve to whichever album was listed first
        // and trash all of it. Refused before the lookup, so nothing reaches the wire.
        if reference.is_empty() {
            bail!("Name an album by id or name — an empty reference cannot pick one");
        }
        let albums = self.client.albums.list().await?;
        if let Some(exact) = albums.iter().find(|album| album.id == reference) {
            return Ok(Some(exact.clone()));
        }
        let lowered = reference.to_lowercase();
        // Only when it is an album's whole name and no other album's. Album names have no
        // unique index, so two called "Holiday" fall through to the ambiguity refusal
        // below rather than resolving to whichever the server listed first.
        let mut whole_name = albums
            .iter()
            .filter(|album| album.name.to_lowercase() == lowered);
        if let (Some(named), None) = (whole_name.next(), whole_name.next()) {
            return Ok(Some(named.clone()));
        }
        let matches: Vec<&Album> = albums
            .iter()
            .filter(|album| album.name.to_lowercase().contains(&lowered))
            .collect();
        match matches.len() {
            1 => Ok(Some(matches[0].clone())),
            0 => Ok(None),
            _ => {
                let names: Vec<&str> = matches.iter().map(|a| a.name.as_str()).collect();
                Err(anyhow!(
                    "\"{reference}\" matches several albums: {}",
                    names.join(", ")
                ))
            }
        }
    }

    /// An album by id, or by enough of its name to be unambiguous. Naming one is what a
    /// person will actually do; refusing an ambiguous name is better than picking one.
    pub async fn find_album(&self, reference: &str) -> Result<Album> {
        let reference = reference.trim();
        self.look_up_album(reference).await?.ok_or_else(|| {
            anyhow!(
                "No album called \"{reference}\". `imogen album list` shows them, and `imogen album create` makes one."
            )
        })
    }

    /// The album of that name, made if it is not there yet. A description is only used
    /// when the album is new: it never overwrites one somebody has already written.
    ///
    /// Only a name nothing answers to is made: this used to treat every failure as "not
    /// there yet", so an ambiguous name quietly added a third album of that name.
    pub async fn album_or_create(
        &self,
        reference: &str,
        description: Option<&str>,
    ) -> Result<Album> {
        let reference = reference.trim();
        if let Some(album) = self.look_up_album(reference).await? {
            return Ok(album);
        }
        self.client
            .albums
            .create(&imogen_sdk::AlbumCreate {
                name: reference.to_string(),
                description: description.map(str::to_string),
                ..Default::default()
            })
            .await
            .context("Could not create the album")
    }

    /// A person by id, by their whole name when exactly one person carries it, or by
    /// enough of it to be unambiguous. The whole-name tier is why somebody called "Al" is
    /// reachable at all rather than lost to "Alice" starting with it. `find_album` has the
    /// same three tiers and the same two guards, for the same reasons.
    pub async fn find_person(&self, reference: &str) -> Result<Person> {
        // `contains("")` is true of every name, so an unset `$PERSON` in a script would
        // otherwise resolve to whoever happened to be listed first — and `merge` and
        // `reassign` move data. Refused before the lookup, so nothing reaches the wire.
        // Trimmed once, so the guard and the search agree: the shell interpolation this
        // is about is also where a trailing newline comes from.
        let reference = reference.trim();
        if reference.is_empty() {
            bail!("Name somebody by id or name — an empty reference cannot pick anybody");
        }
        let people = self.client.people.list(true).await?;
        if let Some(exact) = people.iter().find(|person| person.id == reference) {
            return Ok(exact.clone());
        }
        let lowered = reference.to_lowercase();
        // Only when it is somebody's whole name and nobody else's. Person names are not
        // unique — `people merge` exists because grouping produces two clusters for one
        // person — so two called "Al" fall through to the ambiguity refusal below rather
        // than resolving to whichever the server listed first.
        let mut whole_name = people.iter().filter(|person| {
            person
                .name
                .as_deref()
                .is_some_and(|name| name.to_lowercase() == lowered)
        });
        if let (Some(named), None) = (whole_name.next(), whole_name.next()) {
            return Ok(named.clone());
        }
        let matches: Vec<&Person> = people
            .iter()
            .filter(|person| {
                person
                    .name
                    .as_deref()
                    .map(|name| name.to_lowercase().contains(&lowered))
                    .unwrap_or(false)
            })
            .collect();
        match matches.len() {
            1 => Ok(matches[0].clone()),
            0 => Err(anyhow!("Nobody called \"{reference}\"")),
            _ => Err(anyhow!(
                "\"{reference}\" matches several people; use an id instead"
            )),
        }
    }

    /// Asks before something irreversible. Anything that is not a terminal — a script, an
    /// agent — must pass `--yes` rather than being asked a question nobody will answer.
    pub fn confirm(&self, prompt: &str, assumed: bool) -> Result<bool> {
        if assumed {
            return Ok(true);
        }
        if !std::io::stdin().is_terminal() {
            bail!("{prompt} Pass --yes to go ahead.");
        }
        eprint!("{prompt} [y/N] ");
        std::io::stderr().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
    }
}

impl QueryArgs {
    pub fn is_empty(&self) -> bool {
        self.query.is_none()
            && self.r#type.is_none()
            && self.album.is_none()
            && !self.favorite
            && !self.archived
            && !self.trashed
            && self.after.is_none()
            && self.before.is_none()
            && self.bbox.is_none()
    }
}

impl From<Variant> for imogen_sdk::AssetVariant {
    fn from(value: Variant) -> Self {
        match value {
            Variant::Original => imogen_sdk::AssetVariant::Original,
            Variant::Preview => imogen_sdk::AssetVariant::Preview,
            Variant::Thumbnail => imogen_sdk::AssetVariant::Thumbnail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use crate::cli::GlobalArgs;
    use crate::config::Profile;

    /// A library with nothing to be ambiguous with, which is where an empty reference does
    /// its damage quietly rather than colliding with a second match.
    const ONE_ALBUM: &str = r#"{"items":[
        {"id":"album-1","ownerId":"me","name":"Holiday","description":null,
         "coverAssetId":null,"assetCount":9,"createdAt":"2024-01-01T00:00:00Z",
         "updatedAt":"2024-01-01T00:00:00Z","shareSlug":null}
    ]}"#;

    /// One album whose whole name is the start of another's, which is why the exact-name
    /// tier exists at all.
    const ALBUMS: &str = r#"{"items":[
        {"id":"album-1","ownerId":"me","name":"Holiday","description":null,
         "coverAssetId":null,"assetCount":9,"createdAt":"2024-01-01T00:00:00Z",
         "updatedAt":"2024-01-01T00:00:00Z","shareSlug":null},
        {"id":"album-2","ownerId":"me","name":"Holiday 2024","description":null,
         "coverAssetId":null,"assetCount":4,"createdAt":"2024-01-01T00:00:00Z",
         "updatedAt":"2024-01-01T00:00:00Z","shareSlug":null},
        {"id":"album-3","ownerId":"me","name":"Weekend","description":null,
         "coverAssetId":null,"assetCount":2,"createdAt":"2024-01-01T00:00:00Z",
         "updatedAt":"2024-01-01T00:00:00Z","shareSlug":null}
    ]}"#;

    /// Album names have no unique index, so two albums can carry the same one.
    const TWO_HOLIDAYS: &str = r#"{"items":[
        {"id":"album-1","ownerId":"me","name":"Holiday","description":null,
         "coverAssetId":null,"assetCount":9,"createdAt":"2024-01-01T00:00:00Z",
         "updatedAt":"2024-01-01T00:00:00Z","shareSlug":null},
        {"id":"album-2","ownerId":"me","name":"Holiday","description":null,
         "coverAssetId":null,"assetCount":4,"createdAt":"2024-01-01T00:00:00Z",
         "updatedAt":"2024-01-01T00:00:00Z","shareSlug":null}
    ]}"#;

    #[tokio::test]
    async fn an_empty_album_is_refused_before_anything_reaches_the_wire() {
        // `imogen trash --album "$ALBUM"` with the variable unset. `contains("")` is true
        // of every name, so this used to resolve to the only album in the library and
        // hand `trash` a filter that matched all of it, exit 0.
        for reference in ["", "   "] {
            let stub = stub_returning(ONE_ALBUM).await;
            let error = context(&stub.base_url)
                .to_filter(&QueryArgs {
                    album: Some(reference.to_string()),
                    ..Default::default()
                })
                .await
                .expect_err("an empty --album is not an album");
            assert!(
                error.to_string().contains("Name an album"),
                "said {error} instead of naming the problem"
            );
            assert!(
                stub.calls().is_empty(),
                "nothing should reach the wire for a reference that cannot pick an album"
            );
        }
    }

    #[tokio::test]
    async fn an_empty_reference_makes_no_album_either() {
        // The guard above cannot land on its own: `album_or_create` treated every failure
        // as "not there yet", so a refused empty reference would make an album called "".
        let stub = stub_returning(ONE_ALBUM).await;
        let error = context(&stub.base_url)
            .album_or_create("  ", None)
            .await
            .expect_err("an empty --album is not an album");
        assert!(
            error.to_string().contains("Name an album"),
            "said {error} instead of naming the problem"
        );
        assert!(
            stub.created().is_empty(),
            "an album that cannot be named should not be created"
        );
    }

    #[tokio::test]
    async fn a_name_two_albums_share_is_refused() {
        let stub = stub_returning(TWO_HOLIDAYS).await;
        let error = context(&stub.base_url)
            .find_album("Holiday")
            .await
            .expect_err("two albums are called Holiday");
        assert!(
            error.to_string().contains("matches several albums"),
            "said {error} instead of refusing"
        );
    }

    #[tokio::test]
    async fn an_ambiguous_name_creates_no_duplicate() {
        // `album_or_create` matched on `Err(_)`, so an ambiguous name made a third album
        // of the same name rather than asking which of the two was meant.
        let stub = stub_returning(TWO_HOLIDAYS).await;
        let error = context(&stub.base_url)
            .album_or_create("Holiday", None)
            .await
            .expect_err("two albums are called Holiday");
        assert!(
            error.to_string().contains("matches several albums"),
            "said {error} instead of refusing"
        );
        assert!(
            stub.created().is_empty(),
            "an ambiguous name should not add a third album of that name"
        );
    }

    #[tokio::test]
    async fn an_exact_name_beats_the_longer_one_it_is_a_prefix_of() {
        let stub = stub_returning(ALBUMS).await;
        let album = context(&stub.base_url).find_album("holiday").await.unwrap();
        assert_eq!(album.id, "album-1");
    }

    #[tokio::test]
    async fn a_name_with_a_stray_newline_still_finds_it() {
        // `--album "$(cat name.txt)"` arrives as "Holiday\n". Refusing an empty reference
        // on its trimmed form and then searching on the untrimmed one would report no
        // album called "Holiday\n".
        let stub = stub_returning(ALBUMS).await;
        let album = context(&stub.base_url)
            .find_album("Holiday\n")
            .await
            .unwrap();
        assert_eq!(album.id, "album-1");
    }

    #[tokio::test]
    async fn a_new_album_is_created_under_the_trimmed_name() {
        let stub = stub_returning(ONE_ALBUM).await;
        context(&stub.base_url)
            .album_or_create("Trip\n", None)
            .await
            .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&stub.created()[0]).unwrap();
        assert_eq!(
            sent["name"], "Trip",
            "the name searched for is the name made"
        );
    }

    fn context(server: &str) -> Context {
        let global = GlobalArgs {
            server: None,
            profile: None,
            token: None,
            json: false,
            quiet: true,
            no_color: true,
        };
        Context::from_profile(
            &global,
            "test",
            Profile {
                server: server.to_string(),
                client_id: None,
                access_token: Some("token".into()),
                refresh_token: None,
                expires_in: 3600,
                obtained_at: 0,
                scope: String::new(),
            },
            true,
        )
    }

    struct Stub {
        base_url: String,
        calls: Arc<Mutex<Vec<(String, String, String)>>>,
    }

    impl Stub {
        /// Every request, as method, path and body.
        fn calls(&self) -> Vec<(String, String, String)> {
            self.calls.lock().unwrap().clone()
        }

        /// The bodies of the album creations, which is what a duplicate looks like from
        /// the outside.
        fn created(&self) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter(|(method, path, _)| method == "post" && path == "/api/v1/albums")
                .map(|(_, _, body)| body)
                .collect()
        }
    }

    /// A stub imogen over a library of the caller's choosing, speaking just enough
    /// HTTP/1.1 to list albums and to make one, and recording what it was asked.
    async fn stub_returning(albums: &'static str) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let calls: Arc<Mutex<Vec<(String, String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        let recorded = calls.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let Some((method, path, body)) = read_request(&mut socket).await else {
                        return;
                    };
                    let created = r#"{"id":"album-new","ownerId":"me","name":"Trip","description":null,"coverAssetId":null,"assetCount":0,"createdAt":"2024-01-01T00:00:00Z","updatedAt":"2024-01-01T00:00:00Z","shareSlug":null}"#;
                    let payload = if method == "post" { created } else { albums };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    recorded.lock().unwrap().push((method, path, body));
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });

        Stub { base_url, calls }
    }

    /// The method and path asked for, and the body sent with them.
    async fn read_request(socket: &mut TcpStream) -> Option<(String, String, String)> {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            let read = socket.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(at) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break at;
            }
        };

        let head = String::from_utf8_lossy(&buffer[..head_end]).to_ascii_lowercase();
        let mut words = head.split_whitespace();
        let method = words.next()?.to_string();
        let path = words.next()?.split('?').next()?.to_string();
        let expected: usize = head
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    (name.trim() == "content-length").then(|| value.trim().parse().ok())?
                })
            })
            .unwrap_or(0);

        let mut body = buffer[head_end + 4..].to_vec();
        while body.len() < expected {
            let read = socket.read(&mut chunk).await.ok()?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        Some((method, path, String::from_utf8_lossy(&body).to_string()))
    }
}
