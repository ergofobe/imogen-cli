//! Where the CLI remembers which library it is talking to.
//!
//! One file holds every profile, so a person with a home server and a family server
//! switches between them with `--profile` rather than logging in again each time. It is
//! written owner-only: it holds a token that reads someone's entire photo library.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use imogen_sdk::{StoredTokens, TokenResponse};
use serde::{Deserialize, Serialize};

/// The saved half of an authorization. Kept in the SDK's shape rather than flattened, so
/// expiry arithmetic stays where the SDK does it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub server: String,
    /// Absent for a profile authenticated with a token supplied by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Seconds the access token was minted for.
    #[serde(default)]
    pub expires_in: i64,
    /// Unix milliseconds, so expiry is computable without keeping the clock that read it.
    #[serde(default)]
    pub obtained_at: u128,
    #[serde(default)]
    pub scope: String,
}

impl Profile {
    pub fn from_tokens(server: String, client_id: String, stored: StoredTokens) -> Self {
        Self {
            server,
            client_id: Some(client_id),
            access_token: Some(stored.tokens.access_token),
            refresh_token: stored.tokens.refresh_token,
            expires_in: stored.tokens.expires_in,
            obtained_at: stored.obtained_at,
            scope: stored.tokens.scope,
        }
    }

    /// A profile holding only a token somebody pasted in: no refresh, no expiry.
    pub fn from_token(server: String, token: String) -> Self {
        Self {
            server,
            client_id: None,
            access_token: Some(token),
            refresh_token: None,
            expires_in: 0,
            obtained_at: 0,
            scope: String::new(),
        }
    }

    pub fn stored(&self) -> Option<StoredTokens> {
        Some(StoredTokens {
            tokens: TokenResponse {
                access_token: self.access_token.clone()?,
                token_type: "Bearer".into(),
                expires_in: self.expires_in,
                refresh_token: self.refresh_token.clone(),
                scope: self.scope.clone(),
            },
            obtained_at: self.obtained_at,
        })
    }

    /// True when the token is close enough to expiry to be worth refreshing first. A
    /// profile that never expires (a pasted token) is never stale.
    pub fn needs_refresh(&self) -> bool {
        if self.expires_in == 0 {
            return false;
        }
        self.stored().map(|s| s.is_expired(60)).unwrap_or(false)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Which profile commands use when `--profile` is not given.
    #[serde(default)]
    pub current: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

/// Follows the XDG convention, falling back to `~/.config` where it is not set. This is
/// the same directory `imogen-mcp` uses, under a different filename: the two tools hold
/// separate authorizations so revoking one does not sign the other out.
pub fn config_path() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("IMOGEN_CONFIG") {
        return Ok(PathBuf::from(explicit));
    }
    let base = match std::env::var("XDG_CONFIG_HOME") {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        _ => dirs::home_dir()
            .context("Could not find a home directory to store credentials in")?
            .join(".config"),
    };
    Ok(base.join("imogen").join("cli.json"))
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("Could not read {}", path.display()))?;
        // A corrupt file should not make every command fail forever; say so and start over.
        serde_json::from_str(&text).with_context(|| {
            format!(
                "{} is not valid JSON. Delete it and log in again.",
                path.display()
            )
        })
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Could not create {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text)
            .with_context(|| format!("Could not write {}", path.display()))?;
        restrict(&path)?;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    pub fn set(&mut self, name: &str, profile: Profile) {
        self.profiles.insert(name.to_string(), profile);
        if self.current.is_none() {
            self.current = Some(name.to_string());
        }
    }

    pub fn remove(&mut self, name: &str) -> Option<Profile> {
        let removed = self.profiles.remove(name);
        if self.current.as_deref() == Some(name) {
            self.current = self.profiles.keys().next().cloned();
        }
        removed
    }

    pub fn default_profile_name(&self) -> String {
        self.current
            .clone()
            .or_else(|| self.profiles.keys().next().cloned())
            .unwrap_or_else(|| "default".to_string())
    }
}

#[cfg(unix)]
fn restrict(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) -> Result<()> {
    Ok(())
}
