//! Async HTTP client for a TinyLord instance.
//!
//! Wraps the subset of TinyLord's REST API the provider needs: browser login +
//! refresh, document CRUD, query (with cursor pagination), best-effort index
//! creation, and opening the SSE subscribe stream. Every non-streaming call
//! carries the access token as a `Bearer` header and, on a `401`, refreshes the
//! session **once** before retrying; a second `401` surfaces as
//! [`TinyError::SessionExpired`] so the caller can prompt the user to sign in
//! again.
//!
//! ## Session model (from TinyLord's `browser_auth.rs`)
//!
//! `POST /v1/auth/login {username,password}` returns JSON
//! `{access_token, token_type:"Bearer", expires_in, csrf_token}` and sets two
//! cookies: an HttpOnly `tinylord_refresh` (the rotating refresh session, scoped
//! to `/v1/auth`) and a readable `tinylord_csrf`. `POST /v1/auth/refresh`
//! requires **both** the refresh cookie and an `x-csrf-token` header, and rotates
//! all three. `reqwest`'s cookie store isn't compiled in, so we capture the
//! refresh cookie from `Set-Cookie` and the csrf token from the JSON body and
//! resend them by hand on refresh.

use std::sync::Mutex;
use std::time::Duration;

use reqwest::header::{HeaderMap, AUTHORIZATION, COOKIE, SET_COOKIE};
use reqwest::{Method, StatusCode};
use serde::Deserialize;

/// Timeout for every non-streaming request. Kept off the SSE subscribe stream
/// (which is long-lived) by setting it per-request rather than on the client.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound on the connection (DNS + TCP + TLS) phase for **every** request,
/// including the long-lived SSE `subscribe` stream. Set on the client builder
/// rather than per-request so it applies to the subscribe stream too — the
/// stream must never carry a full request timeout (it stays open by design),
/// but it must still fail fast when the server is unreachable rather than
/// hanging on a dead connect. A tunneled/unreachable host now errors within
/// this bound instead of stalling indefinitely.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default/maximum page size for `query` pagination (TinyLord clamps to its
/// configured `max_query_limit`, default 500).
const PAGE_SIZE: u32 = 500;

/// A document as returned by TinyLord (the §7.4 envelope). `updated_at` /
/// `created_at` are epoch **milliseconds** (`ids::now_ms`). There is no separate
/// version/revision field, so multi-device concurrency is last-writer-wins.
#[derive(Debug, Clone, Deserialize)]
pub struct DocEnvelope {
    pub id: String,
    #[allow(dead_code)]
    pub created_at: i64,
    #[allow(dead_code)]
    pub updated_at: i64,
    pub doc: serde_json::Value,
}

/// A realtime change event decoded from an SSE `change` frame's JSON `data`.
/// `doc` is present for insert/update, `null` for delete.
#[derive(Debug, Clone, Deserialize)]
pub struct ChangeEvent {
    pub seq: i64,
    #[allow(dead_code)]
    pub collection: String,
    pub op: String,
    pub id: String,
    #[serde(default)]
    pub doc: Option<serde_json::Value>,
}

/// Errors from the client, classified so callers can react (network blips →
/// reconnect banner; auth failure → re-prompt login).
#[derive(Debug)]
pub enum TinyError {
    /// The server was unreachable (DNS, connect, timeout, TLS).
    Network(String),
    /// Login/refresh rejected the credentials.
    Auth(String),
    /// The refresh-once path failed — the session is gone; unlock again.
    SessionExpired,
    /// The server returned a non-success status with a message.
    Api { status: u16, message: String },
}

impl TinyError {
    /// A user-facing message. Network failures use the fixed copy the UI expects.
    pub fn user_message(&self) -> String {
        match self {
            TinyError::Network(_) => {
                "TinyLord unreachable — check the server or your connection".to_string()
            }
            TinyError::Auth(m) => m.clone(),
            TinyError::SessionExpired => "Session expired — unlock this vault again.".to_string(),
            TinyError::Api { status, message } => {
                if message.is_empty() {
                    format!("TinyLord error (HTTP {status})")
                } else {
                    format!("TinyLord: {message}")
                }
            }
        }
    }

    /// True for a transient failure the SSE loop should retry (vs. an auth error
    /// that needs the user).
    pub fn is_transient(&self) -> bool {
        matches!(self, TinyError::Network(_) | TinyError::Api { .. })
    }
}

impl From<TinyError> for String {
    fn from(e: TinyError) -> String {
        e.user_message()
    }
}

fn network(e: reqwest::Error) -> TinyError {
    TinyError::Network(e.to_string())
}

