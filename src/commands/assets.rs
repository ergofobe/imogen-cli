//! Listing, showing, editing and trashing photographs.

use anyhow::{bail, Result};
use imogen_sdk::{
    Asset, AssetSelection, AssetStatus, AssetType, AssetUpdate, GeoPoint, TimelineQuery,
};
use serde_json::json;

use crate::cli::{EditArgs, ListArgs, RestoreArgs, SearchArgs, ShowArgs, TrashArgs};
use crate::context::Context;
use crate::dates;
use crate::output::{self, GREEN, RED, YELLOW};

pub async fn list(ctx: &Context, args: &ListArgs) -> Result<()> {
    let limit = (!args.all).then_some(args.limit);
    let assets = ctx.matching(&args.query, limit).await?;
    print_assets(ctx, &assets, args.ids)
}

pub async fn search(ctx: &Context, args: &SearchArgs) -> Result<()> {
    let mut query = args.query.clone();
    query.query = Some(args.text.clone());
    let limit = (!args.all).then_some(args.limit);
    let assets = ctx.matching(&query, limit).await?;
    print_assets(ctx, &assets, args.ids)
}

pub fn print_assets(ctx: &Context, assets: &[Asset], ids_only: bool) -> Result<()> {
    if ctx.out.is_json() {
        return ctx
            .out
            .json(&json!({ "items": assets, "count": assets.len() }));
    }
    if ids_only {
        for asset in assets {
            ctx.out.value(&asset.id);
        }
        return Ok(());
    }
    if assets.is_empty() {
        ctx.out.note("Nothing matched.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = assets
        .iter()
        .map(|asset| {
            vec![
                asset.id.clone(),
                output::date(&asset.captured_at),
                output::truncate(&asset.original_filename, 36),
                match asset.r#type {
                    AssetType::Image => "photo".into(),
                    AssetType::Video => "video".into(),
                },
                output::bytes(asset.size_bytes),
                flags(ctx, asset),
            ]
        })
        .collect();

    ctx.out
        .table(&["ID", "TAKEN", "FILENAME", "KIND", "SIZE", ""], &rows);
    ctx.out.note(format!("\n{} shown", assets.len()));
    Ok(())
}

fn flags(ctx: &Context, asset: &Asset) -> String {
    let mut marks = Vec::new();
    if asset.favorite {
        marks.push(ctx.out.paint("★", YELLOW));
    }
    if asset.archived {
        marks.push(ctx.out.dim("archived"));
    }
    if asset.deleted_at.is_some() {
        marks.push(ctx.out.paint("trashed", RED));
    }
    match asset.status {
        AssetStatus::Ready => {}
        AssetStatus::Failed => marks.push(ctx.out.paint("failed", RED)),
        other => marks.push(ctx.out.dim(match other {
            AssetStatus::Pending => "pending",
            AssetStatus::Processing => "processing",
            _ => "",
        })),
    }
    marks.join(" ")
}

pub async fn show(ctx: &Context, args: &ShowArgs) -> Result<()> {
    let asset = ctx.client.assets.get(&args.id).await?;
    let faces = if args.faces {
        ctx.client
            .people
            .faces_in(&asset.id)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "asset": asset, "faces": faces }));
    }

    if args.image {
        match crate::media::render_asset(ctx, &asset.id, args.rows).await {
            Ok(drawing) => println!("{drawing}"),
            Err(error) => ctx
                .out
                .warn(format!("Could not draw the photograph: {error}")),
        }
    }

    ctx.out.heading(&asset.original_filename);
    let dimensions = match (asset.width, asset.height) {
        (Some(w), Some(h)) => format!("{w} × {h}"),
        _ => String::new(),
    };
    let location = asset
        .location
        .as_ref()
        .map(|point| match &point.place {
            Some(place) => format!("{place} ({:.5}, {:.5})", point.latitude, point.longitude),
            None => format!("{:.5}, {:.5}", point.latitude, point.longitude),
        })
        .unwrap_or_default();
    let camera = asset
        .exif
        .as_ref()
        .map(|exif| {
            [
                exif.make.as_deref(),
                exif.model.as_deref(),
                exif.lens.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
        })
        .unwrap_or_default();
    let exposure = asset
        .exif
        .as_ref()
        .map(|exif| {
            let mut parts = Vec::new();
            if let Some(f) = exif.f_number {
                parts.push(format!("f/{f}"));
            }
            if let Some(t) = exif.exposure_time {
                parts.push(if t >= 1.0 {
                    format!("{t}s")
                } else {
                    format!("1/{}s", (1.0 / t).round())
                });
            }
            if let Some(iso) = exif.iso {
                parts.push(format!("ISO {iso}"));
            }
            if let Some(focal) = exif.focal_length {
                parts.push(format!("{focal}mm"));
            }
            parts.join("  ")
        })
        .unwrap_or_default();

    ctx.out.fields(&[
        ("id", asset.id.clone()),
        (
            "taken",
            format!(
                "{}{}",
                output::datetime(&asset.captured_at),
                if asset.captured_at_is_exact {
                    String::new()
                } else {
                    ctx.out.dim("  (estimated)")
                }
            ),
        ),
        (
            "corrected from",
            asset
                .captured_at_original
                .as_deref()
                .map(output::datetime)
                .unwrap_or_default(),
        ),
        ("uploaded", output::datetime(&asset.created_at)),
        (
            "kind",
            match asset.r#type {
                AssetType::Image => "photo".into(),
                AssetType::Video => format!(
                    "video{}",
                    asset
                        .duration
                        .map(|d| format!(", {d:.0}s"))
                        .unwrap_or_default()
                ),
            },
        ),
        ("size", output::bytes(asset.size_bytes)),
        ("dimensions", dimensions),
        ("type", asset.mime_type.clone()),
        ("camera", camera),
        ("exposure", exposure),
        ("where", location),
        ("description", asset.description.clone().unwrap_or_default()),
        (
            "status",
            match asset.status {
                AssetStatus::Ready => String::new(),
                AssetStatus::Failed => ctx.out.paint("failed to process", RED),
                AssetStatus::Pending => "waiting to be processed".into(),
                AssetStatus::Processing => "being processed".into(),
            },
        ),
        (
            "flags",
            [
                asset.favorite.then_some("favourite"),
                asset.archived.then_some("archived"),
                asset.deleted_at.is_some().then_some("in the trash"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", "),
        ),
        ("checksum", ctx.out.dim(&asset.checksum)),
        (
            "device id",
            asset.device_asset_id.clone().unwrap_or_default(),
        ),
    ]);

    if !faces.is_empty() {
        ctx.out.line("");
        ctx.out.heading("faces");
        let rows: Vec<Vec<String>> = faces
            .iter()
            .map(|face| {
                vec![
                    face.id.clone(),
                    face.person_name.clone().unwrap_or_else(|| "—".into()),
                    format!("{:.0}%", face.score * 100.0),
                    format!("{}×{} at {},{}", face.width, face.height, face.x, face.y),
                ]
            })
            .collect();
        ctx.out.table(&["FACE", "WHO", "SCORE", "BOX"], &rows);
    }
    Ok(())
}

