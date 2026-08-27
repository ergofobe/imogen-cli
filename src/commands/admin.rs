//! Server administration.
//!
//! Every endpoint behind these commands answers 404 rather than 403 to somebody who is
//! not an administrator, so a "not found" here means "you may not", not "this is broken".

use anyhow::Result;
use imogen_sdk::{AdminUserUpdate, InviteCreate, ServerSettingsUpdate, SignsInWith, UserRole};
use serde_json::json;

use crate::cli::{AdminCommand, Role};
use crate::context::Context;
use crate::output::{self, GREEN, RED, YELLOW};

pub async fn run(ctx: &Context, command: &AdminCommand) -> Result<()> {
    match command {
        AdminCommand::Users => users(ctx).await,
        AdminCommand::User {
            id,
            role,
            disable,
            enable,
        } => update_user(ctx, id, *role, *disable, *enable).await,
        AdminCommand::DeleteUser { id, yes } => delete_user(ctx, id, *yes).await,
        AdminCommand::ResetPassword { id, password } => {
            ctx.client.admin.reset_password(id, password).await?;
            done(
                ctx,
                "Password set, and every session that account had is over.",
            )
        }
        AdminCommand::Invites => invites(ctx).await,
        AdminCommand::Invite { email, role, days } => {
            invite(ctx, email.as_deref(), *role, *days).await
        }
        AdminCommand::RevokeInvite { id } => {
            ctx.client.admin.revoke_invite(id).await?;
            done(ctx, "Invitation withdrawn.")
        }
        AdminCommand::Queue => queue(ctx).await,
        AdminCommand::Retry { id } => retry(ctx, id.as_deref()).await,
        AdminCommand::Discard { id } => {
            ctx.client.admin.discard_job(id).await?;
            done(ctx, "Job discarded.")
        }
        AdminCommand::Clients => clients(ctx).await,
        AdminCommand::RevokeClient { id } => {
            ctx.client.admin.revoke_client(id).await?;
            done(ctx, "Application removed, along with its tokens.")
        }
        AdminCommand::Sessions => sessions(ctx).await,
        AdminCommand::RevokeSession { id } => {
            ctx.client.admin.revoke_session(id).await?;
            done(ctx, "Session ended.")
        }
        AdminCommand::Storage => storage(ctx).await,
        AdminCommand::Settings {
            allow_signup,
            trash_retention_days,
            faces_enabled,
        } => settings(ctx, *allow_signup, *trash_retention_days, *faces_enabled).await,
        AdminCommand::Shares => shares(ctx).await,
        AdminCommand::RevokeShare { id } => {
            ctx.client.admin.revoke_share(id).await?;
            done(ctx, "Link closed.")
        }
    }
}

fn done(ctx: &Context, message: &str) -> Result<()> {
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "ok": true, "message": message }));
    }
    ctx.out.note(ctx.out.paint(message, GREEN));
    Ok(())
}

async fn users(ctx: &Context) -> Result<()> {
    let users = ctx.client.admin.users().await?;
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "items": users }));
    }
    let rows: Vec<Vec<String>> = users
        .iter()
        .map(|user| {
            vec![
                user.id.clone(),
                output::truncate(&user.email, 32),
                output::truncate(&user.name, 24),
                match user.role {
                    UserRole::Admin => ctx.out.paint("admin", YELLOW),
                    UserRole::User => "user".to_string(),
                },
                match user.signs_in_with {
                    SignsInWith::Password => "password".into(),
                    SignsInWith::Sso => "sso".into(),
                    SignsInWith::Both => "both".into(),
                },
                user.photo_count.to_string(),
                match user.quota_bytes {
                    Some(quota) => format!(
                        "{} / {}",
                        output::bytes(user.used_bytes),
                        output::bytes(quota)
                    ),
                    None => output::bytes(user.used_bytes),
                },
                if user.disabled {
                    ctx.out.paint("suspended", RED)
                } else {
                    String::new()
                },
            ]
        })
        .collect();
    ctx.out.table(
        &[
            "ID", "EMAIL", "NAME", "ROLE", "SIGN-IN", "PHOTOS", "USING", "",
        ],
        &rows,
    );
    Ok(())
}