/// The mutable session credentials, replaced wholesale on login/refresh.
struct Tokens {
    access: String,
    csrf: String,
    refresh: String,
}

impl std::fmt::Debug for TinyClient {
    /// Manual impl so a Debug dump can never leak the session tokens.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TinyClient")
            .field("base", &self.base)
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

/// A connected TinyLord client for one vault (one base URL + database).
pub struct TinyClient {
    /// Base URL, no trailing slash (e.g. `https://notes.example.com`).
    base: String,
    /// Target database name.
    db: String,
    http: reqwest::Client,
    tokens: Mutex<Tokens>,
}

/// Shape of the login/refresh JSON response.
#[derive(Deserialize)]
struct AuthResponse {
    access_token: String,
    csrf_token: String,
}

impl TinyClient {
    /// Logs in and returns a ready client. A wrong username/password is
    /// [`TinyError::Auth`]; an unreachable server is [`TinyError::Network`].
    pub async fn login(
        base_url: &str,
        database: &str,
        username: &str,
        password: &str,
    ) -> Result<TinyClient, TinyError> {
        let base = normalize_base(base_url);
        // `connect_timeout` on the builder bounds the connect phase of every
        // request made with this client — non-streaming calls AND the SSE
        // subscribe stream — so an unreachable server fails fast. A full
        // request `timeout` is deliberately NOT set on the builder (it would
        // kill the long-lived subscribe stream); non-streaming calls set it
        // per-request instead (see `login`/`refresh`/`send`).
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(network)?;

        let url = format!("{base}/v1/auth/login");
        let resp = http
            .post(&url)
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({ "username": username, "password": password }))
            .send()
            .await
            .map_err(network)?;

        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(TinyError::Auth("Invalid username or password".to_string()));
        }
        if !resp.status().is_success() {
            return Err(api_error(resp).await);
        }

        let refresh = extract_cookie(resp.headers(), "tinylord_refresh").unwrap_or_default();
        let body: AuthResponse = resp.json().await.map_err(network)?;

