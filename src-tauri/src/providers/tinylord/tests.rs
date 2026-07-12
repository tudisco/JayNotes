//! Tests for the tinylord provider.
//!
//! Two layers, neither touching a live instance:
//!
//! 1. **wiremock** fakes of the TinyLord REST API (login/refresh token dance,
//!    document CRUD envelopes, cursor pagination, error classification).
//! 2. A **hand-rolled tokio TCP stub** for the SSE `/subscribe` stream (wiremock
//!    can't hold a streaming response open), exercising `open_subscribe` + the
//!    frame parser end-to-end over a real socket.
//!
//! A third, `#[ignore]`d test drives the REAL TinyLord binary (built from its
//! repo) through an end-to-end create/edit/SSE round trip — see
//! [`full_integration_against_real_tinylord`] for how to run it.

use super::client::{normalize_base, ChangeEvent, SseParser, TinyClient, TinyError};
use super::*;

use rusqlite::Connection;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

#[test]
fn provider_metadata_shape() {
    let meta = TinylordProvider.metadata();
    assert_eq!(meta.kind, "tinylord");
    assert!(meta.capabilities.needs_unlock);
    assert!(!meta.capabilities.reveal_in_finder);
    assert!(!meta.capabilities.folder_backed);
    assert_eq!(meta.unlock_label.as_deref(), Some("Sign in"));

    let keys: Vec<&str> = meta.config_fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, vec!["url", "database", "username", "password"]);
    let db_field = &meta.config_fields[1];
    assert_eq!(db_field.default.as_deref(), Some("jaynotes"));
    assert_eq!(meta.config_fields[3].field_type, "password");
}

#[test]
fn sessions_store_get_lock() {
    let s = TinyLordSessions::default();
    assert!(!s.is_unlocked("v1"));
    s.store("v1", "hunter2");
    assert!(s.is_unlocked("v1"));
    assert_eq!(s.get("v1").as_deref(), Some("hunter2"));
    s.lock("v1");
    assert!(!s.is_unlocked("v1"));
    assert_eq!(s.get("v1"), None);
}

#[test]
fn base64_roundtrip_and_reject() {
    for data in [b"".as_slice(), b"a", b"ab", b"abc", b"abcd", &[0u8, 255, 128, 7]] {
        let enc = base64_encode(data);
        assert_eq!(base64_decode(&enc).unwrap(), data, "roundtrip {data:?}");
    }
    assert!(base64_decode("not base64 !!!").is_none());
}

#[test]
fn note_doc_carries_path_title_content_mtime() {
    let doc = note_doc("folder/My Note.md", "# hi");
    assert_eq!(doc["path"], "folder/My Note.md");
    assert_eq!(doc["title"], "My Note");
    assert_eq!(doc["content"], "# hi");
    assert!(doc["mtime"].as_i64().unwrap() > 0);
}

#[test]
fn normalize_base_variants() {
    assert_eq!(normalize_base("https://x.com/"), "https://x.com");
    assert_eq!(normalize_base(" http://x.com//  "), "http://x.com");
    assert_eq!(normalize_base("notes.example.com"), "https://notes.example.com");
}

#[test]
fn maps_are_bidirectional() {
    let mut m = Maps::default();
    m.insert_note("a.md", "id1");
    m.insert_folder("dir", "id2");
    m.insert_attachment("attachments/x.png", "id3");
    assert_eq!(m.note_path.get("id1").unwrap(), "a.md");
    assert_eq!(m.remove_note_by_id("id1").as_deref(), Some("a.md"));
    assert!(m.note_id.is_empty());
    assert_eq!(m.remove_folder_by_path("dir").as_deref(), Some("id2"));
    assert!(m.folder_path.is_empty());
    assert_eq!(m.remove_attachment_by_id("id3").as_deref(), Some("attachments/x.png"));
    assert!(m.attach_id.is_empty());
}

fn env(id: &str, doc: serde_json::Value) -> DocEnvelope {
    serde_json::from_value(json!({
        "id": id, "created_at": 1, "updated_at": 1, "doc": doc
    }))
    .unwrap()
}

fn mem_index() -> Arc<Mutex<Option<Index>>> {
    let idx = Index::from_conn(Connection::open_in_memory().unwrap(), std::path::Path::new("/tmp"))
        .unwrap();
    Arc::new(Mutex::new(Some(idx)))
}