async fn update_user(
    ctx: &Context,
    id: &str,
    role: Option<Role>,
    disable: bool,
    enable: bool,
) -> Result<()> {
    let patch = AdminUserUpdate {
        role: role.map(|r| match r {
            Role::Admin => UserRole::Admin,
            Role::User => UserRole::User,
        }),
        disabled: if disable {
            Some(true)
        } else if enable {
            Some(false)
        } else {
            None
        },
    };
    if patch == AdminUserUpdate::default() {
        ctx.out
            .note("Nothing to change. Try --role, --disable or --enable.");
        return Ok(());
    }
    let user = ctx.client.admin.update_user(id, &patch).await?;
    if ctx.out.is_json() {
        return ctx.out.json(&user);
    }
    ctx.out
        .note(ctx.out.paint(&format!("Updated {}.", user.email), GREEN));
    Ok(())
}

async fn delete_user(ctx: &Context, id: &str, yes: bool) -> Result<()> {
    if !ctx.confirm(
        "Remove this account? Its photographs go to the trash.",
        yes || ctx.out.is_json(),
    )? {
        ctx.out.note("Left alone.");
        return Ok(());
    }
    ctx.client.admin.delete_user(id).await?;
    done(ctx, "Account removed.")
}

async fn invites(ctx: &Context) -> Result<()> {
    let invites = ctx.client.admin.invites().await?;
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "items": invites }));
    }
    if invites.is_empty() {
        ctx.out.note("No invitations.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = invites
        .iter()
        .map(|invite| {
            vec![
                invite.id.clone(),
                invite.email.clone().unwrap_or_else(|| "anyone".into()),
                format!("{:?}", invite.role).to_lowercase(),
                format!("{:?}", invite.state).to_lowercase(),
                output::date(&invite.expires_at),
            ]
        })
        .collect();
    ctx.out
        .table(&["ID", "FOR", "ROLE", "STATE", "EXPIRES"], &rows);
    Ok(())
}

async fn invite(ctx: &Context, email: Option<&str>, role: Role, days: u32) -> Result<()> {
    let created = ctx
        .client
        .admin
        .create_invite(&InviteCreate {
            email: email.map(|e| Some(e.to_string())),
            role: match role {
                Role::Admin => UserRole::Admin,
                Role::User => UserRole::User,
            },
            expires_in_days: days,
        })
        .await?;
    let url = format!("{}/signup?invite={}", ctx.server, created.token);
    if ctx.out.is_json() {
        return ctx
            .out
            .json(&json!({ "invite": created.invite, "token": created.token, "url": url }));
    }
    ctx.out.value(&url);
    ctx.out.note(ctx.out.paint(
        "That link is shown once. It is stored only as a hash, so if it is lost, revoke it and make another.",
        YELLOW,
    ));
    Ok(())
}

async fn queue(ctx: &Context) -> Result<()> {
    let health = ctx.client.admin.queue().await?;
    if ctx.out.is_json() {
        return ctx.out.json(&health);
    }
    ctx.out.fields(&[
        ("queued", health.queued.to_string()),
        ("running", health.running.to_string()),
        (
            "failed",
            if health.failed > 0 {
                ctx.out.paint(&health.failed.to_string(), RED)
            } else {
                "0".to_string()
            },
        ),
        (
            "stuck",
            if health.stuck > 0 {
                ctx.out.paint(&health.stuck.to_string(), RED)
            } else {
                "0".to_string()
            },
        ),
        (
            "oldest waiting",
            health
                .oldest_queued_at
                .as_deref()
                .map(output::datetime)
                .unwrap_or_default(),
        ),
    ]);
    if !health.failures.is_empty() {
        ctx.out.line("");
        ctx.out.heading("failures");
        let rows: Vec<Vec<String>> = health
            .failures
            .iter()
            .map(|job| {
                vec![
                    job.id.clone(),
                    job.name.clone(),
                    format!("{}/{}", job.attempts, job.max_attempts),
                    output::truncate(job.last_error.as_deref().unwrap_or(""), 60),
                ]
            })
            .collect();
        ctx.out.table(&["ID", "JOB", "TRIES", "ERROR"], &rows);
    }
    Ok(())
}

async fn retry(ctx: &Context, id: Option<&str>) -> Result<()> {
    match id {
        Some(id) => {
            ctx.client.admin.retry_job(id).await?;
            done(ctx, "Job requeued.")
        }
        None => {
            let count = ctx.client.admin.retry_all_jobs().await?;
            if ctx.out.is_json() {
                return ctx.out.json(&json!({ "requeued": count }));
            }
            ctx.out
                .note(ctx.out.paint(&format!("{count} job(s) requeued."), GREEN));
            Ok(())
        }
    }
}

async fn clients(ctx: &Context) -> Result<()> {
    let clients = ctx.client.admin.clients().await?;
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "items": clients }));
    }
    let rows: Vec<Vec<String>> = clients
        .iter()
        .map(|client| {
            vec![
                client.id.clone(),
                output::truncate(&client.name, 28),
                client.scopes.join(" "),
                client.active_tokens.to_string(),
                if client.dynamically_registered {
                    ctx.out.dim("registered itself")
                } else {
                    "configured".to_string()
                },
            ]
        })
        .collect();
    ctx.out
        .table(&["ID", "NAME", "SCOPES", "TOKENS", "ORIGIN"], &rows);
    Ok(())
}