        Ok(TinyClient {
            base,
            db: database.to_string(),
            http,
            tokens: Mutex::new(Tokens {
                access: body.access_token,
                csrf: body.csrf_token,
                refresh,
            }),
        })
    }

    fn access_token(&self) -> String {
        self.tokens.lock().unwrap().access.clone()
    }

    /// Rotates the session via `/v1/auth/refresh`. Returns `Err(SessionExpired)`
    /// when the refresh session is gone (expired/rotated/revoked).
    async fn refresh(&self) -> Result<(), TinyError> {
        let (refresh, csrf) = {
            let t = self.tokens.lock().unwrap();
            (t.refresh.clone(), t.csrf.clone())
        };
        if refresh.is_empty() {
            return Err(TinyError::SessionExpired);
        }
        let url = format!("{}/v1/auth/refresh", self.base);
        let resp = self
            .http
            .post(&url)
            .timeout(REQUEST_TIMEOUT)
            .header(COOKIE, format!("tinylord_refresh={refresh}"))
            .header("x-csrf-token", csrf)
            .send()
            .await
            .map_err(network)?;

        if resp.status() == StatusCode::UNAUTHORIZED || resp.status() == StatusCode::FORBIDDEN {
            return Err(TinyError::SessionExpired);
        }
        if !resp.status().is_success() {
            return Err(api_error(resp).await);
        }

        let new_refresh = extract_cookie(resp.headers(), "tinylord_refresh");
        let body: AuthResponse = resp.json().await.map_err(network)?;
        let mut t = self.tokens.lock().unwrap();
        t.access = body.access_token;
        t.csrf = body.csrf_token;
        if let Some(r) = new_refresh {
            t.refresh = r;
        }
        Ok(())
    }

    /// Sends an authenticated request, refreshing once on `401`. `body` is JSON
    /// (cloned for the retry). Returns the raw response on any non-401 status.
    async fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<reqwest::Response, TinyError> {
        let build = |access: &str| {
            let mut req = self
                .http
                .request(method.clone(), url)
                .timeout(REQUEST_TIMEOUT)
                .header(AUTHORIZATION, format!("Bearer {access}"));
            if let Some(b) = body {
                req = req.json(b);
            }
            req
        };

        let resp = build(&self.access_token()).send().await.map_err(network)?;
        if resp.status() != StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }
        // One refresh + retry.
        self.refresh().await?;
        let resp = build(&self.access_token()).send().await.map_err(network)?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(TinyError::SessionExpired);
        }
        Ok(resp)
    }

    fn coll_url(&self, coll: &str, suffix: &str) -> String {
        format!(
            "{}/v1/db/{}/collections/{}{}",
            self.base, self.db, coll, suffix
        )
    }

    /// Creates a document, returning the server envelope (with its new id).
    pub async fn create_doc(
        &self,
        coll: &str,
        doc: &serde_json::Value,
    ) -> Result<DocEnvelope, TinyError> {
        let url = self.coll_url(coll, "/documents");
        let resp = self.send(Method::POST, &url, Some(doc)).await?;
        if !resp.status().is_success() {
            return Err(api_error(resp).await);
        }
        resp.json().await.map_err(network)
    }

    /// Replaces (upserts) the document at `id`.
    pub async fn put_doc(
        &self,
        coll: &str,
        id: &str,
        doc: &serde_json::Value,
    ) -> Result<DocEnvelope, TinyError> {
        let url = self.coll_url(coll, &format!("/documents/{id}"));
        let resp = self.send(Method::PUT, &url, Some(doc)).await?;
        if !resp.status().is_success() {
            return Err(api_error(resp).await);
        }
        resp.json().await.map_err(network)
    }

    /// Deletes the document at `id`. Returns false if it didn't exist (404).
    pub async fn delete_doc(&self, coll: &str, id: &str) -> Result<bool, TinyError> {
        let url = self.coll_url(coll, &format!("/documents/{id}"));
        let resp = self.send(Method::DELETE, &url, None).await?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !resp.status().is_success() {
            return Err(api_error(resp).await);
        }
        Ok(true)
    }

    /// Fetches every document in `coll` (optionally filtered), following the
    /// cursor until the last page. Uses the default id-ordered sort so cursor
    /// pagination applies.
    pub async fn query_all(
        &self,
        coll: &str,
        filter: Option<serde_json::Value>,
    ) -> Result<Vec<DocEnvelope>, TinyError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut req = serde_json::json!({ "limit": PAGE_SIZE });
            if let Some(f) = &filter {
                req["filter"] = f.clone();
            }
            if let Some(c) = &cursor {
                req["cursor"] = serde_json::Value::String(c.clone());
            }
            let url = self.coll_url(coll, "/query");
            let resp = self.send(Method::POST, &url, Some(&req)).await?;
            if !resp.status().is_success() {
                return Err(api_error(resp).await);
            }
            let page: QueryPage = resp.json().await.map_err(network)?;
            let n = page.items.len();
            out.extend(page.items);
            match page.next_cursor {
                Some(c) if n > 0 => cursor = Some(c),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Fetches every document in `coll` projected down to its `path` field —
    /// used to seed the attachments map without transferring the (potentially
    /// large) `bytes_b64` payloads. Returns `(id, path)` pairs.
    pub async fn query_paths(&self, coll: &str) -> Result<Vec<(String, String)>, TinyError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut req = serde_json::json!({ "limit": PAGE_SIZE, "projection": ["path"] });
            if let Some(c) = &cursor {
                req["cursor"] = serde_json::Value::String(c.clone());
            }
            let url = self.coll_url(coll, "/query");
            let resp = self.send(Method::POST, &url, Some(&req)).await?;
            if !resp.status().is_success() {
                return Err(api_error(resp).await);
            }
            let page: QueryPage = resp.json().await.map_err(network)?;
            let n = page.items.len();
            for env in page.items {
                if let Some(p) = env.doc.get("path").and_then(|v| v.as_str()) {
                    out.push((env.id, p.to_string()));
                }
            }
            match page.next_cursor {
                Some(c) if n > 0 => cursor = Some(c),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Finds the single document whose `path` field equals `path`, if any.
    pub async fn find_by_path(
        &self,
        coll: &str,
        path: &str,
    ) -> Result<Option<DocEnvelope>, TinyError> {
        let filter = serde_json::json!({ "path": path });
        let items = self.query_all(coll, Some(filter)).await?;
        Ok(items.into_iter().next())
    }

    /// Best-effort: create a (non-unique) index on `$.path` for a collection so
    /// path lookups don't table-scan. Index creation requires the per-database
    /// `admin` grant, so a plain write user gets a 403 here — that's fine, the
    /// query still works without the index, so any error is swallowed.
    pub async fn ensure_path_index(&self, coll: &str) {
        let url = self.coll_url(coll, "/indexes");
        let body = serde_json::json!({ "path": "$.path", "unique": false });
        let _ = self.send(Method::POST, &url, Some(&body)).await;
    }

    /// Opens the SSE subscribe stream for `coll`, resuming after `last_event_id`
    /// when provided. Returns the streaming response; the caller reads frames
    /// with [`reqwest::Response::chunk`]. No request timeout is set (the stream
    /// is long-lived); a `401` refreshes once before retrying.
    pub async fn open_subscribe(
        &self,
        coll: &str,
        last_event_id: Option<i64>,
    ) -> Result<reqwest::Response, TinyError> {
        let url = self.coll_url(coll, "/subscribe");
        let build = |access: &str| {
            let mut req = self
                .http
                .get(&url)
                .header(AUTHORIZATION, format!("Bearer {access}"))
                .header("accept", "text/event-stream");
            if let Some(id) = last_event_id {
                req = req.header("last-event-id", id.to_string());
            }
            req
        };

        let resp = build(&self.access_token()).send().await.map_err(network)?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            self.refresh().await?;
            let resp = build(&self.access_token()).send().await.map_err(network)?;
            if resp.status() == StatusCode::UNAUTHORIZED {
                return Err(TinyError::SessionExpired);
            }
            if !resp.status().is_success() {
                return Err(api_error(resp).await);
            }
            return Ok(resp);
        }
        if !resp.status().is_success() {
            return Err(api_error(resp).await);
        }
        Ok(resp)
    }
}

