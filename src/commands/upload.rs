//! Sending files to the library.
//!
//! Two ways in. Flags describe a whole run, which is what a person typing a command wants.
//! A manifest describes each file separately, which is what a script moving a library in
//! from somewhere else wants: it has already worked out each photograph's date, place and
//! description, and needs somewhere to put them.

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context as _, Result};
use futures::stream::{self, StreamExt};
use imogen_sdk::{AssetSelection, AssetUploadMetadata, GeoPoint, UploadOptions};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::cli::UploadArgs;
use crate::context::Context;
use crate::output::{self, GREEN};

/// Extensions imogen stores. A folder of photographs usually has a `Thumbs.db` and a
/// `.DS_Store` in it too, and sending those only to have them refused wastes the
/// bandwidth of whichever is larger.
pub const MEDIA_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "avif", "tif", "tiff", "bmp", "dng",
    "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf", "srw", "pef", "mp4", "m4v", "mov", "webm",
    "avi", "mkv", "3gp", "mpg", "mpeg", "mts", "m2ts",
];

pub fn is_media(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| MEDIA_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// One line of a manifest: a file, and what the caller already knows about it.
///
/// Field names are the API's own, so a script that has a photograph's metadata in the
/// shape the server uses does not have to rename anything on the way through.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestEntry {
    pub path: PathBuf,
    /// ISO-8601, a plain date, or seconds since the epoch.
    #[serde(default)]
    pub captured_at: Option<serde_json::Value>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub location: Option<GeoPoint>,
    #[serde(default)]
    pub favorite: Option<bool>,
    /// What the photograph is called in the library, whatever the file on disk is named.
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub device_asset_id: Option<String>,
    /// An album to file it into, made if the name is new.
    #[serde(default)]
    pub album: Option<String>,
}

/// A file to send, and what to say about it.
struct Job {
    path: PathBuf,
    metadata: AssetUploadMetadata,
    album: Option<String>,
    size: u64,
}

pub async fn upload(ctx: &Context, args: &UploadArgs) -> Result<()> {
    let jobs = match &args.manifest {
        Some(manifest) => from_manifest(manifest, args)?,
        None => from_paths(args)?,
    };

    if jobs.is_empty() {
        ctx.out.note("Nothing to upload.");
        return Ok(());
    }
    if (args.filename.is_some() || args.device_id.is_some()) && jobs.len() > 1 {
        bail!(
            "--filename and --device-id name one photograph, but {} were selected",
            jobs.len()
        );
    }

    let total_bytes: u64 = jobs.iter().map(|job| job.size).sum();
    if args.dry_run {
        return report_plan(ctx, &jobs, total_bytes);
    }

    // Every album named across the run is made up front: creating one lazily from six
    // concurrent uploads makes six albums of the same name.
    let mut album_ids: BTreeMap<String, String> = BTreeMap::new();
    let mut names: Vec<String> = jobs.iter().filter_map(|job| job.album.clone()).collect();
    names.sort();
    names.dedup();
    for name in names {
        let album = ctx.album_or_create(&name, None).await?;
        album_ids.insert(name, album.id);
    }

    let report = match &args.report {
        Some(path) => Some(Arc::new(Mutex::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("Could not write the report at {}", path.display()))?,
        ))),
        None => None,
    };

    let progress = progress_bar(ctx, jobs.len() as u64);
    let sent = Arc::new(AtomicU64::new(0));

    let outcomes = stream::iter(jobs)
        .map(|job| {
            let progress = progress.clone();
            let sent = sent.clone();
            let report = report.clone();
            async move {
                let options = UploadOptions {
                    metadata: job.metadata.clone(),
                    on_progress: None,
                };
                let result = ctx.client.assets.upload(&job.path, &options).await;

                if let Some(report) = &report {
                    let line = match &result {
                        Ok(outcome) => json!({
                            "path": job.path,
                            "ok": true,
                            "id": outcome.asset.id,
                            "duplicate": outcome.duplicate,
                        }),
                        Err(error) => json!({
                            "path": job.path,
                            "ok": false,
                            "error": error.to_string(),
                        }),
                    };
                    // Flushed per line so an interrupted run leaves a usable record.
                    let mut file = report.lock().await;
                    let _ = writeln!(file, "{line}");
                    let _ = file.flush();
                }

                if let Some(bar) = &progress {
                    bar.inc(1);
                    let total = sent.fetch_add(job.size, Ordering::Relaxed) + job.size;
                    bar.set_message(output::bytes(total));
                }
                (job, result)
            }
        })
        .buffer_unordered(args.concurrency.max(1))
        .collect::<Vec<_>>()
        .await;

    if let Some(bar) = &progress {
        bar.finish_and_clear();
    }

    let mut uploaded = Vec::new();
    let mut duplicates = 0usize;
    let mut failures = Vec::new();
    let mut by_album: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (job, result) in outcomes {
        match result {
            Ok(outcome) => {
                if outcome.duplicate {
                    duplicates += 1;
                }
                if let Some(id) = job.album.as_ref().and_then(|name| album_ids.get(name)) {
                    by_album
                        .entry(id.clone())
                        .or_default()
                        .push(outcome.asset.id.clone());
                }
                uploaded.push(outcome.asset);
            }
            Err(error) => failures.push(json!({
                "path": job.path.display().to_string(),
                "error": error.to_string(),
            })),
        }
    }

    let mut added = 0u64;
    for (album_id, asset_ids) in &by_album {
        // Album membership goes on in chunks: an imported album can hold thousands.
        for chunk in asset_ids.chunks(500) {
            match ctx
                .client
                .albums
                .add_assets(album_id, &AssetSelection::ids(chunk))
                .await
            {
                Ok(result) => added += result.added,
                Err(error) => ctx.out.warn(format!("Could not fill an album: {error}")),
            }
        }
    }

    if ctx.out.is_json() {
        ctx.out.json(&json!({
            "uploaded": uploaded.len() - duplicates,
            "duplicates": duplicates,
            "failed": failures.len(),
            "addedToAlbums": added,
            "items": uploaded,
            "failures": failures,
        }))?;
    } else {
        for failure in &failures {
            ctx.out.warn(format!(
                "{}: {}",
                failure["path"].as_str().unwrap_or_default(),
                failure["error"].as_str().unwrap_or_default()
            ));
        }
        let summary = format!(
            "Uploaded {}{}{}.",
            output::plural(uploaded.len() - duplicates, "file"),
            if duplicates > 0 {
                format!(", {duplicates} already there")
            } else {
                String::new()
            },
            if added > 0 {
                format!(", {added} filed into albums")
            } else {
                String::new()
            }
        );
        ctx.out.note(ctx.out.paint(&summary, GREEN));
    }

    if !failures.is_empty() {
        bail!("{} failed", output::plural(failures.len(), "file"));
    }
    Ok(())
}