#[test]
fn apply_full_sync_populates_and_prunes_index() {
    let index = mem_index();
    let maps = Arc::new(Mutex::new(Maps::default()));

    // First sync: two notes, one folder, one attachment; hidden note excluded
    // from the index but kept in the maps (AI revisions need addressing).
    let sync = FullSync {
        notes: vec![
            env("n1", json!({"path": "a.md", "content": "alpha body", "mtime": 1})),
            env("n2", json!({"path": "sub/b.md", "content": "beta body", "mtime": 1})),
            env("n3", json!({"path": ".revisions/r.md", "content": "hidden", "mtime": 1})),
        ],
        folders: vec![env("f1", json!({"path": "empty-dir"}))],
        attachments: vec![("a1".into(), "attachments/pic.png".into())],
    };
    apply_full_sync(&index, &maps, sync);

    {
        let guard = index.lock().unwrap();
        let idx = guard.as_ref().unwrap();
        assert_eq!(idx.note_count().unwrap(), 2, "hidden note not indexed");
        let hits = idx.search("alpha", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "a.md");
    }
    {
        let m = maps.lock().unwrap();
        assert_eq!(m.note_id.len(), 3, "hidden note still addressable");
        assert_eq!(m.folder_id.get("empty-dir").unwrap(), "f1");
        assert_eq!(m.attach_id.get("attachments/pic.png").unwrap(), "a1");
    }

    // Second sync: a.md vanished server-side → its index row is pruned.
    let sync = FullSync {
        notes: vec![env("n2", json!({"path": "sub/b.md", "content": "beta body", "mtime": 2}))],
        folders: vec![],
        attachments: vec![],
    };
    apply_full_sync(&index, &maps, sync);
    {
        let guard = index.lock().unwrap();
        let idx = guard.as_ref().unwrap();
        assert_eq!(idx.note_count().unwrap(), 1);
        assert!(idx.search("alpha", 10).unwrap().is_empty(), "pruned note unsearchable");
    }
    assert!(maps.lock().unwrap().note_id.get("a.md").is_none());
}

#[test]
fn self_write_suppression_window() {
    let recent: Arc<Mutex<HashMap<String, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
    register_write(&recent, "note.md");
    assert!(was_recently_written(&recent, "note.md"), "fresh write suppressed");
    assert!(!was_recently_written(&recent, "other.md"), "other paths unaffected");

    // Age the entry past the 2s window: the echo must no longer be suppressed.
    recent
        .lock()
        .unwrap()
        .insert("note.md".into(), Instant::now() - Duration::from_secs(3));
    assert!(!was_recently_written(&recent, "note.md"), "stale write not suppressed");
}

// ---------------------------------------------------------------------------
// wiremock API fakes
// ---------------------------------------------------------------------------

/// The standard login mock: JSON tokens + both cookies.
fn login_mock() -> Mock {
    Mock::given(method("POST"))
        .and(path("/v1/auth/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header(
                    "set-cookie",
                    "tinylord_refresh=refresh-1; Path=/v1/auth; HttpOnly; SameSite=Strict; Max-Age=2592000",
                )
                .append_header(
                    "set-cookie",
                    "tinylord_csrf=csrf-1; Path=/; SameSite=Strict; Max-Age=2592000",
                )
                .set_body_json(json!({
                    "access_token": "access-1",
                    "token_type": "Bearer",
                    "expires_in": 900,
                    "csrf_token": "csrf-1"
                })),
        )
}

async fn logged_in(server: &MockServer) -> TinyClient {
    TinyClient::login(&server.uri(), "jaynotes", "jay", "pw")
        .await
        .expect("login should succeed")
}

#[tokio::test]
async fn login_success_sends_bearer_on_calls() {
    let server = MockServer::start().await;
    login_mock().mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/db/jaynotes/collections/notes/query"))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "id": "01A", "created_at": 1, "updated_at": 2,
                  "doc": { "path": "a.md", "content": "hello", "mtime": 2 } }
            ],
            "next_cursor": null
        })))
        .mount(&server)
        .await;

    let client = logged_in(&server).await;
    let docs = client.query_all("notes", None).await.unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].id, "01A");
    assert_eq!(docs[0].doc["content"], "hello");
}

#[tokio::test]
async fn login_wrong_password_is_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/login"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({ "error": "invalid username or password" })),
        )
        .mount(&server)
        .await;

    let err = TinyClient::login(&server.uri(), "jaynotes", "jay", "wrong")
        .await
        .expect_err("should fail");
    assert!(matches!(err, TinyError::Auth(_)));
    assert!(err.user_message().contains("Invalid username or password"));
}