pub async fn stats(ctx: &Context) -> Result<()> {
    let stats = ctx.client.assets.stats().await?;
    if ctx.out.is_json() {
        return ctx.out.json(&stats);
    }
    ctx.out.fields(&[
        ("photographs", stats.asset_count.to_string()),
        ("images", stats.image_count.to_string()),
        ("videos", stats.video_count.to_string()),
        ("albums", stats.album_count.to_string()),
        ("favourites", stats.favorite_count.to_string()),
        ("in the trash", stats.trashed_count.to_string()),
        ("storage", output::bytes(stats.storage_bytes)),
        (
            "earliest",
            stats
                .earliest_captured_at
                .as_deref()
                .map(output::date)
                .unwrap_or_default(),
        ),
        (
            "latest",
            stats
                .latest_captured_at
                .as_deref()
                .map(output::date)
                .unwrap_or_default(),
        ),
    ]);
    Ok(())
}

/// The days inside a pair of bounds, widened the way every other filter widens them.
///
/// A bucket's date is a whole day, and a bound may be less than one: `--before 2011` means
/// the end of 2011, not the string "2011". Compared raw, `"2011-08-15" <= "2011"` is false
/// and the command answers with nothing — it hid whole years in silence, which on a
/// twenty-year library is the one answer that looks like a working command.
fn within(
    buckets: Vec<imogen_sdk::TimelineBucket>,
    after: Option<&str>,
    before: Option<&str>,
) -> Vec<imogen_sdk::TimelineBucket> {
    let from = after.map(|bound| day_of(&dates::to_start_of_day(bound)));
    let until = before.map(|bound| day_of(&dates::to_end_of_day(bound)));
    buckets
        .into_iter()
        .filter(|bucket| {
            from.as_deref()
                .is_none_or(|from| bucket.date.as_str() >= from)
        })
        .filter(|bucket| {
            until
                .as_deref()
                .is_none_or(|until| bucket.date.as_str() <= until)
        })
        .collect()
}