fn report_plan(ctx: &Context, jobs: &[Job], total_bytes: u64) -> Result<()> {
    if ctx.out.is_json() {
        let items: Vec<_> = jobs
            .iter()
            .map(|job| json!({ "path": job.path, "metadata": job.metadata, "album": job.album }))
            .collect();
        return ctx.out.json(&json!({
            "count": items.len(),
            "bytes": total_bytes,
            "items": items,
        }));
    }
    for job in jobs {
        ctx.out.value(job.path.display().to_string());
    }
    ctx.out.note(format!(
        "\n{}, {}",
        output::plural(jobs.len(), "file"),
        output::bytes(total_bytes)
    ));
    Ok(())
}

/// Files named on the command line, all carrying the same metadata.
fn from_paths(args: &UploadArgs) -> Result<Vec<Job>> {
    if args.paths.is_empty() {
        bail!("Name some files or folders, or pass --manifest");
    }
    let shared = shared_metadata(args)?;
    let files = collect(&args.paths, args.recursive)?;

    files
        .into_iter()
        .map(|path| {
            let mut metadata = shared.clone();
            if args.device_ids {
                metadata.device_asset_id = Some(relative_id(&path, &args.paths));
            }
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            Ok(Job {
                metadata,
                album: args.album.clone(),
                path,
                size,
            })
        })
        .collect()
}

/// One JSON object per line. A blank line is skipped; a line that will not parse stops the
/// run before anything is sent, because a manifest half-understood is worse than refused.
fn from_manifest(manifest: &Path, args: &UploadArgs) -> Result<Vec<Job>> {
    let text = if manifest == Path::new("-") {
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        buffer
    } else {
        std::fs::read_to_string(manifest)
            .with_context(|| format!("Could not read {}", manifest.display()))?
    };

    let shared = shared_metadata(args)?;
    let mut jobs = Vec::new();
    for (index, line) in text.as_bytes().lines().enumerate() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: ManifestEntry = serde_json::from_str(line)
            .with_context(|| format!("Line {} of the manifest is not a valid entry", index + 1))?;

        let mut metadata = shared.clone();
        // The entry is the more specific statement, so it wins over the run-wide flags.
        if let Some(captured_at) = &entry.captured_at {
            metadata.captured_at =
                Some(timestamp_from(captured_at).with_context(|| {
                    format!("Line {} of the manifest: bad capturedAt", index + 1)
                })?);
        }
        if entry.description.is_some() {
            metadata.description = entry.description.clone();
        }
        if entry.location.is_some() {
            metadata.location = entry.location.clone();
        }
        if entry.favorite.is_some() {
            metadata.favorite = entry.favorite;
        }
        if entry.filename.is_some() {
            metadata.filename = entry.filename.clone();
        }
        if entry.device_asset_id.is_some() {
            metadata.device_asset_id = entry.device_asset_id.clone();
        }

        if !entry.path.is_file() {
            bail!(
                "Line {} of the manifest: {} is not a file",
                index + 1,
                entry.path.display()
            );
        }
        let size = std::fs::metadata(&entry.path).map(|m| m.len()).unwrap_or(0);
        jobs.push(Job {
            album: entry.album.clone().or_else(|| args.album.clone()),
            path: entry.path,
            metadata,
            size,
        });
    }
    Ok(jobs)
}