#[tokio::test]
async fn unreachable_server_yields_fixed_user_message() {
    // A port nothing listens on: connection refused → Network.
    let err = TinyClient::login("http://127.0.0.1:1", "jaynotes", "jay", "pw")
        .await
        .expect_err("should fail");
    assert!(matches!(err, TinyError::Network(_)));
    assert!(err.is_transient());
    assert_eq!(
        err.user_message(),
        "TinyLord unreachable — check the server or your connection"
    );
}

#[tokio::test]
async fn expired_access_token_refreshes_once_and_retries() {
    let server = MockServer::start().await;
    login_mock().mount(&server).await;

    // First query with the stale token → 401 (matches only once).
    Mock::given(method("POST"))
        .and(path("/v1/db/jaynotes/collections/notes/query"))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "error": "expired" })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Refresh requires the cookie + csrf header, rotates everything.
    Mock::given(method("POST"))
        .and(path("/v1/auth/refresh"))
        .and(header("cookie", "tinylord_refresh=refresh-1"))
        .and(header("x-csrf-token", "csrf-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header(
                    "set-cookie",
                    "tinylord_refresh=refresh-2; Path=/v1/auth; HttpOnly",
                )
                .set_body_json(json!({
                    "access_token": "access-2",
                    "token_type": "Bearer",
                    "expires_in": 900,
                    "csrf_token": "csrf-2"
                })),
        )
        .expect(1)
        .mount(&server)
        .await;

    // Retry with the fresh token succeeds.
    Mock::given(method("POST"))
        .and(path("/v1/db/jaynotes/collections/notes/query"))
        .and(header("authorization", "Bearer access-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [], "next_cursor": null
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = logged_in(&server).await;
    let docs = client.query_all("notes", None).await.unwrap();
    assert!(docs.is_empty());
}

#[tokio::test]
async fn failed_refresh_surfaces_session_expired() {
    let server = MockServer::start().await;
    login_mock().mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/db/jaynotes/collections/notes/query"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "error": "expired" })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/refresh"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "error": "gone" })))
        .mount(&server)
        .await;

    let client = logged_in(&server).await;
    let err = client.query_all("notes", None).await.expect_err("should fail");
    assert!(matches!(err, TinyError::SessionExpired));
    assert!(err.user_message().contains("unlock"));
}

#[tokio::test]
async fn query_all_follows_cursor_pagination() {
    let server = MockServer::start().await;
    login_mock().mount(&server).await;

    // Page 2 (cursor present) — mounted first so first-match-wins picks it only
    // when the body actually carries the cursor.
    Mock::given(method("POST"))
        .and(path("/v1/db/jaynotes/collections/notes/query"))
        .and(body_partial_json(json!({ "cursor": "01A" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "id": "01B", "created_at": 1, "updated_at": 1,
                  "doc": { "path": "b.md", "content": "two", "mtime": 1 } }
            ],
            "next_cursor": null
        })))
        .expect(1)
        .mount(&server)
        .await;

    // Page 1 (no cursor) reports a next_cursor.
    Mock::given(method("POST"))
        .and(path("/v1/db/jaynotes/collections/notes/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "id": "01A", "created_at": 1, "updated_at": 1,
                  "doc": { "path": "a.md", "content": "one", "mtime": 1 } }
            ],
            "next_cursor": "01A"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = logged_in(&server).await;
    let docs = client.query_all("notes", None).await.unwrap();
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0].doc["path"], "a.md");
    assert_eq!(docs[1].doc["path"], "b.md");
}

