//! A small Supabase client: email/password auth and the `entries` table.
//! See `docs/cloud-sync.md` for the protocol shared with the macOS app.

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::config;

const KEYRING_SERVICE: &str = "app.humansongs.freewrite";
const KEYRING_USER: &str = "supabase-session";

#[derive(Clone, Debug, Deserialize)]
pub struct CloudConfig {
    pub url: String,
    pub anon_key: String,
}

impl CloudConfig {
    /// Runtime environment, then `cloud.json`, then values baked in at build time.
    pub fn load() -> Option<Self> {
        let from_env = || {
            Some(CloudConfig {
                url: std::env::var("FREEWRITE_SUPABASE_URL").ok()?,
                anon_key: std::env::var("FREEWRITE_SUPABASE_ANON_KEY").ok()?,
            })
        };
        let from_file = || {
            let text = fs::read_to_string(config::dir().join("cloud.json")).ok()?;
            serde_json::from_str::<CloudConfig>(&text).ok()
        };
        let built_in = || {
            Some(CloudConfig {
                url: option_env!("FREEWRITE_SUPABASE_URL")?.to_string(),
                anon_key: option_env!("FREEWRITE_SUPABASE_ANON_KEY")?.to_string(),
            })
        };
        from_env()
            .or_else(from_file)
            .or_else(built_in)
            .filter(|c| !c.url.trim().is_empty() && !c.anon_key.trim().is_empty())
            .map(|c| CloudConfig {
                url: c.url.trim().trim_end_matches('/').to_string(),
                anon_key: c.anon_key.trim().to_string(),
            })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
    pub user_id: Uuid,
    pub email: String,
}

impl Session {
    pub fn expires_soon(&self) -> bool {
        self.expires_at - Utc::now().timestamp() < 60
    }
}

/// What we persist: just enough to refresh on the next launch. The access
/// token stays in memory, which also keeps us under Windows' credential size limit.
#[derive(Serialize, Deserialize)]
struct StoredSession {
    refresh_token: String,
    user_id: Uuid,
    email: String,
}

fn session_file() -> PathBuf {
    config::dir().join("session.json")
}

pub fn save_session(session: &Session) {
    let stored = StoredSession {
        refresh_token: session.refresh_token.clone(),
        user_id: session.user_id,
        email: session.email.clone(),
    };
    let Ok(json) = serde_json::to_string(&stored) else {
        return;
    };
    let in_keyring = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .and_then(|entry| entry.set_password(&json))
        .is_ok();
    if in_keyring {
        let _ = fs::remove_file(session_file());
    } else {
        // No credential store (e.g. no Secret Service running): fall back to a
        // file only the current user can read.
        let _ = config::write_private(&session_file(), &json);
    }
}

/// Returns a session with an expired access token; refresh it before use.
pub fn load_session() -> Option<Session> {
    let json = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .and_then(|entry| entry.get_password())
        .ok()
        .or_else(|| fs::read_to_string(session_file()).ok())?;
    let stored: StoredSession = serde_json::from_str(&json).ok()?;
    Some(Session {
        access_token: String::new(),
        refresh_token: stored.refresh_token,
        expires_at: 0,
        user_id: stored.user_id,
        email: stored.email,
    })
}

pub fn clear_session() {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
        let _ = entry.delete_credential();
    }
    let _ = fs::remove_file(session_file());
}

#[derive(Debug)]
pub enum CloudError {
    /// The session is no longer valid; the user has to sign in again.
    Unauthorized(String),
    Network(String),
    Server(String),
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CloudError::Unauthorized(msg) | CloudError::Server(msg) => f.write_str(msg),
            CloudError::Network(msg) => write!(f, "Couldn't reach the server ({msg})"),
        }
    }
}

impl From<reqwest::Error> for CloudError {
    fn from(err: reqwest::Error) -> Self {
        CloudError::Network(err.without_url().to_string())
    }
}

pub type CloudResult<T> = Result<T, CloudError>;

