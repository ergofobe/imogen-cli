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
    /// Held for the whole of a refresh, so only one is ever in flight. A refresh token is
    /// single-use: the server rotates it and, on seeing a rotated one come back, revokes
    /// the entire family as a stolen-token replay. Six uploads running at once all
    /// noticing the same expiry and each spending the same refresh token is exactly that
    /// pattern, and it signs the machine out rather than refreshing it.
    refreshing: Mutex<()>,
    /// A token given on the command line is used as-is and never persisted.
    ephemeral: bool,
}

impl ProfileTokens {
    pub fn new(name: impl Into<String>, profile: Profile, ephemeral: bool) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            profile: Mutex::new(profile),
            refreshing: Mutex::new(()),
            ephemeral,
        })
    }

    /// What this authorization is allowed to do, for `imogen status`.
    pub async fn scope(&self) -> String {
        self.profile.lock().await.scope.clone()
    }

    /// Spends `seen` for a new access token, unless somebody else got there first.
    ///
    /// Callers sample the refresh token before queueing here. Whoever wins the lock
    /// spends it; the rest wake to find a different token saved, which says their work
    /// has already been done for them and that spending `seen` again would be the replay
    /// the server revokes for.
    async fn refresh_from(&self, seen: &str) -> Option<String> {
        let _guard = self.refreshing.lock().await;

        let (server, client_id, refresh_token) = {
            let profile = self.profile.lock().await;
            if profile.refresh_token.as_deref() != Some(seen) {
                return profile.access_token.clone();
            }
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

    /// The refresh token as it stands, to be handed back to `refresh_from`.
    async fn current_refresh_token(&self) -> Option<String> {
        self.profile.lock().await.refresh_token.clone()
    }
}

impl TokenSource for ProfileTokens {
    fn token(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async move {
            let (stale, seen) = {
                let profile = self.profile.lock().await;
                (profile.needs_refresh(), profile.refresh_token.clone())
            };
            if stale {
                if let Some(seen) = seen {
                    if let Some(token) = self.refresh_from(&seen).await {
                        return Some(token);
                    }
                }
            }
            self.profile.lock().await.access_token.clone()
        })
    }
}

impl RefreshToken for ProfileTokens {
    fn refresh(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async move {
            let seen = self.current_refresh_token().await?;
            self.refresh_from(&seen).await
        })
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
    // Discovery and registration are separate steps against separate URLs, and they fail
    // for entirely different reasons — one means the server is unreachable, the other
    // means it declined. Reporting both as "could not register" sends somebody looking in
    // the wrong place. The result is cached, so naming the step costs no extra request.
    oauth.discover().await.with_context(|| {
        format!("Could not reach {server}. Check the URL, and that the server is up.")
    })?;
    let client = oauth
        .register(app_name, std::slice::from_ref(&redirect_uri), scopes)
        .await
        .with_context(|| format!("{server} would not register this machine"))?;
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::io::Read;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    use super::*;

    /// An authorization server that rotates refresh tokens and, like the real one,
    /// revokes the whole family when a rotated token comes back. Counts what it is asked.
    struct FakeAuthServer {
        port: u16,
        grants: Arc<AtomicUsize>,
        revoked: Arc<AtomicUsize>,
    }