#[tokio::test]
async fn create_put_delete_docs_roundtrip_envelopes() {
    let server = MockServer::start().await;
    login_mock().mount(&server).await;

    Mock::given(method("POST"))
        .and(path("/v1/db/jaynotes/collections/notes/documents"))
        .and(body_partial_json(json!({ "path": "a.md" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "01NEW", "created_at": 5, "updated_at": 5,
            "doc": { "path": "a.md", "content": "x", "mtime": 5 }
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/db/jaynotes/collections/notes/documents/01NEW"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "01NEW", "created_at": 5, "updated_at": 9,
            "doc": { "path": "a.md", "content": "y", "mtime": 9 }
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/db/jaynotes/collections/notes/documents/01NEW"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/db/jaynotes/collections/notes/documents/GONE"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({ "error": "document not found" })),
        )
        .mount(&server)
        .await;

    let client = logged_in(&server).await;

    let created = client
        .create_doc("notes", &json!({ "path": "a.md", "content": "x", "mtime": 5 }))
        .await
        .unwrap();
    assert_eq!(created.id, "01NEW");

    let updated = client
        .put_doc("notes", "01NEW", &json!({ "path": "a.md", "content": "y", "mtime": 9 }))
        .await
        .unwrap();
    assert_eq!(updated.updated_at, 9);

    assert!(client.delete_doc("notes", "01NEW").await.unwrap());
    assert!(!client.delete_doc("notes", "GONE").await.unwrap(), "404 → false");
}

#[tokio::test]
async fn find_by_path_filters_on_path_equality() {
    let server = MockServer::start().await;
    login_mock().mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/db/jaynotes/collections/notes/query"))
        .and(body_partial_json(json!({ "filter": { "path": "sub/b.md" } })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "id": "01B", "created_at": 1, "updated_at": 1,
                  "doc": { "path": "sub/b.md", "content": "found", "mtime": 1 } }
            ],
            "next_cursor": null
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/db/jaynotes/collections/notes/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [], "next_cursor": null
        })))
        .mount(&server)
        .await;

    let client = logged_in(&server).await;
    let hit = client.find_by_path("notes", "sub/b.md").await.unwrap();
    assert_eq!(hit.unwrap().doc["content"], "found");
    let miss = client.find_by_path("notes", "nope.md").await.unwrap();
    assert!(miss.is_none());
}

#[tokio::test]
async fn api_error_carries_server_message() {
    let server = MockServer::start().await;
    login_mock().mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/db/jaynotes/collections/notes/documents"))
        .respond_with(
            ResponseTemplate::new(413).set_body_json(json!({ "error": "document too large" })),
        )
        .mount(&server)
        .await;

    let client = logged_in(&server).await;
    let err = client
        .create_doc("notes", &json!({ "path": "big.md" }))
        .await
        .expect_err("should fail");
    match &err {
        TinyError::Api { status, message } => {
            assert_eq!(*status, 413);
            assert_eq!(message, "document too large");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
    assert!(err.user_message().contains("document too large"));
}

#[tokio::test]
async fn ensure_path_index_swallows_forbidden() {
    let server = MockServer::start().await;
    login_mock().mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/db/jaynotes/collections/notes/indexes"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(json!({ "error": "requires 'admin'" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = logged_in(&server).await;
    client.ensure_path_index("notes").await; // must not panic or error
}

// ---------------------------------------------------------------------------
// SSE via a hand-rolled tokio TCP stub
// ---------------------------------------------------------------------------

/// A minimal HTTP/1.1 stub speaking just enough for login + one SSE subscribe:
/// reads a request, answers `/v1/auth/login` with JSON, and answers
/// `/subscribe` with a `text/event-stream` that emits `frames` then closes.
async fn spawn_sse_stub(frames: Vec<String>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let frames = frames.clone();
            tokio::spawn(async move {
                // Read until the header terminator (requests here have small,
                // fully-buffered bodies).
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut tmp).await {
                        Ok(0) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        Err(_) => return,
                    }
                }
                let req = String::from_utf8_lossy(&buf);
                let first_line = req.lines().next().unwrap_or("");

                if first_line.contains("/v1/auth/login") {
                    let body = r#"{"access_token":"access-sse","token_type":"Bearer","expires_in":900,"csrf_token":"csrf-sse"}"#;
                    // `connection: close` so reqwest never tries to reuse this
                    // one-shot connection for the subscribe request.
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nset-cookie: tinylord_refresh=r1; Path=/v1/auth; HttpOnly\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                } else if first_line.contains("/subscribe") {
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n",
                        )
                        .await;
                    // A keep-alive comment first (must be ignored), then frames.
                    let _ = sock.write_all(b": keep-alive\n\n").await;
                    for f in &frames {
                        let _ = sock.write_all(f.as_bytes()).await;
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    // Leave briefly open, then close (client sees end-of-stream).
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            });
        }
    });

    format!("http://{addr}")
}

