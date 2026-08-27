//! Signing in and out, and the saved logins.

use anyhow::{bail, Result};
use imogen_sdk::OAuthClient;
use serde_json::json;

use crate::auth;
use crate::cli::{GlobalArgs, LoginArgs, ProfilesArgs};
use crate::config::{config_path, Config, Profile};
use crate::context::Context;
use crate::output::{Output, GREEN, YELLOW};

pub async fn login(global: &GlobalArgs, args: &LoginArgs) -> Result<()> {
    let out = Output::new(global.json, global.no_color, global.quiet);
    let mut config = Config::load()?;

    let server = args
        .server
        .clone()
        .or_else(|| global.server.clone())
        .or_else(|| config.get(&args.name).map(|p| p.server.clone()))
        .map(|s| auth::trim(&s));
    let Some(server) = server else {
        bail!("Which library? Example: imogen login --server https://photos.example.com");
    };

    // A token given by hand skips the browser entirely, which is what a headless machine
    // or an agent with a token from elsewhere needs.
    if let Some(token) = args.with_token.clone().or_else(|| global.token.clone()) {
        let profile = Profile::from_token(server.clone(), token);
        return finish(global, &out, &mut config, &args.name, profile).await;
    }

    let scopes = (!args.scope.is_empty()).then(|| args.scope.clone());
    out.note(format!("Authorizing against {server}…"));

    let (client_id, tokens) = auth::browser_login(
        &server,
        &args.app_name,
        scopes.as_deref(),
        !args.no_browser,
        |url| {
            if args.no_browser {
                out.note("Open this in a browser to authorize:\n");
                println!("{url}");
                out.note("");
            } else {
                out.note(format!(
                    "Opening your browser. If it does not open, visit:\n\n  {url}\n"
                ));
            }
        },
    )
    .await?;

    let profile = Profile::from_tokens(server, client_id, tokens);
    finish(global, &out, &mut config, &args.name, profile).await
}

async fn finish(
    global: &GlobalArgs,
    out: &Output,
    config: &mut Config,
    name: &str,
    profile: Profile,
) -> Result<()> {
    // Prove the credential works before writing it down: a profile that was saved and
    // then fails on the first real command is a worse outcome than failing here.
    let ctx = Context::from_profile(global, name, profile.clone(), true);
    let user = ctx.client.auth.me().await?;

    config.set(name, profile.clone());
    config.current = Some(name.to_string());
    config.save()?;

    if out.is_json() {
        return out.json(&json!({
            "profile": name,
            "server": profile.server,
            "user": user,
            "credentials": config_path()?,
        }));
    }
    out.note(out.paint(
        &format!(
            "Signed in to {} as {} <{}>.",
            profile.server, user.name, user.email
        ),
        GREEN,
    ));
    out.note(format!(
        "Saved as profile “{name}” in {}",
        config_path()?.display()
    ));
    Ok(())
}

pub async fn logout(global: &GlobalArgs, revoke: bool) -> Result<()> {
    let out = Output::new(global.json, global.no_color, global.quiet);
    let mut config = Config::load()?;
    let name = global
        .profile
        .clone()
        .unwrap_or_else(|| config.default_profile_name());

    let Some(profile) = config.get(&name).cloned() else {
        if out.is_json() {
            return out.json(&json!({ "removed": false, "profile": name }));
        }
        out.note("Nothing to sign out of.");
        return Ok(());
    };

    if revoke {
        if let Some(token) = &profile.access_token {
            let oauth = OAuthClient::new(profile.server.clone());
            // The local credential goes either way; a server that will not revoke should
            // not leave a token sitting on disk.
            if oauth.revoke(token).await.is_err() {
                out.warn("The server would not revoke the token; forgetting it locally anyway.");
            }
        }
    }

    config.remove(&name);
    config.save()?;

    if out.is_json() {
        return out.json(&json!({ "removed": true, "profile": name }));
    }
    out.note(out.paint(&format!("Signed out of “{name}”."), GREEN));
    Ok(())
}

pub fn profiles(global: &GlobalArgs, args: &ProfilesArgs) -> Result<()> {
    let out = Output::new(global.json, global.no_color, global.quiet);
    let mut config = Config::load()?;

    if let Some(name) = &args.set_default {
        if !config.profiles.contains_key(name) {
            bail!("No profile called “{name}”");
        }
        config.current = Some(name.clone());
        config.save()?;
        if out.is_json() {
            return out.json(&json!({ "current": name }));
        }
        out.note(out.paint(&format!("“{name}” is now the default."), GREEN));
        return Ok(());
    }

    if out.is_json() {
        return out.json(&config);
    }
    if config.profiles.is_empty() {
        out.note("No saved logins. Run: imogen login --server https://photos.example.com");
        return Ok(());
    }
    let current = config.default_profile_name();
    let rows: Vec<Vec<String>> = config
        .profiles
        .iter()
        .map(|(name, profile)| {
            vec![
                if *name == current {
                    out.paint(name, YELLOW)
                } else {
                    name.clone()
                },
                profile.server.clone(),
                if profile.client_id.is_some() {
                    "browser".into()
                } else {
                    "token".into()
                },
                profile.scope.clone(),
            ]
        })
        .collect();
    out.table(&["PROFILE", "SERVER", "SIGNED IN VIA", "SCOPES"], &rows);
    Ok(())
}