pub enum SignUpOutcome {
    SignedIn(Session),
    /// The project requires email confirmation before the first sign-in.
    ConfirmEmail,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RemoteEntry {
    pub id: Uuid,
    pub filename: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub deleted: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EntryUpsert {
    pub user_id: Uuid,
    pub id: Uuid,
    pub filename: String,
    pub content: String,
    pub deleted: bool,
    pub client_updated_at: DateTime<Utc>,
}

pub struct CloudClient {
    http: Client,
    config: CloudConfig,
}

impl CloudClient {
    pub fn new(config: CloudConfig) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("freewrite-desktop/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("HTTP client");
        CloudClient { http, config }
    }

    fn auth(&self, path: &str) -> RequestBuilder {
        self.http
            .post(format!("{}/auth/v1/{path}", self.config.url))
            .header("apikey", &self.config.anon_key)
    }

    fn rest(&self, method: reqwest::Method, session: &Session) -> RequestBuilder {
        self.http
            .request(method, format!("{}/rest/v1/entries", self.config.url))
            .header("apikey", &self.config.anon_key)
            .bearer_auth(&session.access_token)
    }

    pub fn sign_in(&self, email: &str, password: &str) -> CloudResult<Session> {
        let response = self
            .auth("token?grant_type=password")
            .json(&json!({ "email": email, "password": password }))
            .send()?;
        parse_session(check(response)?)
    }

    pub fn sign_up(&self, email: &str, password: &str) -> CloudResult<SignUpOutcome> {
        let response = self
            .auth("signup")
            .json(&json!({ "email": email, "password": password }))
            .send()?;
        let body = check(response)?;
        if body.get("access_token").is_some() {
            parse_session(body).map(SignUpOutcome::SignedIn)
        } else {
            Ok(SignUpOutcome::ConfirmEmail)
        }
    }

    pub fn refresh(&self, session: &Session) -> CloudResult<Session> {
        let response = self
            .auth("token?grant_type=refresh_token")
            .json(&json!({ "refresh_token": session.refresh_token }))
            .send()?;
        match check(response) {
            // A rejected refresh token means the session is gone for good.
            Err(CloudError::Server(msg)) => Err(CloudError::Unauthorized(msg)),
            other => parse_session(other?),
        }
    }

    pub fn sign_out(&self, session: &Session) {
        let _ = self
            .auth("logout")
            .bearer_auth(&session.access_token)
            .send();
    }

    /// Rows after `after` in `(updated_at, id)` order, oldest first.
    pub fn fetch_changes(
        &self,
        session: &Session,
        after: Option<PageKey>,
        limit: usize,
    ) -> CloudResult<Vec<RemoteEntry>> {
        let mut query = vec![
            (
                "select",
                "id,filename,content,deleted,updated_at".to_string(),
            ),
            ("order", "updated_at.asc,id.asc".to_string()),
            ("limit", limit.to_string()),
        ];
        match after {
            None => {}
            Some(PageKey {
                updated_at,
                id: None,
            }) => {
                query.push(("updated_at", format!("gt.{}", timestamp(updated_at))));
            }
            // Keyset paging: rows edited mid-pull move to the end instead of
            // shifting later pages, so none are skipped.
            Some(PageKey {
                updated_at,
                id: Some(id),
            }) => {
                let t = timestamp(updated_at);
                query.push((
                    "or",
                    // Quoted: `.` and `:` are reserved inside PostgREST logic trees.
                    format!("(updated_at.gt.\"{t}\",and(updated_at.eq.\"{t}\",id.gt.{id}))"),
                ));
            }
        }
        let response = self
            .rest(reqwest::Method::GET, session)
            .query(&query)
            .send()?;
        let body = check(response)?;
        serde_json::from_value(body).map_err(|e| CloudError::Server(e.to_string()))
    }

    pub fn upsert(&self, session: &Session, rows: &[EntryUpsert]) -> CloudResult<()> {
        let response = self
            .rest(reqwest::Method::POST, session)
            .query(&[("on_conflict", "user_id,id")])
            .header("Prefer", "resolution=merge-duplicates,return=minimal")
            .json(rows)
            .send()?;
        check(response).map(|_| ())
    }
}

/// A position in the `(updated_at, id)` order of the entries table.
#[derive(Clone, Copy, Debug)]
pub struct PageKey {
    pub updated_at: DateTime<Utc>,
    pub id: Option<Uuid>,
}

/// Postgres keeps microseconds; so must we, or equality filters miss.
fn timestamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn check(response: Response) -> CloudResult<Value> {
    let status = response.status();
    let text = response.text()?;
    let body: Value = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    if status.is_success() {
        return Ok(body);
    }
    let message = ["error_description", "msg", "message", "error"]
        .iter()
        .find_map(|key| body.get(key).and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| format!("Request failed ({status})"));
    if status == StatusCode::UNAUTHORIZED {
        Err(CloudError::Unauthorized(message))
    } else {
        Err(CloudError::Server(message))
    }
}

fn parse_session(body: Value) -> CloudResult<Session> {
    let invalid = || CloudError::Server("Unexpected response from the server".to_string());
    let str_field = |key: &str| body.get(key).and_then(Value::as_str).map(str::to_string);
    let user = body.get("user").ok_or_else(invalid)?;
    let expires_at = body
        .get("expires_at")
        .and_then(Value::as_i64)
        .or_else(|| {
            let expires_in = body.get("expires_in").and_then(Value::as_i64)?;
            Some(Utc::now().timestamp() + expires_in)
        })
        .unwrap_or(0);
    Ok(Session {
        access_token: str_field("access_token").ok_or_else(invalid)?,
        refresh_token: str_field("refresh_token").ok_or_else(invalid)?,
        expires_at,
        user_id: user
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or_else(invalid)?,
        email: user
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_password_grant_response() {
        let body = json!({
            "access_token": "jwt",
            "token_type": "bearer",
            "expires_in": 3600,
            "expires_at": 1_900_000_000,
            "refresh_token": "refresh",
            "user": { "id": "0c9f4b4e-1f0a-4c7e-9d7b-7a1b2c3d4e5f", "email": "a@b.c" }
        });
        let session = parse_session(body).unwrap();
        assert_eq!(session.refresh_token, "refresh");
        assert_eq!(session.expires_at, 1_900_000_000);
        assert_eq!(session.email, "a@b.c");
        assert!(!session.expires_soon());
    }

    #[test]
    fn parses_postgrest_timestamps() {
        let rows: Vec<RemoteEntry> = serde_json::from_value(json!([{
            "id": "0c9f4b4e-1f0a-4c7e-9d7b-7a1b2c3d4e5f",
            "filename": "x.md",
            "content": "\n\nhi",
            "deleted": false,
            "updated_at": "2026-10-29T12:34:56.123456+00:00"
        }]))
        .unwrap();
        assert_eq!(rows[0].updated_at.timestamp_subsec_micros(), 123_456);
    }
}