#[tokio::test]
async fn sse_subscribe_decodes_change_events_over_a_real_socket() {
    let frames = vec![
        // insert, update, delete for the notes collection — the §9 event shape.
        "id: 1\nevent: change\ndata: {\"seq\":1,\"collection\":\"notes\",\"op\":\"insert\",\"id\":\"01A\",\"doc\":{\"path\":\"a.md\",\"content\":\"v1\",\"mtime\":1}}\n\n".to_string(),
        "id: 2\nevent: change\ndata: {\"seq\":2,\"collection\":\"notes\",\"op\":\"update\",\"id\":\"01A\",\"doc\":{\"path\":\"a.md\",\"content\":\"v2\",\"mtime\":2}}\n\n".to_string(),
        "id: 3\nevent: change\ndata: {\"seq\":3,\"collection\":\"notes\",\"op\":\"delete\",\"id\":\"01A\",\"doc\":null}\n\n".to_string(),
        "event: resync\ndata: {}\n\n".to_string(),
    ];
    let base = spawn_sse_stub(frames).await;

    let client = TinyClient::login(&base, "jaynotes", "jay", "pw").await.unwrap();
    let mut resp = client.open_subscribe("notes", None).await.unwrap();

    let mut parser = SseParser::new();
    let mut events: Vec<ChangeEvent> = Vec::new();
    let mut resyncs = 0;
    while let Ok(Some(bytes)) = resp.chunk().await {
        for frame in parser.feed(&bytes) {
            match frame.event.as_str() {
                "change" => events.push(serde_json::from_str(&frame.data).unwrap()),
                "resync" => resyncs += 1,
                _ => {}
            }
        }
    }

    assert_eq!(events.len(), 3);
    assert_eq!(events[0].op, "insert");
    assert_eq!(events[0].doc.as_ref().unwrap()["content"], "v1");
    assert_eq!(events[1].op, "update");
    assert_eq!(events[2].op, "delete");
    assert!(events[2].doc.is_none());
    assert_eq!(resyncs, 1);
}

#[tokio::test]
async fn sse_subscribe_sends_last_event_id_header() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();

    tokio::spawn(async move {
        let mut tx = Some(tx);
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut tmp).await {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    Err(_) => break,
                }
            }
            let req = String::from_utf8_lossy(&buf).into_owned();
            if req.contains("/v1/auth/login") {
                let body = r#"{"access_token":"a","token_type":"Bearer","expires_in":900,"csrf_token":"c"}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            } else if req.contains("/subscribe") {
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\n")
                    .await;
                if let Some(tx) = tx.take() {
                    let _ = tx.send(req);
                }
            }
        }
    });

    let client = TinyClient::login(&format!("http://{addr}"), "jaynotes", "jay", "pw")
        .await
        .unwrap();
    let _resp = client.open_subscribe("notes", Some(42)).await.unwrap();
    let req = rx.await.unwrap();
    assert!(
        req.to_lowercase().contains("last-event-id: 42"),
        "subscribe request must resume with Last-Event-ID; got:\n{req}"
    );
}

// ---------------------------------------------------------------------------
// Full integration against the REAL TinyLord binary (opt-in)
// ---------------------------------------------------------------------------