/// The `YYYY-MM-DD` out of an instant, which is the grain a day bucket is keyed by.
fn day_of(timestamp: &str) -> String {
    timestamp.split('T').next().unwrap_or_default().to_string()
}

pub async fn timeline(ctx: &Context, after: Option<&str>, before: Option<&str>) -> Result<()> {
    let timeline = ctx
        .client
        .assets
        .timeline(&TimelineQuery::default())
        .await?;
    let buckets = within(timeline.buckets, after, before);

    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "buckets": buckets }));
    }
    let peak = buckets.iter().map(|b| b.count).max().unwrap_or(1).max(1);
    let rows: Vec<Vec<String>> = buckets
        .iter()
        .map(|bucket| {
            let width = ((bucket.count as f64 / peak as f64) * 32.0).round() as usize;
            vec![
                bucket.date.clone(),
                format!("{:>5}", bucket.count),
                ctx.out.dim(&"▇".repeat(width.max(1))),
            ]
        })
        .collect();
    ctx.out.table(&["DAY", "COUNT", ""], &rows);
    Ok(())
}

pub async fn edit(ctx: &Context, args: &EditArgs) -> Result<()> {
    let patch = build_patch(args)?;
    if patch == AssetUpdate::default() {
        bail!("Nothing to change. Try --favorite, --description, --captured-at or --location.");
    }

    let targets = ctx.select(&args.ids, &args.select.to_query(), None).await?;
    if targets.is_empty() {
        ctx.out.note("Nothing matched.");
        return Ok(());
    }
    if args.ids.is_empty()
        && !ctx.confirm(
            &format!("Edit {}?", output::plural(targets.len(), "photograph")),
            args.yes || ctx.out.is_json(),
        )?
    {
        ctx.out.note("Left alone.");
        return Ok(());
    }

    let mut updated = Vec::new();
    let mut failures = Vec::new();
    for id in &targets {
        match ctx.client.assets.update(id, &patch).await {
            Ok(asset) => updated.push(asset),
            Err(error) => failures.push(json!({ "id": id, "error": error.to_string() })),
        }
    }

    if ctx.out.is_json() {
        return ctx.out.json(&json!({
            "updated": updated.len(),
            "failed": failures.len(),
            "items": updated,
            "failures": failures,
        }));
    }
    for failure in &failures {
        ctx.out.warn(failure.to_string());
    }
    ctx.out.note(ctx.out.paint(
        &format!("Edited {}.", output::plural(updated.len(), "photograph")),
        GREEN,
    ));
    Ok(())
}

fn build_patch(args: &EditArgs) -> Result<AssetUpdate> {
    let mut patch = AssetUpdate::default();
    if args.favorite {
        patch.favorite = Some(true);
    }
    if args.no_favorite {
        patch.favorite = Some(false);
    }
    if args.archive {
        patch.archived = Some(true);
    }
    if args.unarchive {
        patch.archived = Some(false);
    }
    if let Some(description) = &args.description {
        patch.description = Some(Some(description.clone()));
    }
    if args.clear_description {
        patch.description = Some(None);
    }
    if let Some(captured_at) = &args.captured_at {
        patch.captured_at = Some(crate::dates::to_timestamp(captured_at)?);
    }
    if args.reset_captured_at {
        patch.reset_captured_at = Some(true);
    }
    if let Some(location) = &args.location {
        patch.location = Some(Some(parse_location(location)?));
    }
    if args.clear_location {
        patch.location = Some(None);
    }
    Ok(patch)
}

/// `50.1109,-5.5372` or `50.1109,-5.5372,12.5`.
pub fn parse_location(input: &str) -> Result<GeoPoint> {
    let parts: Vec<&str> = input.split(',').map(str::trim).collect();
    if parts.len() < 2 || parts.len() > 3 {
        bail!("A location is lat,lon or lat,lon,altitude — got \"{input}\"");
    }
    let latitude: f64 = parts[0].parse()?;
    let longitude: f64 = parts[1].parse()?;
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        bail!("{input} is not a point on Earth");
    }
    Ok(GeoPoint {
        latitude,
        longitude,
        altitude: parts.get(2).map(|a| a.parse()).transpose()?,
        place: None,
    })
}

