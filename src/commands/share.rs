//! Public links.

use anyhow::Result;
use imogen_sdk::{ShareLink, ShareLinkCreate};
use serde_json::json;

use crate::cli::{ShareCommand, ShareKind};
use crate::context::Context;
use crate::output::GREEN;

pub async fn run(ctx: &Context, command: &ShareCommand) -> Result<()> {
    match command {
        ShareCommand::Create {
            kind,
            id,
            expires,
            password,
            no_download,
        } => {
            let input = ShareLinkCreate {
                expires_at: match expires {
                    Some(when) => Some(Some(crate::dates::to_timestamp(when)?)),
                    None => None,
                },
                allow_download: !no_download,
                password: password.clone().map(Some),
            };
            let link = match kind {
                ShareKind::Photo => ctx.client.assets.share(id, &input).await?,
                ShareKind::Album => {
                    let album = ctx.find_album(id).await?;
                    ctx.client.albums.share(&album.id, &input).await?
                }
            };
            if ctx.out.is_json() {
                return ctx.out.json(&link);
            }
            ctx.out.value(&link.url);
            describe(ctx, &link);
            Ok(())
        }

        ShareCommand::Show { kind, id } => {
            let link = match kind {
                ShareKind::Photo => ctx.client.assets.share_link(id).await?,
                ShareKind::Album => {
                    let album = ctx.find_album(id).await?;
                    ctx.client.albums.share_link(&album.id).await?
                }
            };
            match link {
                Some(link) => {
                    if ctx.out.is_json() {
                        return ctx.out.json(&link);
                    }
                    ctx.out.value(&link.url);
                    describe(ctx, &link);
                }
                None => {
                    if ctx.out.is_json() {
                        return ctx.out.json(&json!(null));
                    }
                    ctx.out.note("Not published.");
                }
            }
            Ok(())
        }

        ShareCommand::Revoke { kind, id } => {
            match kind {
                ShareKind::Photo => ctx.client.assets.unshare(id).await?,
                ShareKind::Album => {
                    let album = ctx.find_album(id).await?;
                    ctx.client.albums.unshare(&album.id).await?
                }
            }
            if ctx.out.is_json() {
                return ctx.out.json(&json!({ "revoked": true }));
            }
            ctx.out.note(ctx.out.paint("No longer published.", GREEN));
            Ok(())
        }
    }
}

fn describe(ctx: &Context, link: &ShareLink) {
    ctx.out.fields(&[
        (
            "expires",
            link.expires_at
                .as_deref()
                .map(crate::output::datetime)
                .unwrap_or_else(|| "never".into()),
        ),
        (
            "downloads",
            if link.allow_download { "on" } else { "off" }.to_string(),
        ),
    ]);
}
