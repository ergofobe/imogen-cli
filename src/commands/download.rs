//! Fetching photographs back out.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use futures::stream::{self, StreamExt};
use imogen_sdk::Asset;
use serde_json::json;

use crate::cli::DownloadArgs;
use crate::context::Context;
use crate::output::{self, GREEN};

pub async fn download(ctx: &Context, args: &DownloadArgs) -> Result<()> {
    let assets = resolve(ctx, args).await?;
    if assets.is_empty() {
        ctx.out.note("Nothing matched.");
        return Ok(());
    }

    let variant = args.variant.into();
    let mut planned: Vec<(Asset, PathBuf)> = Vec::new();
    let mut taken: HashSet<PathBuf> = HashSet::new();
    for asset in assets {
        let path = unique(
            &args.out.join(layout(&args.layout, &asset, args)),
            &mut taken,
        );
        planned.push((asset, path));
    }

    if args.dry_run {
        if ctx.out.is_json() {
            let items: Vec<_> = planned
                .iter()
                .map(|(asset, path)| json!({ "id": asset.id, "path": path }))
                .collect();
            return ctx
                .out
                .json(&json!({ "items": items, "count": items.len() }));
        }
        for (_, path) in &planned {
            ctx.out.value(path.display().to_string());
        }
        ctx.out
            .note(format!("\n{}", output::plural(planned.len(), "file")));
        return Ok(());
    }

    let progress = crate::commands::upload::progress_bar(ctx, planned.len() as u64);
    let outcomes = stream::iter(planned)
        .map(|(asset, path)| {
            let progress = progress.clone();
            async move {
                let result = if path.exists() && !args.overwrite {
                    Ok(None)
                } else {
                    ctx.client
                        .assets
                        .download_to(&asset.id, variant, &path, None)
                        .await
                        .map(Some)
                };
                if let Some(bar) = &progress {
                    bar.inc(1);
                }
                (asset, path, result)
            }
        })
        .buffer_unordered(args.concurrency.max(1))
        .collect::<Vec<_>>()
        .await;

    if let Some(bar) = &progress {
        bar.finish_and_clear();
    }

    let mut written = Vec::new();
    let mut skipped = 0usize;
    let mut failures = Vec::new();
    for (asset, path, result) in outcomes {
        match result {
            Ok(Some(bytes)) => written.push(json!({
                "id": asset.id,
                "path": path,
                "bytes": bytes,
            })),
            Ok(None) => skipped += 1,
            Err(error) => failures.push(json!({
                "id": asset.id,
                "path": path,
                "error": error.to_string(),
            })),
        }
    }

    if ctx.out.is_json() {
        return ctx.out.json(&json!({
            "written": written.len(),
            "skipped": skipped,
            "failed": failures.len(),
            "items": written,
            "failures": failures,
        }));
    }
    for failure in &failures {
        ctx.out.warn(format!(
            "{}: {}",
            failure["id"].as_str().unwrap_or_default(),
            failure["error"].as_str().unwrap_or_default()
        ));
    }
    let bytes: u64 = written
        .iter()
        .filter_map(|item| item["bytes"].as_u64())
        .sum();
    ctx.out.note(ctx.out.paint(
        &format!(
            "Wrote {}, {}{}.",
            output::plural(written.len(), "file"),
            output::bytes(bytes),
            if skipped > 0 {
                format!(", {skipped} already there")
            } else {
                String::new()
            }
        ),
        GREEN,
    ));
    if !failures.is_empty() {
        bail!("{} failed", output::plural(failures.len(), "file"));
    }
    Ok(())
}

async fn resolve(ctx: &Context, args: &DownloadArgs) -> Result<Vec<Asset>> {
    if !args.ids.is_empty() {
        let mut assets = Vec::new();
        for id in &args.ids {
            assets.push(ctx.client.assets.get(id).await?);
        }
        return Ok(assets);
    }
    if args.query.is_empty() {
        bail!("Name some asset ids, or give a filter such as --query or --album");
    }
    ctx.matching(&args.query, args.limit).await
}

/// Fills in a layout template. Anything the template does not name is left out, so
/// `--layout '{id}{ext}'` gives a flat folder of stable names and the default gives a
/// year/month tree that looks like the library.
fn layout(template: &str, asset: &Asset, args: &DownloadArgs) -> PathBuf {
    let (year, month, day) = crate::dates::parts(&asset.captured_at);
    let filename = sanitize(&asset.original_filename);
    let (stem, extension) = match filename.rsplit_once('.') {
        Some((stem, extension)) => (stem.to_string(), format!(".{extension}")),
        None => (filename.clone(), String::new()),
    };
    // A derived rendition is a WebP whatever the original was; saying otherwise would
    // hand somebody a `.heic` no viewer can open.
    let extension = match args.variant {
        crate::cli::Variant::Original => extension,
        _ => ".webp".to_string(),
    };

    let filled = template
        .replace("{yyyy}", &year)
        .replace("{mm}", &month)
        .replace("{dd}", &day)
        .replace("{id}", &asset.id)
        .replace("{stem}", &stem)
        .replace("{name}", &format!("{stem}{extension}"))
        .replace("{ext}", &extension)
        .replace("{album}", &args.query.album.clone().unwrap_or_default());

    PathBuf::from(filled.trim_start_matches('/'))
}

/// Two photographs can honestly share a filename. Rather than one silently overwriting
/// the other, the second gets a suffix.
fn unique(path: &Path, taken: &mut HashSet<PathBuf>) -> PathBuf {
    if taken.insert(path.to_path_buf()) {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string();
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    for index in 2..10_000 {
        let candidate = path.with_file_name(format!("{stem}-{index}{extension}"));
        if taken.insert(candidate.clone()) {
            return candidate;
        }
    }
    path.to_path_buf()
}

/// A filename the local filesystem will accept, whatever the camera called it.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim_matches(['.', ' ']).to_string();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filename_from_a_camera_cannot_escape_the_output_folder() {
        // Separators become underscores and the leading dots are trimmed, so nothing
        // that arrives as a filename can climb out of the folder it was aimed at.
        assert_eq!(sanitize("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize(""), "file");
        assert_eq!(sanitize("harbour.jpg"), "harbour.jpg");
    }

    #[test]
    fn a_repeated_destination_gets_a_suffix() {
        let mut taken = HashSet::new();
        let path = PathBuf::from("a/b.jpg");
        assert_eq!(unique(&path, &mut taken), PathBuf::from("a/b.jpg"));
        assert_eq!(unique(&path, &mut taken), PathBuf::from("a/b-2.jpg"));
        assert_eq!(unique(&path, &mut taken), PathBuf::from("a/b-3.jpg"));
    }
}
