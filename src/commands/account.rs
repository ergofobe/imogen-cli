//! Your own account, and the state of the connection to the library.

use anyhow::Result;
use imogen_sdk::{PasswordChangeRequest, ProfileUpdate, UserRole};
use serde_json::json;

use crate::cli::AccountCommand;
use crate::context::Context;
use crate::output::{self, GREEN, RED};

pub async fn run(ctx: &Context, command: &AccountCommand) -> Result<()> {
    match command {
        AccountCommand::Show => whoami(ctx).await,
        AccountCommand::Update {
            name,
            email,
            current_password,
        } => {
            let updated = ctx
                .client
                .auth
                .update_profile(&ProfileUpdate {
                    name: name.clone(),
                    email: email.clone(),
                    current_password: current_password.clone(),
                })
                .await?;
            if ctx.out.is_json() {
                return ctx.out.json(&updated);
            }
            ctx.out.note(ctx.out.paint("Updated.", GREEN));
            Ok(())
        }
        AccountCommand::Password { current, new } => {
            ctx.client
                .auth
                .change_password(&PasswordChangeRequest {
                    current_password: current.clone(),
                    new_password: new.clone(),
                })
                .await?;
            if ctx.out.is_json() {
                return ctx.out.json(&json!({ "changed": true }));
            }
            ctx.out.note(ctx.out.paint("Password changed.", GREEN));
            Ok(())
        }
        AccountCommand::LogoutEverywhere { yes } => {
            if !ctx.confirm(
                "End every session this account has, on every device?",
                *yes || ctx.out.is_json(),
            )? {
                ctx.out.note("Left alone.");
                return Ok(());
            }
            ctx.client.auth.logout_everywhere().await?;
            if ctx.out.is_json() {
                return ctx.out.json(&json!({ "loggedOut": true }));
            }
            ctx.out.note(ctx.out.paint("Signed out everywhere.", GREEN));
            Ok(())
        }
    }
}

pub async fn whoami(ctx: &Context) -> Result<()> {
    let user = ctx.client.auth.me().await?;
    if ctx.out.is_json() {
        return ctx.out.json(&user);
    }
    ctx.out.heading(&user.name);
    ctx.out.fields(&[
        ("email", user.email.clone()),
        ("id", user.id.clone()),
        (
            "role",
            match user.role {
                UserRole::Admin => "administrator".into(),
                UserRole::User => "user".to_string(),
            },
        ),
        (
            "signs in with",
            if user.oidc_subject.is_some() {
                "single sign-on".to_string()
            } else {
                "a password".to_string()
            },
        ),
        (
            "using",
            match user.quota_bytes {
                Some(quota) => format!(
                    "{} of {}  ({:.0}%)",
                    output::bytes(user.used_bytes),
                    output::bytes(quota),
                    (user.used_bytes as f64 / quota.max(1) as f64) * 100.0
                ),
                None => output::bytes(user.used_bytes),
            },
        ),
        ("server", ctx.server.clone()),
        ("profile", ctx.profile_name.clone()),
    ]);
    Ok(())
}

/// Whether the server is reachable and whether this profile can actually reach it. Useful
/// on its own, and the first thing to run when something else has just failed.
pub async fn status(ctx: &Context) -> Result<()> {
    let health = ctx.client.health().await;
    let user = match &health {
        Ok(_) => ctx.client.auth.me().await.ok(),
        Err(_) => None,
    };

    if ctx.out.is_json() {
        return ctx.out.json(&json!({
            "server": ctx.server,
            "profile": ctx.profile_name,
            "reachable": health.is_ok(),
            "version": health.as_ref().ok().map(|h| h.version.clone()),
            "error": health.as_ref().err().map(|e| e.to_string()),
            "authenticated": user.is_some(),
            "user": user,
        }));
    }

    ctx.out.fields(&[
        ("server", ctx.server.clone()),
        ("profile", ctx.profile_name.clone()),
        (
            "reachable",
            match &health {
                Ok(health) => ctx
                    .out
                    .paint(&format!("yes, imogen {}", health.version), GREEN),
                Err(error) => ctx.out.paint(&format!("no — {error}"), RED),
            },
        ),
        (
            "signed in",
            match &user {
                Some(user) => format!("{} <{}>", user.name, user.email),
                None if health.is_ok() => ctx.out.paint("no", RED),
                None => String::new(),
            },
        ),
        ("scopes", ctx.tokens.scope().await),
    ]);
    Ok(())
}
