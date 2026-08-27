//! Listing, showing, editing and trashing photographs.

use anyhow::{bail, Result};
use imogen_sdk::{
    Asset, AssetFilter, AssetSelection, AssetStatus, AssetType, AssetUpdate, GeoPoint, TilePage,
    TimelineBucket, TimelineBucketQuery, TimelineQuery, TimelineTile,
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

/// Every photograph under a filter, to the last one.
///
/// `GET /albums/{id}` and `GET /people/{id}` hand back a capped cover sample now, so
/// anything that has to enumerate a whole album or a whole person walks the timeline
/// instead — the same surface the web grid pages, under an `albumId` or `personId`
/// filter. The day buckets say which periods exist; each period is then followed by its
/// cursor to the end. Nothing here caps or limits: a short list feeding `xargs` is a
/// silently wrong list.
pub async fn all_tiles(ctx: &Context, filter: &AssetFilter) -> Result<Vec<TimelineTile>> {
    let timeline = ctx
        .client
        .assets
        .timeline(&TimelineQuery {
            covers: None,
            filter: filter.clone(),
        })
        .await?;
    collect_tiles(&timeline.buckets, |period, cursor| async move {
        let page = ctx
            .client
            .assets
            .timeline_bucket(&TimelineBucketQuery {
                period,
                cursor,
                // Unset, so the server's own page size applies rather than a guess here.
                limit: None,
                filter: filter.clone(),
            })
            .await?;
        Ok(page)
    })
    .await
}

/// The walk itself, over whatever fetches a page — the network in earnest, a fake under
/// test.
async fn collect_tiles<F, Fut>(
    buckets: &[TimelineBucket],
    mut fetch: F,
) -> Result<Vec<TimelineTile>>
where
    F: FnMut(String, Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<TilePage>>,
{
    let mut tiles = Vec::new();
    for period in periods_of(buckets) {
        let mut cursor = None;
        loop {
            let page = fetch(period.clone(), cursor).await?;
            tiles.extend(page.items);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
    }
    Ok(tiles)
}

/// The months the day buckets fall in, each once, in the order the timeline gave them.
///
/// Months rather than days because the bucket endpoint takes either, and a month is one
/// round trip where its days would be thirty.
fn periods_of(buckets: &[TimelineBucket]) -> Vec<String> {
    let mut periods: Vec<String> = Vec::new();
    for bucket in buckets {
        let period: String = bucket.date.chars().take(7).collect();
        if !periods.contains(&period) {
            periods.push(period);
        }
    }
    periods
}

/// The rows a timeline tile can fill.
///
/// A tile is the grid's lean projection, and it carries no filename, no size, and no
/// archived or trashed mark. Those columns are left out rather than printed empty:
/// hydrating a whole album into `Asset`s to fill them would be one request per
/// photograph. `imogen ls --album <name> --all` still walks `GET /assets` and prints the
/// full row for anyone who wants it.
pub fn print_tiles(ctx: &Context, tiles: &[TimelineTile], ids_only: bool) -> Result<()> {
    if ids_only {
        for tile in tiles {
            ctx.out.value(&tile.id);
        }
        return Ok(());
    }
    if tiles.is_empty() {
        ctx.out.note("Nothing matched.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = tiles
        .iter()
        .map(|tile| {
            vec![
                tile.id.clone(),
                output::date(&tile.captured_at),
                match tile.r#type {
                    AssetType::Image => "photo".into(),
                    AssetType::Video => match tile.duration {
                        Some(seconds) => format!("video, {seconds:.0}s"),
                        None => "video".into(),
                    },
                },
                tile_flags(ctx, tile),
            ]
        })
        .collect();

    ctx.out.table(&["ID", "TAKEN", "KIND", ""], &rows);
    ctx.out.note(format!("\n{} shown", tiles.len()));
    Ok(())
}

/// The marks a tile can carry. Archived and trashed are absent by construction: the
/// timeline excludes both unless asked for them, and a tile could not say so anyway.
fn tile_flags(ctx: &Context, tile: &TimelineTile) -> String {
    let mut marks = Vec::new();
    if tile.favorite {
        marks.push(ctx.out.paint("★", YELLOW));
    }
    match tile.status {
        AssetStatus::Ready => {}
        AssetStatus::Failed => marks.push(ctx.out.paint("failed", RED)),
        AssetStatus::Pending => marks.push(ctx.out.dim("pending")),
        AssetStatus::Processing => marks.push(ctx.out.dim("processing")),
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
    if !args.ids.is_empty() {
        let mut count = 0u64;
        for chunk in args.ids.chunks(500) {
            let result = ctx.client.assets.trash(&AssetSelection::ids(chunk)).await?;
            count += result.count;
        }
        return report_count(ctx, count, "moved to the trash");
    }
    if args.query.is_empty() {
        bail!("Name some asset ids, or give a filter such as --query or --album");
    }

    let filter = ctx.to_filter(&args.query).await?;
    let count = ctx.count(&filter).await?;
    if count == 0 {
        ctx.out.note("Nothing matched.");
        return Ok(());
    }
    if !ctx.confirm(
        &format!(
            "Move {} to the trash?",
            output::plural(count as usize, "photograph")
        ),
        args.yes || ctx.out.is_json(),
    )? {
        ctx.out.note("Left alone.");
        return Ok(());
    }

    let result = ctx
        .client
        .assets
        .trash(&AssetSelection {
            query: Some(filter),
            ..Default::default()
        })
        .await?;
    report_count(ctx, result.count, "moved to the trash")
}

pub async fn restore(ctx: &Context, args: &RestoreArgs) -> Result<()> {
    if !args.ids.is_empty() {
        let mut count = 0u64;
        for chunk in args.ids.chunks(500) {
            let result = ctx
                .client
                .assets
                .restore(&AssetSelection::ids(chunk))
                .await?;
            count += result.count;
        }
        return report_count(ctx, count, "restored");
    }

    let filter = AssetFilter {
        trashed: Some(true),
        ..Default::default()
    };
    let count = ctx.count(&filter).await?;
    if count == 0 {
        ctx.out.note("The trash is empty.");
        return Ok(());
    }
    if !ctx.confirm(
        &format!(
            "Restore all {} from the trash?",
            output::plural(count as usize, "photograph")
        ),
        args.yes || ctx.out.is_json(),
    )? {
        ctx.out.note("Left alone.");
        return Ok(());
    }

    let result = ctx
        .client
        .assets
        .restore(&AssetSelection {
            query: Some(filter),
            ..Default::default()
        })
        .await?;
    report_count(ctx, result.count, "restored")
}

fn report_count(ctx: &Context, count: u64, verb: &str) -> Result<()> {
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "count": count }));
    }
    ctx.out.note(ctx.out.paint(
        &format!("{} {verb}.", output::plural(count as usize, "photograph")),
        GREEN,
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use imogen_sdk::TimelineBucket;
    use std::collections::HashMap;

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

    fn tile(id: &str) -> TimelineTile {
        TimelineTile {
            id: id.into(),
            captured_at: "2011-08-15T00:00:00.000Z".into(),
            width: None,
            height: None,
            r#type: AssetType::Image,
            status: AssetStatus::Ready,
            favorite: false,
            duration: None,
            placeholder_color: None,
            live_photo_video_id: None,
        }
    }

    /// A stand-in for `GET /assets/timeline/bucket`: months, each holding one or more
    /// pages, with the page index standing in for the opaque cursor.
    fn pages_of(month: &str) -> Vec<Vec<&'static str>> {
        let library: HashMap<&str, Vec<Vec<&'static str>>> = HashMap::from([
            ("2011-08", vec![vec!["a", "b"]]),
            // Three pages: a month heavy enough that one round trip is not the whole of it.
            ("2011-07", vec![vec!["c", "d"], vec!["e", "f"], vec!["g"]]),
            ("2010-12", vec![vec!["h"]]),
        ]);
        library
            .get(month)
            .unwrap_or_else(|| panic!("asked for {month}, which the timeline never listed"))
            .clone()
    }

    /// The whole point of the walk: an album or a person is enumerated to the last
    /// photograph, because the ids feed `xargs` and a short list is a silently wrong one.
    ///
    /// Two ways to get this wrong are both pinned here — stopping at the first page of a
    /// month, and stopping at the first month — because either leaves a list that looks
    /// perfectly well-formed.
    #[tokio::test]
    async fn every_page_of_every_month_is_walked() {
        let buckets = days(&[
            "2011-08-15",
            "2011-08-02",
            "2011-07-30",
            "2011-07-01",
            "2010-12-31",
        ]);

        let tiles = collect_tiles(&buckets, |period, cursor| async move {
            let pages = pages_of(&period);
            let index: usize = cursor.map(|c| c.parse().unwrap()).unwrap_or(0);
            Ok(TilePage {
                items: pages[index].iter().map(|id| tile(id)).collect(),
                next_cursor: (index + 1 < pages.len()).then(|| (index + 1).to_string()),
                total: None,
            })
        })
        .await
        .unwrap();

        assert_eq!(
            tiles.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c", "d", "e", "f", "g", "h"],
            "every tile in every month, in timeline order"
        );
    }

    /// Days are what the timeline hands back and months are what the bucket endpoint
    /// takes, so the walk asks for each month once rather than for each day.
    #[test]
    fn the_months_are_asked_for_once_each_newest_first() {
        assert_eq!(
            periods_of(&days(&[
                "2011-08-15",
                "2011-08-02",
                "2011-07-30",
                "2011-07-01",
                "2010-12-31",
            ])),
            vec!["2011-08", "2011-07", "2010-12"]
        );
        assert!(periods_of(&[]).is_empty());
    }
}
