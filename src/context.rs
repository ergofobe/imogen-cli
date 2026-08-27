//! What every command is handed: a client, a way to print, and the few lookups that would
//! otherwise be repeated in a dozen places.

use std::io::{IsTerminal, Write};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context as _, Result};
use futures::StreamExt;
use imogen_sdk::{
    Album, Asset, AssetQuery, AssetSort, AssetType, ClientOptions, ImogenClient, Person, SortOrder,
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

    /// Translates the command-line filters into the API's query, resolving an album given
    /// by name into its id on the way through.
    pub async fn to_query(&self, args: &QueryArgs) -> Result<AssetQuery> {
        let album_id = match &args.album {
            Some(reference) => Some(self.find_album(reference).await?.id),
            None => None,
        };
        Ok(AssetQuery {
            cursor: None,
            limit: None,
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

    /// An album by id, or by enough of its name to be unambiguous. Naming one is what a
    /// person will actually do; refusing an ambiguous name is better than picking one.
    pub async fn find_album(&self, reference: &str) -> Result<Album> {
        let albums = self.client.albums.list().await?;
        if let Some(exact) = albums.iter().find(|album| album.id == reference) {
            return Ok(exact.clone());
        }
        let lowered = reference.to_lowercase();
        if let Some(named) = albums
            .iter()
            .find(|album| album.name.to_lowercase() == lowered)
        {
            return Ok(named.clone());
        }
        let matches: Vec<&Album> = albums
            .iter()
            .filter(|album| album.name.to_lowercase().contains(&lowered))
            .collect();
        match matches.len() {
            1 => Ok(matches[0].clone()),
            0 => Err(anyhow!(
                "No album called \"{reference}\". `imogen album list` shows them, and `imogen album create` makes one."
            )),
            _ => {
                let names: Vec<&str> = matches.iter().map(|a| a.name.as_str()).collect();
                Err(anyhow!(
                    "\"{reference}\" matches several albums: {}",
                    names.join(", ")
                ))
            }
        }
    }

    /// The album of that name, made if it is not there yet. A description is only used
    /// when the album is new: it never overwrites one somebody has already written.
    pub async fn album_or_create(
        &self,
        reference: &str,
        description: Option<&str>,
    ) -> Result<Album> {
        match self.find_album(reference).await {
            Ok(album) => Ok(album),
            Err(_) => self
                .client
                .albums
                .create(&imogen_sdk::AlbumCreate {
                    name: reference.to_string(),
                    description: description.map(str::to_string),
                    ..Default::default()
                })
                .await
                .context("Could not create the album"),
        }
    }

    pub async fn find_person(&self, reference: &str) -> Result<Person> {
        let people = self.client.people.list(true).await?;
        if let Some(exact) = people.iter().find(|person| person.id == reference) {
            return Ok(exact.clone());
        }
        let lowered = reference.to_lowercase();
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