async fn sessions(ctx: &Context) -> Result<()> {
    let sessions = ctx.client.admin.sessions().await?;
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "items": sessions }));
    }
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|session| {
            vec![
                session.id.clone(),
                output::truncate(&session.user_email, 28),
                session.ip_address.clone().unwrap_or_default(),
                output::truncate(session.user_agent.as_deref().unwrap_or(""), 32),
                output::datetime(&session.last_used_at),
                if session.current {
                    ctx.out.paint("this one", YELLOW)
                } else {
                    String::new()
                },
            ]
        })
        .collect();
    ctx.out
        .table(&["ID", "WHO", "FROM", "CLIENT", "LAST USED", ""], &rows);
    Ok(())
}

async fn storage(ctx: &Context) -> Result<()> {
    let report = ctx.client.admin.storage().await?;
    if ctx.out.is_json() {
        return ctx.out.json(&report);
    }
    ctx.out.fields(&[
        ("data directory", report.data_dir.clone()),
        ("originals", output::bytes(report.original_bytes)),
        ("derivatives", output::bytes(report.derivative_bytes)),
        (
            "in the trash",
            format!(
                "{} in {} photograph(s), kept {} day(s)",
                output::bytes(report.trashed_bytes),
                report.trashed_count,
                report.trash_retention_days
            ),
        ),
        (
            "next sweep",
            report
                .next_sweep_at
                .as_deref()
                .map(output::datetime)
                .unwrap_or_default(),
        ),
        (
            "missing files",
            if report.missing_files > 0 {
                ctx.out.paint(&report.missing_files.to_string(), RED)
            } else {
                "0".to_string()
            },
        ),
    ]);
    if !report.per_user.is_empty() {
        ctx.out.line("");
        let rows: Vec<Vec<String>> = report
            .per_user
            .iter()
            .map(|user| {
                vec![
                    output::truncate(&user.email, 32),
                    user.photo_count.to_string(),
                    output::bytes(user.used_bytes),
                ]
            })
            .collect();
        ctx.out.table(&["WHO", "PHOTOS", "USING"], &rows);
    }
    Ok(())
}

async fn settings(
    ctx: &Context,
    allow_signup: Option<bool>,
    trash_retention_days: Option<u32>,
    faces_enabled: Option<bool>,
) -> Result<()> {
    let patch = ServerSettingsUpdate {
        allow_signup,
        trash_retention_days,
        faces_enabled,
    };
    let settings = if patch == ServerSettingsUpdate::default() {
        ctx.client.admin.settings().await?
    } else {
        ctx.client.admin.update_settings(&patch).await?
    };
    if ctx.out.is_json() {
        return ctx.out.json(&settings);
    }
    ctx.out.fields(&[
        (
            "anyone may sign up",
            if settings.allow_signup { "yes" } else { "no" }.to_string(),
        ),
        (
            "trash kept for",
            format!("{} day(s)", settings.trash_retention_days),
        ),
        (
            "face grouping",
            if settings.faces_enabled { "on" } else { "off" }.to_string(),
        ),
    ]);
    Ok(())
}

async fn shares(ctx: &Context) -> Result<()> {
    let shares = ctx.client.admin.shares().await?;
    if ctx.out.is_json() {
        return ctx.out.json(&json!({ "items": shares }));
    }
    if shares.is_empty() {
        ctx.out.note("Nothing is public.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = shares
        .iter()
        .map(|share| {
            vec![
                share.id.clone(),
                format!("{:?}", share.kind).to_lowercase(),
                output::truncate(&share.target, 32),
                output::truncate(&share.created_by_email, 28),
                share
                    .expires_at
                    .as_deref()
                    .map(output::date)
                    .unwrap_or_else(|| "never".into()),
                [
                    share.has_password.then_some("password"),
                    (!share.allow_download).then_some("no download"),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", "),
            ]
        })
        .collect();
    ctx.out
        .table(&["ID", "KIND", "WHAT", "WHO", "EXPIRES", ""], &rows);
    Ok(())
}
