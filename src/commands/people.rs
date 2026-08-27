//! People, as grouped by face recognition.

use anyhow::Result;
use imogen_sdk::{AssetFilter, PersonUpdate};
use serde_json::json;

use crate::cli::PeopleCommand;
use crate::context::Context;
use crate::output::{self, GREEN};

pub async fn run(ctx: &Context, command: &PeopleCommand) -> Result<()> {
    match command {
        PeopleCommand::List { hidden } => list(ctx, *hidden).await,
        PeopleCommand::Show { person, ids } => show(ctx, person, *ids).await,
        PeopleCommand::Name { person, name } => rename(ctx, person, name).await,
        PeopleCommand::Hide { person, undo } => hide(ctx, person, !*undo).await,
        PeopleCommand::Merge { keep, merge } => merge_people(ctx, keep, merge).await,
        PeopleCommand::Faces { asset } => faces(ctx, asset).await,
        PeopleCommand::Status => status(ctx).await,
        PeopleCommand::Enable { off } => enable(ctx, !*off).await,
    }
}

async fn list(ctx: &Context, hidden: bool) -> Result<()> {
    let people = ctx.client.people.list(hidden).await?;
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "items": people }));
    }
    if people.is_empty() {
        ctx.out
            .note("Nobody has been grouped. Face grouping may be switched off — try `imogen people status`.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = people
        .iter()
        .map(|person| {
            vec![
                person.id.clone(),
                person
                    .name
                    .clone()
                    .unwrap_or_else(|| ctx.out.dim("unnamed")),
                person.photo_count.to_string(),
                if person.hidden {
                    "hidden".into()
                } else {
                    String::new()
                },
            ]
        })
        .collect();
    // FACES, not PHOTOS: `photoCount` on the wire is the server's `faceCount`, and one
    // photograph holding two faces of the same person counts twice in it.
    ctx.out.table(&["ID", "NAME", "FACES", ""], &rows);
    Ok(())
}

/// The person, and every photograph they appear in.
///
/// Two things were wrong here at once. The photos on `GET /people/{id}` are a capped
/// cover sample, so this pages the timeline under a `personId` filter instead — every
/// one, because `imogen people show alice --ids` feeds a pipeline. And `photoCount` on
/// the wire is the server's `faceCount`: it counts faces, so a photograph with two of
/// this person's faces in it counts twice. It is still worth showing, but only under its
/// own name. "photographs" is the number of rows below it; "faces" is the other number.
async fn show(ctx: &Context, reference: &str, ids_only: bool) -> Result<()> {
    let person = ctx.find_person(reference).await?;
    let tiles = crate::commands::assets::all_tiles(
        ctx,
        &AssetFilter {
            person_id: Some(person.id.clone()),
            ..Default::default()
        },
    )
    .await?;

    if ctx.out.is_json() {
        return ctx.out.json(&json!({
            "person": person,
            "items": tiles,
            "count": tiles.len(),
            "faceCount": person.photo_count,
        }));
    }
    if ids_only {
        return crate::commands::assets::print_tiles(ctx, &tiles, true);
    }
    ctx.out.heading(person.name.as_deref().unwrap_or("unnamed"));
    ctx.out.fields(&[
        ("id", person.id.clone()),
        ("photographs", tiles.len().to_string()),
        ("faces", person.photo_count.to_string()),
    ]);
    ctx.out.line("");
    crate::commands::assets::print_tiles(ctx, &tiles, false)
}

async fn rename(ctx: &Context, reference: &str, name: &str) -> Result<()> {
    let person = ctx.find_person(reference).await?;
    ctx.client
        .people
        .update(
            &person.id,
            &PersonUpdate {
                name: Some(Some(name.to_string())),
                hidden: None,
            },
        )
        .await?;
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "id": person.id, "name": name }));
    }
    ctx.out
        .note(ctx.out.paint(&format!("Named “{name}”."), GREEN));
    Ok(())
}

async fn hide(ctx: &Context, reference: &str, hidden: bool) -> Result<()> {
    let person = ctx.find_person(reference).await?;
    ctx.client
        .people
        .update(
            &person.id,
            &PersonUpdate {
                name: None,
                hidden: Some(hidden),
            },
        )
        .await?;
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "id": person.id, "hidden": hidden }));
    }
    ctx.out.note(
        ctx.out
            .paint(if hidden { "Hidden." } else { "Shown again." }, GREEN),
    );
    Ok(())
}

async fn merge_people(ctx: &Context, keep: &str, merge: &[String]) -> Result<()> {
    let kept = ctx.find_person(keep).await?;
    let mut ids = Vec::new();
    for reference in merge {
        ids.push(ctx.find_person(reference).await?.id);
    }
    let result = ctx.client.people.merge(&kept.id, &ids).await?;
    if ctx.out.is_json() {
        return ctx.out.json(&result);
    }
    ctx.out.note(ctx.out.paint(
        &format!(
            "Moved {} face(s) onto {}.",
            result.moved,
            kept.name.as_deref().unwrap_or(&kept.id)
        ),
        GREEN,
    ));
    Ok(())
}

async fn faces(ctx: &Context, asset: &str) -> Result<()> {
    let faces = ctx.client.people.faces_in(asset).await?;
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "items": faces }));
    }
    if faces.is_empty() {
        ctx.out.note("No faces found in that photograph.");
        return Ok(());
    }
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
    Ok(())
}

async fn status(ctx: &Context) -> Result<()> {
    let status = ctx.client.people.status().await?;
    if ctx.out.is_json() {
        return ctx.out.json(&status);
    }
    ctx.out.fields(&[
        (
            "face grouping",
            if status.enabled { "on" } else { "off" }.to_string(),
        ),
        (
            "models",
            if status.models_ready {
                "downloaded".to_string()
            } else {
                let have: u64 = status.models.iter().map(|m| m.bytes).sum();
                let want: u64 = status.models.iter().map(|m| m.expected_bytes).sum();
                format!("{} of {}", output::bytes(have), output::bytes(want))
            },
        ),
        ("people", status.people_count.to_string()),
        ("waiting to be scanned", status.pending.to_string()),
    ]);
    Ok(())
}

async fn enable(ctx: &Context, enabled: bool) -> Result<()> {
    ctx.client.people.set_enabled(enabled).await?;
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "enabled": enabled }));
    }
    ctx.out.note(ctx.out.paint(
        if enabled {
            "Face grouping is on. The models download in the background, then the library is scanned."
        } else {
            "Face grouping is off."
        },
        GREEN,
    ));
    Ok(())
}