pub async fn trash(ctx: &Context, args: &TrashArgs) -> Result<()> {
    let targets = ctx.select(&args.ids, &args.query, None).await?;
    if targets.is_empty() {
        ctx.out.note("Nothing matched.");
        return Ok(());
    }
    if args.ids.is_empty()
        && !ctx.confirm(
            &format!(
                "Move {} to the trash?",
                output::plural(targets.len(), "photograph")
            ),
            args.yes || ctx.out.is_json(),
        )?
    {
        ctx.out.note("Left alone.");
        return Ok(());
    }

    let result = ctx
        .client
        .assets
        .trash(&AssetSelection::ids(&targets))
        .await?;
    if ctx.out.is_json() {
        return ctx.out.json(&result);
    }
    ctx.out.note(ctx.out.paint(
        &format!(
            "{} moved to the trash.",
            output::plural(result.count as usize, "photograph")
        ),
        GREEN,
    ));
    Ok(())
}

pub async fn restore(ctx: &Context, args: &RestoreArgs) -> Result<()> {
    let targets = if args.ids.is_empty() {
        let query = crate::cli::QueryArgs {
            trashed: true,
            ..Default::default()
        };
        let assets = ctx.matching(&query, None).await?;
        if assets.is_empty() {
            ctx.out.note("The trash is empty.");
            return Ok(());
        }
        if !ctx.confirm(
            &format!(
                "Restore all {} from the trash?",
                output::plural(assets.len(), "photograph")
            ),
            args.yes || ctx.out.is_json(),
        )? {
            ctx.out.note("Left alone.");
            return Ok(());
        }
        assets.into_iter().map(|asset| asset.id).collect()
    } else {
        args.ids.clone()
    };

    let result = ctx
        .client
        .assets
        .restore(&AssetSelection::ids(&targets))
        .await?;
    if ctx.out.is_json() {
        return ctx.out.json(&result);
    }
    ctx.out.note(ctx.out.paint(
        &format!(
            "{} restored.",
            output::plural(result.count as usize, "photograph")
        ),
        GREEN,
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use imogen_sdk::TimelineBucket;

    fn days(dates: &[&str]) -> Vec<TimelineBucket> {
        dates
            .iter()
            .map(|date| TimelineBucket {
                date: (*date).into(),
                count: 1,
                cover_asset_id: None,
            })
            .collect()
    }

    fn kept(after: Option<&str>, before: Option<&str>) -> Vec<String> {
        within(
            days(&[
                "2012-01-04",
                "2011-12-31",
                "2011-08-15",
                "2011-01-01",
                "2010-12-31",
            ]),
            after,
            before,
        )
        .into_iter()
        .map(|bucket| bucket.date)
        .collect()
    }

    /// A bound less than a whole day means the whole of that period. Compared raw,
    /// `"2011-08-15" <= "2011"` is false, so `--before 2011` answered with nothing at all
    /// and looked like a library with no photographs in it.
    #[test]
    fn a_bare_year_covers_the_whole_year() {
        assert_eq!(
            kept(None, Some("2011")),
            vec!["2011-12-31", "2011-08-15", "2011-01-01", "2010-12-31"]
        );
        assert_eq!(
            kept(Some("2011"), None),
            vec!["2012-01-04", "2011-12-31", "2011-08-15", "2011-01-01"]
        );
        assert_eq!(
            kept(Some("2011"), Some("2011")),
            vec!["2011-12-31", "2011-08-15", "2011-01-01"],
            "both bounds together are the whole year and nothing else"
        );
    }

    /// And the same for a bare month, whose last day is not the same in every month.
    #[test]
    fn a_bare_month_covers_the_whole_month() {
        assert_eq!(
            kept(None, Some("2011-08")),
            vec!["2011-08-15", "2011-01-01", "2010-12-31"]
        );
        assert_eq!(kept(Some("2011-08"), Some("2011-08")), vec!["2011-08-15"]);
        assert_eq!(
            within(days(&["2011-02-28"]), None, Some("2011-02"))
                .into_iter()
                .map(|bucket| bucket.date)
                .collect::<Vec<_>>(),
            vec!["2011-02-28"],
            "February ends on its own last day"
        );
    }

    #[test]
    fn a_whole_day_is_still_taken_at_its_word() {
        assert_eq!(
            kept(Some("2011-08-15"), Some("2011-08-15")),
            vec!["2011-08-15"]
        );
        assert_eq!(kept(None, None).len(), 5, "no bounds keeps everything");
    }
}