#[derive(Deserialize)]
struct QueryPage {
    items: Vec<DocEnvelope>,
    #[serde(default)]
    next_cursor: Option<String>,
}

/// Builds an [`TinyError::Api`] from a failed response, reading its `{error}` or
/// `{message}` body when present.
async fn api_error(resp: reqwest::Response) -> TinyError {
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| {
            v.get("message")
                .or_else(|| v.get("error"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or(text);
    TinyError::Api { status, message }
}

/// Extracts a cookie value by name from a response's `Set-Cookie` headers.
fn extract_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    for value in headers.get_all(SET_COOKIE) {
        if let Ok(s) = value.to_str() {
            for part in s.split(';') {
                let part = part.trim();
                if let Some(v) = part.strip_prefix(&prefix) {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Trims trailing slashes and whitespace from a base URL, defaulting a missing
/// scheme to `https://`.
pub fn normalize_base(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

// ---------------------------------------------------------------------------
// SSE frame parser
// ---------------------------------------------------------------------------

/// One decoded SSE frame: an event name and its accumulated `data` payload.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SseFrame {
    pub event: String,
    pub data: String,
}

/// Incremental SSE parser over a byte stream. Feed raw chunks; it yields
/// complete frames (delimited by a blank line), following the `event:`/`data:`
/// field grammar. `id:` is carried inside the `data` JSON (`seq`), so it is not
/// tracked here.
#[derive(Default)]
pub struct SseParser {
    buf: String,
    event: String,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk of bytes and returns any frames completed by it.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseFrame> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut frames = Vec::new();
        // Process complete lines (terminated by \n); keep any trailing partial.
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=pos).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                // Blank line → dispatch the accumulated frame (if any).
                if !self.event.is_empty() || !self.data.is_empty() {
                    frames.push(SseFrame {
                        event: std::mem::take(&mut self.event),
                        data: std::mem::take(&mut self.data).join("\n"),
                    });
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix(':') {
                let _ = rest; // comment / keep-alive line
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line, ""),
            };
            match field {
                "event" => self.event = value.to_string(),
                "data" => self.data.push(value.to_string()),
                _ => {} // "id", "retry", unknown → ignored
            }
        }
        frames
    }
}

#[cfg(test)]
mod parser_tests {
    use super::*;

    #[test]
    fn parses_single_change_frame() {
        let mut p = SseParser::new();
        let frames = p.feed(b"event: change\ndata: {\"seq\":1}\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event, "change");
        assert_eq!(frames[0].data, "{\"seq\":1}");
    }

    #[test]
    fn splits_across_chunks_and_ignores_comments() {
        let mut p = SseParser::new();
        assert!(p.feed(b": keep-alive\nevent: chan").is_empty());
        assert!(p.feed(b"ge\ndata: {\"a\":").is_empty());
        let frames = p.feed(b"1}\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event, "change");
        assert_eq!(frames[0].data, "{\"a\":1}");
    }

    #[test]
    fn multiline_data_is_joined() {
        let mut p = SseParser::new();
        let frames = p.feed(b"event: x\ndata: a\ndata: b\n\n");
        assert_eq!(frames[0].data, "a\nb");
    }

    #[test]
    fn resync_frame_has_no_data() {
        let mut p = SseParser::new();
        let frames = p.feed(b"event: resync\ndata: {}\n\n");
        assert_eq!(frames[0].event, "resync");
    }
}
