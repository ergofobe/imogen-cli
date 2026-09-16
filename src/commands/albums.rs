//! Albums, and the links that publish them.

use anyhow::{bail, Result};
use imogen_sdk::{AlbumCreate, AlbumUpdate, AssetFilter, AssetSelection};
use serde_json::json;

use crate::cli::{AlbumCommand, QueryArgs};
use crate::context::{album_name, Context};
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
    let name = album_name(name)?;
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
    // Before the album is even looked up: a rename that could never be undone by name is
    // refused whatever else the command was asked to change.
    let name = name.map(album_name).transpose()?;
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

    if !assets.is_empty() {
        let mut added = 0u64;
        let mut skipped = 0u64;
        let mut count = 0u64;
        for chunk in assets.chunks(500) {
            let result = ctx
                .client
                .albums
                .add_assets(&album.id, &AssetSelection::ids(chunk))
                .await?;
            added += result.added;
            skipped += result.skipped;
            count = result.asset_count;
        }
        return report_added(ctx, &album.name, added, skipped, count);
    }
    if query.is_empty() {
        bail!("Name some asset ids, or give a filter such as --query or --album");
    }

    let filter = ctx.to_filter(query).await?;
    let matched = ctx.count(&filter).await?;
    if matched == 0 {
        ctx.out.note("Nothing matched.");
        return Ok(());
    }
    let result = ctx
        .client
        .albums
        .add_assets(
            &album.id,
            &AssetSelection {
                query: Some(filter),
                ..Default::default()
            },
        )
        .await?;
    report_added(
        ctx,
        &album.name,
        result.added,
        result.skipped,
        result.asset_count,
    )
}

fn report_added(ctx: &Context, name: &str, added: u64, skipped: u64, count: u64) -> Result<()> {
    if ctx.out.is_json() {
        return ctx.out.json(&json!({
            "added": added,
            "skipped": skipped,
            "assetCount": count,
        }));
    }
    ctx.out.note(ctx.out.paint(
        &format!(
            "Added {added} to “{name}”{}.",
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
    if assets.is_empty() {
        ctx.out.note("Nothing matched.");
        return Ok(());
    }
    let album = ctx.find_album(reference).await?;
    let mut removed = 0u64;
    for chunk in assets.chunks(500) {
        let result = ctx
            .client
            .albums
            .remove_assets(&album.id, &AssetSelection::ids(chunk))
            .await?;
        removed += result.removed;
    }
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "removed": removed }));
    }
    ctx.out.note(
        ctx.out
            .paint(&format!("Took {removed} out of “{}”.", album.name), GREEN),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use crate::cli::GlobalArgs;
    use crate::config::Profile;

    const ONE_ALBUM: &str = r#"{"items":[
        {"id":"album-1","ownerId":"me","name":"Holiday","description":null,
         "coverAssetId":null,"assetCount":9,"createdAt":"2024-01-01T00:00:00Z",
         "updatedAt":"2024-01-01T00:00:00Z","shareSlug":null}
    ]}"#;

    #[tokio::test]
    async fn an_album_cannot_be_created_without_a_name() {
        // `imogen album create "$NAME"` with the variable unset. The name was sent as it
        // stood, and the album that came back could never be named again: `find_album`
        // refuses an empty reference, and no non-empty one matches an empty name.
        for name in ["", "   "] {
            let stub = stub().await;
            let error = create(&context(&stub.base_url), name, None, &[])
                .await
                .expect_err("an empty name is not a name");
            assert!(
                error.to_string().contains("An album needs a name"),
                "said {error} instead of naming the problem"
            );
            assert!(
                stub.calls().is_empty(),
                "nothing should reach the wire for a name that could never be used again"
            );
        }
    }

    #[tokio::test]
    async fn an_album_cannot_be_renamed_to_nothing() {
        // The same write through the other door: an album that had a usable name loses it.
        for name in ["", "   "] {
            let stub = stub().await;
            let error = update(
                &context(&stub.base_url),
                "album-1",
                Some(name),
                None,
                false,
                None,
            )
            .await
            .expect_err("an empty name is not a name");
            assert!(
                error.to_string().contains("An album needs a name"),
                "said {error} instead of naming the problem"
            );
            assert!(
                stub.calls().is_empty(),
                "the refusal comes before the album is even looked up"
            );
        }
    }

    #[tokio::test]
    async fn a_new_album_is_created_under_the_trimmed_name() {
        // `album create` has to agree with `upload --album`, which already creates under
        // the trimmed name: otherwise the same string makes two albums that a listing
        // cannot tell apart. `named_album` compares the stored name trimmed, so a padded
        // one answers to something it is not spelled as.
        let stub = stub().await;
        create(&context(&stub.base_url), " Trip\n", None, &[])
            .await
            .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&stub.sent("post")[0]).unwrap();
        assert_eq!(sent["name"], "Trip");
    }

    #[tokio::test]
    async fn a_rename_is_stored_trimmed_too() {
        let stub = stub().await;
        update(
            &context(&stub.base_url),
            "album-1",
            Some(" Trip\n"),
            None,
            false,
            None,
        )
        .await
        .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&stub.sent("patch")[0]).unwrap();
        assert_eq!(sent["name"], "Trip");
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
        calls: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Stub {
        /// Every request, as method and body.
        fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().clone()
        }

        /// The bodies written by that method, which is what the album was named.
        fn sent(&self, method: &str) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter(|(sent, _)| sent == method)
                .map(|(_, body)| body)
                .collect()
        }
    }

    /// A stub imogen over a library of one album, speaking just enough HTTP/1.1 to list
    /// albums, make one and change one, and recording what it was asked.
    async fn stub() -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        let recorded = calls.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let Some((method, body)) = read_request(&mut socket).await else {
                        return;
                    };
                    let one = r#"{"id":"album-1","ownerId":"me","name":"Trip","description":null,"coverAssetId":null,"assetCount":0,"createdAt":"2024-01-01T00:00:00Z","updatedAt":"2024-01-01T00:00:00Z","shareSlug":null}"#;
                    let payload = if method == "get" { ONE_ALBUM } else { one };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    recorded.lock().unwrap().push((method, body));
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });

        Stub { base_url, calls }
    }

    /// The method asked for, and the body sent with it.
    async fn read_request(socket: &mut TcpStream) -> Option<(String, String)> {
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
        let method = head.split_whitespace().next()?.to_string();
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
        Some((method, String::from_utf8_lossy(&body).to_string()))
    }
}
