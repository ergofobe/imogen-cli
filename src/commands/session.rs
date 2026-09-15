//! Signing in and out, and the saved logins.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use imogen_sdk::OAuthClient;
use serde::Serialize;
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
            "credentials": config_path()?.display().to_string(),
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
        return out.json(&profiles_view(&config));
    }
    if config.profiles.is_empty() {
        out.note("No saved logins. Run: imogen login --server https://photos.example.com");
        return Ok(());
    }
    let current = current_profile(&config);
    let rows: Vec<Vec<String>> = config
        .profiles
        .iter()
        .map(|(name, profile)| {
            vec![
                if Some(name.as_str()) == current {
                    out.paint(name, YELLOW)
                } else {
                    name.clone()
                },
                profile.server.clone(),
                signed_in_via(profile).to_string(),
                profile.scope.clone(),
            ]
        })
        .collect();
    out.table(&["PROFILE", "SERVER", "SIGNED IN VIA", "SCOPES"], &rows);
    Ok(())
}

/// What `--json` says about the saved logins: the four things the table already shows,
/// and nothing else.
///
/// A view struct rather than `#[serde(skip_serializing)]` on `Profile`'s token fields,
/// because this one names everything it prints. A skip attribute has to be remembered by
/// whoever adds the next credential to the config; a struct that has to be extended on
/// purpose cannot leak one by omission.
#[derive(Serialize)]
struct ProfilesView<'a> {
    current: Option<&'a str>,
    profiles: BTreeMap<&'a str, ProfileView<'a>>,
}

#[derive(Serialize)]
struct ProfileView<'a> {
    server: &'a str,
    signed_in_via: &'static str,
    scope: &'a str,
    /// Unix milliseconds. Absent for a pasted token, which carries no expiry at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<u128>,
}

fn profiles_view(config: &Config) -> ProfilesView<'_> {
    ProfilesView {
        current: current_profile(config),
        profiles: config
            .profiles
            .iter()
            .map(|(name, profile)| {
                (
                    name.as_str(),
                    ProfileView {
                        server: &profile.server,
                        signed_in_via: signed_in_via(profile),
                        scope: &profile.scope,
                        expires_at: expires_at(profile),
                    },
                )
            })
            .collect(),
    }
}

/// The profile a command would reach for today — the one the table paints yellow.
/// `Config::default_profile_name` invents “default” for an empty config, which is the
/// right answer for naming a profile about to be written and the wrong one for describing
/// what is saved.
fn current_profile(config: &Config) -> Option<&str> {
    let named = config.current.as_deref();
    config
        .profiles
        .keys()
        .map(String::as_str)
        .find(|name| Some(*name) == named)
        .or_else(|| config.profiles.keys().next().map(String::as_str))
}

fn signed_in_via(profile: &Profile) -> &'static str {
    if profile.client_id.is_some() {
        "browser"
    } else {
        "token"
    }
}

fn expires_at(profile: &Profile) -> Option<u128> {
    if profile.expires_in <= 0 || profile.obtained_at == 0 {
        return None;
    }
    Some(profile.obtained_at + profile.expires_in as u128 * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser_profile() -> Profile {
        Profile {
            server: "https://photos.example.com".into(),
            client_id: Some("client-1".into()),
            access_token: Some("at-SECRET-ACCESS".into()),
            refresh_token: Some("rt-SECRET-REFRESH".into()),
            expires_in: 3600,
            obtained_at: 1_700_000_000_000,
            scope: "library:read".into(),
        }
    }

    fn saved() -> Config {
        let mut config = Config::default();
        config.set("home", browser_profile());
        config.set(
            "family",
            Profile::from_token(
                "https://family.example.com".into(),
                "at-SECRET-PASTED".into(),
            ),
        );
        config
    }

    /// A grep of the serialised bytes rather than a field-by-field check: a credential
    /// added to `Profile` later would reintroduce the leak silently, and only looking at
    /// the whole string catches it.
    #[test]
    fn the_json_answer_carries_no_credentials() {
        let config = saved();
        let json = serde_json::to_string(&profiles_view(&config)).unwrap();

        for secret in ["at-SECRET-ACCESS", "rt-SECRET-REFRESH", "at-SECRET-PASTED"] {
            assert!(!json.contains(secret), "{secret} reached stdout: {json}");
        }
        assert!(
            !json.contains("token\":"),
            "no credential field at all: {json}"
        );
    }

    #[test]
    fn the_json_answer_describes_what_the_table_shows() {
        let config = saved();
        let json: serde_json::Value = serde_json::to_value(profiles_view(&config)).unwrap();

        assert_eq!(
            json["current"], "home",
            "the first profile saved is the default"
        );
        assert_eq!(
            json["profiles"]["home"]["server"],
            "https://photos.example.com"
        );
        assert_eq!(json["profiles"]["home"]["signed_in_via"], "browser");
        assert_eq!(json["profiles"]["home"]["scope"], "library:read");
        assert_eq!(
            json["profiles"]["home"]["expires_at"],
            1_700_000_000_000u64 + 3_600_000
        );

        assert_eq!(json["profiles"]["family"]["signed_in_via"], "token");
        assert!(
            json["profiles"]["family"]["expires_at"].is_null(),
            "a pasted token has no expiry to report"
        );
    }

    #[test]
    fn nothing_is_current_until_something_is_saved() {
        let json = serde_json::to_value(profiles_view(&Config::default())).unwrap();
        assert!(json["current"].is_null());
    }

    /// `current` naming a profile that was removed would send automation to a login that
    /// is not there; the answer falls back the same way the commands themselves do.
    #[test]
    fn a_stale_current_falls_back_to_a_profile_that_exists() {
        let mut config = saved();
        config.current = Some("gone".into());
        let json = serde_json::to_value(profiles_view(&config)).unwrap();
        assert_eq!(json["current"], "family", "the first profile by name");
    }
}
