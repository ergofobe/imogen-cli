//! Signing in, and keeping the token usable once signed in.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener as StdListener;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use imogen_sdk::{BoxFuture, OAuthClient, RefreshToken, StoredTokens, TokenSource};
use tokio::sync::Mutex;

use crate::config::{Config, Profile};

/// The token the SDK asks for on every request, refreshed in place when it goes stale and
/// written back so the next invocation of the CLI starts with a live one.
pub struct ProfileTokens {
    name: String,
    profile: Mutex<Profile>,
    /// A token given on the command line is used as-is and never persisted.
    ephemeral: bool,
}

impl ProfileTokens {
    pub fn new(name: impl Into<String>, profile: Profile, ephemeral: bool) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            profile: Mutex::new(profile),
            ephemeral,
        })
    }

    /// What this authorization is allowed to do, for `imogen status`.
    pub async fn scope(&self) -> String {
        self.profile.lock().await.scope.clone()
    }

    async fn refreshed(&self) -> Option<String> {
        let (server, client_id, refresh_token) = {
            let profile = self.profile.lock().await;
            (
                profile.server.clone(),
                profile.client_id.clone()?,
                profile.refresh_token.clone()?,
            )
        };

        let oauth = OAuthClient::new(server.clone());
        let stored = oauth.refresh(&client_id, &refresh_token).await.ok()?;
        let updated = Profile::from_tokens(server, client_id, stored);
        let access = updated.access_token.clone();

        if !self.ephemeral {
            // Best effort: a token that cannot be saved is still a token that works now.
            if let Ok(mut config) = Config::load() {
                config.profiles.insert(self.name.clone(), updated.clone());
                let _ = config.save();
            }
        }
        *self.profile.lock().await = updated;
        access
    }
}

impl TokenSource for ProfileTokens {
    fn token(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async move {
            if self.profile.lock().await.needs_refresh() {
                if let Some(token) = self.refreshed().await {
                    return Some(token);
                }
            }
            self.profile.lock().await.access_token.clone()
        })
    }
}

impl RefreshToken for ProfileTokens {
    fn refresh(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async move { self.refreshed().await })
    }
}

/// Authorization code with PKCE against a loopback redirect — the flow RFC 8252
/// prescribes for a command-line tool. The browser handles the login, and the code comes
/// back to a server that exists only for the few seconds the flow takes.
pub async fn browser_login(
    server: &str,
    app_name: &str,
    scopes: Option<&[String]>,
    open_browser: bool,
    on_url: impl Fn(&str),
) -> Result<(String, StoredTokens)> {
    let listener = StdListener::bind("127.0.0.1:0")
        .context("Could not open a local port to receive the authorization")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let oauth = OAuthClient::new(server.to_string());
    let client = oauth
        .register(app_name, std::slice::from_ref(&redirect_uri), scopes)
        .await
        .with_context(|| format!("Could not register this machine with {server}"))?;
    let pending = oauth
        .begin_authorization(&client.client_id, &redirect_uri, scopes)
        .await?;

    on_url(&pending.authorization_url);
    if open_browser {
        let _ = open_in_browser(&pending.authorization_url);
    }

    // The wait is blocking and single-shot, so it belongs off the runtime's worker.
    let callback = tokio::task::spawn_blocking(move || wait_for_callback(listener)).await??;
    let tokens = oauth.complete_authorization(&pending, &callback).await?;
    Ok((client.client_id, tokens))
}

/// Reads one request, answers it, and hands back the URL it arrived on. Anything that is
/// not the callback path gets a 404, so a stray probe does not end the wait.
fn wait_for_callback(listener: StdListener) -> Result<String> {
    let port = listener.local_addr()?.port();
    loop {
        let (mut stream, _) = listener.accept()?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;

        let target = request_line.split_whitespace().nth(1).unwrap_or("/");
        if !target.starts_with("/callback") {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            continue;
        }

        let body = DONE_PAGE.as_bytes();
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        );
        let _ = stream.write_all(body);
        let _ = stream.flush();
        return Ok(format!("http://127.0.0.1:{port}{target}"));
    }
}

fn open_in_browser(url: &str) -> Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("cmd");
        c.args(["/c", "start", "", url]);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| anyhow!("Could not open a browser: {error}"))
}

/// Resolves which profile a command should run as, given the flags and what is saved.
pub fn resolve(
    config: &Config,
    profile_name: &str,
    server: Option<&str>,
    token: Option<&str>,
) -> Result<(Profile, bool)> {
    if let Some(token) = token {
        let server = server
            .map(str::to_string)
            .or_else(|| config.get(profile_name).map(|p| p.server.clone()))
            .ok_or_else(|| {
                anyhow!("A token needs a server too: pass --server, or set IMOGEN_SERVER")
            })?;
        return Ok((Profile::from_token(trim(&server), token.to_string()), true));
    }

    if let Some(saved) = config.get(profile_name) {
        let mut profile = saved.clone();
        if let Some(server) = server {
            profile.server = trim(server);
        }
        return Ok((profile, false));
    }

    // No saved profile: a bare --server still allows the unauthenticated endpoints.
    match server {
        Some(server) => Ok((
            Profile {
                server: trim(server),
                client_id: None,
                access_token: None,
                refresh_token: None,
                expires_in: 0,
                obtained_at: 0,
                scope: String::new(),
            },
            true,
        )),
        None => bail!("Not signed in. Run: imogen login --server https://photos.example.com"),
    }
}

pub fn trim(server: &str) -> String {
    server.trim_end_matches('/').to_string()
}

const DONE_PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>Connected · imogen</title>
<style>
  body { margin:0; min-height:100vh; display:grid; place-items:center;
         font:16px/1.5 ui-sans-serif,-apple-system,sans-serif;
         background:#101113; color:#f2f3f4; }
  p { color:#9096a0; margin:.5rem 0 0; font-size:.925rem; }
  strong { font-weight:600; letter-spacing:-0.02em; font-size:1.25rem; }
</style>
<div style="text-align:center">
  <strong>Connected</strong>
  <p>You can close this tab and go back to your terminal.</p>
</div>"#;