/// The metadata the flags describe, which every file in the run starts from.
fn shared_metadata(args: &UploadArgs) -> Result<AssetUploadMetadata> {
    Ok(AssetUploadMetadata {
        device_asset_id: args.device_id.clone(),
        captured_at: args
            .captured_at
            .as_deref()
            .map(crate::dates::to_timestamp)
            .transpose()?,
        favorite: args.favorite.then_some(true),
        filename: args.filename.clone(),
        description: args.description.clone(),
        location: args
            .location
            .as_deref()
            .map(crate::commands::assets::parse_location)
            .transpose()?,
    })
}

/// A manifest may state a capture time as text or as a number of seconds, because the
/// thing a script is converting from has usually chosen one or the other already.
fn timestamp_from(value: &serde_json::Value) -> Result<String> {
    match value {
        serde_json::Value::String(text) => crate::dates::to_timestamp(text),
        serde_json::Value::Number(number) => match number.as_i64() {
            Some(seconds) => crate::dates::from_unix_seconds(seconds),
            None => bail!("{number} is not a whole number of seconds"),
        },
        other => bail!("{other} is not a time"),
    }
}

/// Every file a set of paths names. A file given explicitly is taken at its word; a file
/// found by walking a folder has to look like media.
pub fn collect(paths: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for path in paths {
        if !path.exists() {
            bail!("{} is not there", path.display());
        }
        if path.is_file() {
            found.push(path.clone());
            continue;
        }
        let walker = walkdir::WalkDir::new(path).max_depth(if recursive { usize::MAX } else { 1 });
        for entry in walker.into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file() && is_media(entry.path()) {
                found.push(entry.path().to_path_buf());
            }
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// A stable name for a file within the folder it came from, so the same photograph sent
/// from another machine is recognised rather than stored twice.
pub fn relative_id(path: &Path, roots: &[PathBuf]) -> String {
    for root in roots {
        if let Ok(relative) = path.strip_prefix(root) {
            if let Some(text) = relative.to_str() {
                return text.to_string();
            }
        }
    }
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string()
}

pub fn progress_bar(ctx: &Context, files: u64) -> Option<ProgressBar> {
    if ctx.out.quiet || ctx.out.is_json() || !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        return None;
    }
    let bar = ProgressBar::new(files);
    bar.set_style(
        ProgressStyle::with_template("{bar:32} {pos}/{len} files  {msg}")
            .unwrap()
            .progress_chars("━━╸"),
    );
    Some(bar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_time_may_be_text_or_seconds() {
        assert_eq!(
            timestamp_from(&json!("2024-06-01")).unwrap(),
            "2024-06-01T12:00:00Z"
        );
        assert_eq!(
            timestamp_from(&json!(1717233000)).unwrap(),
            "2024-06-01T09:10:00Z"
        );
        assert!(timestamp_from(&json!(true)).is_err());
    }

    #[test]
    fn a_manifest_entry_uses_the_apis_own_field_names() {
        let entry: ManifestEntry = serde_json::from_str(
            r#"{"path":"/tmp/a.jpg","capturedAt":1717233000,"favorite":true,
                "location":{"latitude":50.1,"longitude":-5.5},"album":"Cornwall"}"#,
        )
        .unwrap();
        assert_eq!(entry.album.as_deref(), Some("Cornwall"));
        assert_eq!(entry.location.unwrap().latitude, 50.1);
        assert_eq!(entry.favorite, Some(true));
    }

    #[test]
    fn a_misspelled_field_is_refused_rather_than_ignored() {
        // Silently dropping `capturedat` would import a whole library with wrong dates.
        let result = serde_json::from_str::<ManifestEntry>(
            r#"{"path":"/tmp/a.jpg","capturedat":"2024-06-01"}"#,
        );
        assert!(result.is_err());
    }
}
