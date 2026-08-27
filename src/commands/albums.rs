//! Albums, and the links that publish them.

use anyhow::Result;
use imogen_sdk::{AlbumCreate, AlbumUpdate, AssetFilter, AssetSelection};
use serde_json::json;

use crate::cli::{AlbumCommand, QueryArgs};
use crate::context::Context;
use crate::output::GREEN;

pub async fn run(ctx: &Context, command: &AlbumCommand) -> Result<()> {
    match command {
        AlbumCommand::List => list(ctx).await,
        AlbumCommand::Show { album, ids } => show(ctx, album, *ids).await,
        AlbumCommand::Create {
            name,
            description,
            assets,
        } => create(ctx, name, description.as_deref(), assets).await,
        AlbumCommand::Update {
            album,
            name,
            description,
            clear_description,
            cover,
        } => {
            update(
                ctx,
                album,
                name.as_deref(),
                description.as_deref(),
                *clear_description,
                cover.as_deref(),
            )
            .await
        }
        AlbumCommand::Delete { album, yes } => delete(ctx, album, *yes).await,
        AlbumCommand::Add {
            target,
            assets,
            query,
        } => add(ctx, target, assets, query).await,
        AlbumCommand::Remove { album, assets } => remove(ctx, album, assets).await,
    }
}

async fn list(ctx: &Context) -> Result<()> {
    let albums = ctx.client.albums.list().await?;
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "items": albums }));
    }
    if albums.is_empty() {
        ctx.out.note("No albums yet.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = albums
        .iter()
        .map(|album| {
            vec![
                album.id.clone(),
                crate::output::truncate(&album.name, 40),
                album.asset_count.to_string(),
                crate::output::date(&album.updated_at),
                album
                    .share_slug
                    .as_ref()
                    .map(|_| "public".to_string())
                    .unwrap_or_default(),
            ]
        })
        .collect();
    ctx.out
        .table(&["ID", "NAME", "PHOTOS", "UPDATED", ""], &rows);
    Ok(())
}

/// The album, and every photograph in it.
///
/// The assets on `GET /albums/{id}` are a capped cover sample now — sixty of them,
/// however many the album holds — so this pages the timeline under an `albumId` filter
/// instead. It has to be every one: `imogen album show holidays --ids | xargs imogen
/// trash` is a real pipeline, and a list quietly cut to sixty would trash sixty.
///
/// The header counts what was actually fetched rather than the album's own
/// `assetCount`, so the number above the rows is the number of rows. The two agree in
/// the ordinary case — both leave out the trashed, the archived and the vaulted — and
/// where they would not, the honest number is the one belonging to the list printed.
async fn show(ctx: &Context, reference: &str, ids_only: bool) -> Result<()> {
    let album = ctx.find_album(reference).await?;
    let tiles = crate::commands::assets::all_tiles(
        ctx,
        &AssetFilter {
            album_id: Some(album.id.clone()),
            ..Default::default()
        },
    )
    .await?;

    if ctx.out.is_json() {
        return ctx.out.json(&json!({
            "album": album,
            "items": tiles,
            "count": tiles.len(),
        }));
    }
    if ids_only {
        return crate::commands::assets::print_tiles(ctx, &tiles, true);
    }
    ctx.out.heading(&album.name);
    ctx.out.fields(&[
        ("id", album.id.clone()),
        ("description", album.description.clone().unwrap_or_default()),
        ("photographs", tiles.len().to_string()),
        ("created", crate::output::date(&album.created_at)),
        (
            "public link",
            album
                .share_slug
                .as_ref()
                .map(|slug| format!("{}/share/{slug}", ctx.server))
                .unwrap_or_default(),
        ),
    ]);
    ctx.out.line("");
    crate::commands::assets::print_tiles(ctx, &tiles, false)
}

async fn create(
    ctx: &Context,
    name: &str,
    description: Option<&str>,
    assets: &[String],
) -> Result<()> {
    let album = ctx
        .client
        .albums
        .create(&AlbumCreate {
            name: name.to_string(),
            description: description.map(str::to_string),
            asset_ids: (!assets.is_empty()).then(|| assets.to_vec()),
        })
        .await?;
    if ctx.out.is_json() {
        return ctx.out.json(&album);
    }
    ctx.out.value(&album.id);
    ctx.out
        .note(ctx.out.paint(&format!("Made “{}”.", album.name), GREEN));
    Ok(())
}

async fn update(
    ctx: &Context,
    reference: &str,
    name: Option<&str>,
    description: Option<&str>,
    clear_description: bool,
    cover: Option<&str>,
) -> Result<()> {
    let album = ctx.find_album(reference).await?;
    let patch = AlbumUpdate {
        name: name.map(str::to_string),
        description: if clear_description {
            Some(None)
        } else {
            description.map(|d| Some(d.to_string()))
        },
        cover_asset_id: cover.map(|c| Some(c.to_string())),
    };
    let updated = ctx.client.albums.update(&album.id, &patch).await?;
    if ctx.out.is_json() {
        return ctx.out.json(&updated);
    }
    ctx.out.note(
        ctx.out
            .paint(&format!("Updated “{}”.", updated.name), GREEN),
    );
    Ok(())
}

async fn delete(ctx: &Context, reference: &str, yes: bool) -> Result<()> {
    let album = ctx.find_album(reference).await?;
    if !ctx.confirm(
        &format!(
            "Delete “{}”? The {} photographs in it are not deleted.",
            album.name, album.asset_count
        ),
        yes || ctx.out.is_json(),
    )? {
        ctx.out.note("Left alone.");
        return Ok(());
    }
    ctx.client.albums.remove(&album.id).await?;
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "deleted": album.id }));
    }
    ctx.out
        .note(ctx.out.paint(&format!("Deleted “{}”.", album.name), GREEN));
    Ok(())
}

async fn add(ctx: &Context, reference: &str, assets: &[String], query: &QueryArgs) -> Result<()> {
    let album = ctx.find_album(reference).await?;
    let ids = ctx.select(assets, query, None).await?;
    if ids.is_empty() {
        ctx.out.note("Nothing matched.");
        return Ok(());
    }
    let mut added = 0u64;
    let mut skipped = 0u64;
    let mut count = 0u64;
    for chunk in ids.chunks(500) {
        let result = ctx
            .client
            .albums
            .add_assets(&album.id, &AssetSelection::ids(chunk))
            .await?;
        added += result.added;
        skipped += result.skipped;
        count = result.asset_count;
    }
    if ctx.out.is_json() {
        return ctx.out.json(&json!({
            "added": added,
            "skipped": skipped,
            "assetCount": count,
        }));
    }
    ctx.out.note(ctx.out.paint(
        &format!(
            "Added {added} to “{}”{}.",
            album.name,
            if skipped > 0 {
                format!(", {skipped} were already in it")
            } else {
                String::new()
            }
        ),
        GREEN,
    ));
    Ok(())
}

async fn remove(ctx: &Context, reference: &str, assets: &[String]) -> Result<()> {
    let album = ctx.find_album(reference).await?;
    let result = ctx
        .client
        .albums
        .remove_assets(&album.id, &AssetSelection::ids(assets))
        .await?;
    if ctx.out.is_json() {
        return ctx.out.json(&result);
    }
    ctx.out.note(ctx.out.paint(
        &format!("Took {} out of “{}”.", result.removed, album.name),
        GREEN,
    ));
    Ok(())
}