/// End-to-end test against a real TinyLord instance built from its repo.
///
/// Ignored by default because it builds and runs an external server binary.
/// Run with:
///
/// ```sh
/// cargo test --features provider-tinylord tinylord_real -- --ignored --nocapture
/// ```
///
/// Requires the TinyLord repo at `/Volumes/WorkDrive/Hot/Jason3/TinyLord` and a
/// working `cargo` toolchain. The test: builds the binary, starts it with a
/// temp config (auto-generated encryption key, registration enabled, loopback
/// port), bootstraps an admin token from stdout, creates a `jaynotes` database
/// + a browser user + a write grant, then drives `TinyClient` through login →
/// create → find → update → SSE change observation → delete.
#[tokio::test]
#[ignore = "runs a real TinyLord server; see doc comment"]
async fn tinylord_real_end_to_end() {
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};

    const TL_REPO: &str = "/Volumes/WorkDrive/Hot/Jason3/TinyLord";

    // Kill the child on every exit path.
    struct Guard(Child);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    // 1. Build the server binary.
    let status = Command::new("cargo")
        .args(["build"])
        .current_dir(TL_REPO)
        .status()
        .expect("cargo must be runnable");
    assert!(status.success(), "TinyLord build failed");
    let binary = format!("{TL_REPO}/target/debug/tinylord");

    // 2. Temp workspace + config. Port: bind :0 once to pick a free one.
    let dir = std::env::temp_dir().join(format!("jaynotes-tl-int-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let config_path = dir.join("tinylord.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[server]
bind = "127.0.0.1:{port}"
data_dir = "{d}/data"
snapshot_dir = "{d}/snapshots"

[auth]
public_registration = true
secure_cookies = false

[encryption]
enabled = true
key_source = "key_file"
key_file = "{d}/tinylord.key"
"#,
            d = dir.display()
        ),
    )
    .unwrap();

    // 3. Generate the key, then start the server and scrape the admin token.
    let status = Command::new(&binary)
        .args(["--config", config_path.to_str().unwrap(), "keygen", "--out"])
        .arg(dir.join("tinylord.key"))
        .status()
        .expect("keygen runs");
    assert!(status.success(), "keygen failed");

    let mut child = Command::new(&binary)
        .args(["--config", config_path.to_str().unwrap(), "serve"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("server starts");
    let stdout = child.stdout.take().unwrap();
    let mut guard = Guard(child);

    let admin_token = {
        let reader = BufReader::new(stdout);
        let mut token = None;
        for line in reader.lines().map_while(Result::ok) {
            let t = line.trim();
            // The bootstrap block prints the bare token on its own line.
            if t.len() > 20 && !t.contains(' ') && !t.contains('=') {
                token = Some(t.to_string());
                break;
            }
        }
        token.expect("bootstrap admin token printed")
    };

    // Wait for /health.
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();
    for i in 0.. {
        if let Ok(r) = http.get(format!("{base}/health")).send().await {
            if r.status().is_success() {
                break;
            }
        }
        assert!(i < 100, "server never became healthy");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 4. Admin bootstrap: database, browser user, grant.
    let auth = format!("Bearer {admin_token}");
    let r = http
        .post(format!("{base}/v1/admin/databases"))
        .header("authorization", &auth)
        .json(&json!({ "name": "jaynotes" }))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "create database: {}", r.status());

    let r = http
        .post(format!("{base}/v1/auth/register"))
        .json(&json!({ "username": "jay", "password": "integration-password" }))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "register: {}", r.status());
    let tokens: serde_json::Value = r.json().await.unwrap();
    let me: serde_json::Value = http
        .get(format!("{base}/v1/auth/me"))
        .header(
            "authorization",
            format!("Bearer {}", tokens["access_token"].as_str().unwrap()),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let user_id = me["id"].as_str().unwrap();

    let r = http
        .post(format!("{base}/v1/admin/grants"))
        .header("authorization", &auth)
        .json(&json!({ "principal_id": user_id, "database": "jaynotes", "role": "write" }))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "grant: {}", r.status());

    // 5. Drive the provider's client end-to-end.
    let client = TinyClient::login(&base, "jaynotes", "jay", "integration-password")
        .await
        .expect("provider login");

    // Subscribe BEFORE writing so the create is observed live.
    let mut sub = client.open_subscribe("notes", None).await.unwrap();

    let created = client
        .create_doc("notes", &note_doc("integration/Note.md", "# hello from the test"))
        .await
        .unwrap();

    // Observe the SSE insert.
    let mut parser = SseParser::new();
    let mut seen_insert = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    'outer: while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), sub.chunk()).await {
            Ok(Ok(Some(bytes))) => {
                for frame in parser.feed(&bytes) {
                    if frame.event == "change" {
                        let ev: ChangeEvent = serde_json::from_str(&frame.data).unwrap();
                        if ev.op == "insert" && ev.id == created.id {
                            seen_insert = true;
                            break 'outer;
                        }
                    }
                }
            }
            _ => break,
        }
    }
    assert!(seen_insert, "SSE insert event observed");

    // Find / update / delete round trip.
    let found = client
        .find_by_path("notes", "integration/Note.md")
        .await
        .unwrap()
        .expect("created note is queryable by path");
    assert_eq!(found.id, created.id);

    let updated = client
        .put_doc("notes", &created.id, &note_doc("integration/Note.md", "# edited"))
        .await
        .unwrap();
    assert_eq!(updated.doc["content"], "# edited");

    assert!(client.delete_doc("notes", &created.id).await.unwrap());
    assert!(client
        .find_by_path("notes", "integration/Note.md")
        .await
        .unwrap()
        .is_none());

    // Explicit teardown (the Drop guard also covers panics above).
    let _ = guard.0.kill();
    std::fs::remove_dir_all(&dir).ok();
}