    impl FakeAuthServer {
        fn start() -> Self {
            let listener = StdListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let grants = Arc::new(AtomicUsize::new(0));
            let revoked = Arc::new(AtomicUsize::new(0));

            let (grants_bg, revoked_bg) = (grants.clone(), revoked.clone());
            std::thread::spawn(move || {
                // Every refresh token this server has already spent, and whether the
                // family has been revoked for a replay.
                let spent: StdMutex<HashSet<String>> = StdMutex::new(HashSet::new());
                let mut issued = 0usize;

                for stream in listener.incoming() {
                    let mut stream = match stream {
                        Ok(s) => s,
                        Err(_) => break,
                    };
                    let mut buffer = [0u8; 4096];
                    let read = stream.read(&mut buffer).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..read]).to_string();

                    let body = if request.starts_with("GET /.well-known") {
                        let base = format!("http://127.0.0.1:{port}");
                        serde_json::json!({
                            "issuer": base,
                            "authorization_endpoint": format!("{base}/oauth/authorize"),
                            "token_endpoint": format!("{base}/oauth/token"),
                        })
                        .to_string()
                    } else {
                        let presented = request
                            .rsplit("refresh_token=")
                            .next()
                            .unwrap_or_default()
                            .split('&')
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .to_string();

                        let mut spent = spent.lock().unwrap();
                        if revoked_bg.load(Ordering::SeqCst) > 0 || !spent.insert(presented) {
                            // A replay: the family goes, exactly as the server does it.
                            revoked_bg.fetch_add(1, Ordering::SeqCst);
                            let error = serde_json::json!({
                                "error": "invalid_grant",
                                "error_description": "refresh token has already been rotated",
                            })
                            .to_string();
                            let _ = stream.write_all(
                                format!(
                                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    error.len(),
                                    error
                                )
                                .as_bytes(),
                            );
                            continue;
                        }

                        issued += 1;
                        grants_bg.fetch_add(1, Ordering::SeqCst);
                        serde_json::json!({
                            "access_token": format!("at{issued}"),
                            "token_type": "Bearer",
                            "expires_in": 3600,
                            "refresh_token": format!("rt{issued}"),
                            "scope": "library:read",
                        })
                        .to_string()
                    };

                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    );
                }
            });

            Self {
                port,
                grants,
                revoked,
            }
        }

        fn url(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }
    }

    /// A profile whose access token expired a moment ago, so every caller agrees it is
    /// stale and they all reach for the refresh token together.
    fn expired_profile(server: String) -> Profile {
        Profile {
            server,
            client_id: Some("client".into()),
            access_token: Some("at0".into()),
            refresh_token: Some("rt0".into()),
            expires_in: 3600,
            obtained_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
                - 3_600_000,
            scope: "library:read".into(),
        }
    }

    /// Six concurrent uploads hitting the same expiry must spend the refresh token once.
    /// Spending it six times is what the server reads as a stolen token, and it answers
    /// by revoking the family — signing the machine out in the middle of the run.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_refresh_spends_the_token_once() {
        let server = FakeAuthServer::start();
        let tokens = ProfileTokens::new("test", expired_profile(server.url()), true);

        let results = futures::future::join_all((0..6).map(|_| {
            let tokens = tokens.clone();
            async move { tokens.token().await }
        }))
        .await;

        assert_eq!(
            server.grants.load(Ordering::SeqCst),
            1,
            "the refresh token must be spent once, not once per request in flight"
        );
        assert_eq!(
            server.revoked.load(Ordering::SeqCst),
            0,
            "a replayed refresh token revokes the family and signs the machine out"
        );
        for result in &results {
            assert_eq!(
                result.as_deref(),
                Some("at1"),
                "every caller gets the new token"
            );
        }
    }

    /// The 401 path is the same token, reached a different way: several requests failing
    /// at once must still produce one refresh.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_unauthorized_retries_spend_the_token_once() {
        let server = FakeAuthServer::start();
        let tokens = ProfileTokens::new("test", expired_profile(server.url()), true);

        futures::future::join_all((0..6).map(|_| {
            let tokens = tokens.clone();
            async move { RefreshToken::refresh(&*tokens).await }
        }))
        .await;

        assert_eq!(server.grants.load(Ordering::SeqCst), 1);
        assert_eq!(server.revoked.load(Ordering::SeqCst), 0);
    }

    /// A later expiry is a real refresh, not a coalesced one: the guard must not wedge
    /// the profile on the first token it ever fetched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_later_expiry_refreshes_again() {
        let server = FakeAuthServer::start();
        let tokens = ProfileTokens::new("test", expired_profile(server.url()), true);

        assert_eq!(tokens.token().await.as_deref(), Some("at1"));

        // Age the freshly-minted token the way another hour of uploading would.
        {
            let mut profile = tokens.profile.lock().await;
            profile.obtained_at -= 3_600_000;
        }

        assert_eq!(tokens.token().await.as_deref(), Some("at2"));
        assert_eq!(server.grants.load(Ordering::SeqCst), 2);
        assert_eq!(server.revoked.load(Ordering::SeqCst), 0);
    }
}
