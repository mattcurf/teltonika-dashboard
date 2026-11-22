//! Client behaviour against a fake device: token renewal, the poll cache and its
//! invalidation. The fake answers the way RutOS does, including HTTP 207 bulk replies whose
//! items fail with an Authorization error when the token has expired.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{post, put};
use reqwest::Method;
use serde_json::{Value, json};
use teltonika::{Client, Error};

const PASSWORD: &str = "secret";

#[derive(Default)]
struct Fake {
    logins: AtomicUsize,
    tokens: Mutex<HashSet<String>>,
    /// Endpoints of each bulk request, in order.
    bulks: Mutex<Vec<Vec<String>>>,
    /// Rejects every token, even a new one.
    reject_all: AtomicBool,
}

impl Fake {
    fn expire_tokens(&self) {
        self.tokens.lock().unwrap().clear();
    }
    fn logins(&self) -> usize {
        self.logins.load(Ordering::SeqCst)
    }
    fn bulks(&self) -> Vec<Vec<String>> {
        self.bulks.lock().unwrap().clone()
    }
    fn authorized(&self, headers: &HeaderMap) -> bool {
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        !self.reject_all.load(Ordering::SeqCst)
            && token.is_some_and(|t| self.tokens.lock().unwrap().contains(t))
    }
}

fn expired() -> Value {
    json!({ "success": false, "errors": [{ "code": 123, "error": "Expired bearer token", "source": "Authorization" }] })
}

async fn login(State(f): State<Arc<Fake>>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let n = f.logins.fetch_add(1, Ordering::SeqCst) + 1;
    if body["password"] != PASSWORD {
        let e = json!({ "success": false, "errors": [{ "code": 120, "error": "Invalid username and/or password", "source": "Authorization" }] });
        return (StatusCode::UNAUTHORIZED, Json(e));
    }
    let token = format!("token{n}");
    f.tokens.lock().unwrap().insert(token.clone());
    (
        StatusCode::OK,
        Json(json!({ "success": true, "data": { "token": token, "expires": 299 } })),
    )
}

async fn bulk(
    State(f): State<Arc<Fake>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let endpoints: Vec<String> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["endpoint"].as_str().unwrap().to_string())
        .collect();
    // A rejected token still gets HTTP 207; every item fails instead.
    if !f.authorized(&headers) {
        return (
            StatusCode::MULTI_STATUS,
            Json(
                json!({ "success": true, "data": endpoints.iter().map(|_| expired()).collect::<Vec<_>>() }),
            ),
        );
    }
    f.bulks.lock().unwrap().push(endpoints.clone());
    let items: Vec<Value> = endpoints
        .iter()
        .map(|e| json!({ "success": true, "data": { "endpoint": e } }))
        .collect();
    (
        StatusCode::MULTI_STATUS,
        Json(json!({ "success": true, "data": items })),
    )
}

async fn thing(State(f): State<Arc<Fake>>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if !f.authorized(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(expired()));
    }
    (
        StatusCode::OK,
        Json(json!({ "success": true, "data": { "ok": true } })),
    )
}

async fn start() -> (Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    let app = axum::Router::new()
        .route("/api/login", post(login))
        .route("/api/bulk", post(bulk))
        .route("/api/thing", put(thing).get(thing))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, url)
}

const FRESH: Duration = Duration::ZERO;
const MINUTE: Duration = Duration::from_secs(60);

#[tokio::test]
async fn logs_in_again_when_bulk_items_report_an_expired_token() {
    let (fake, url) = start().await;
    let c = Client::new(&url, "admin", PASSWORD).unwrap();
    c.poll(&[("/a", FRESH)]).await.unwrap();
    assert_eq!(fake.logins(), 1);

    fake.expire_tokens();
    let r = c.poll(&[("/a", FRESH)]).await.unwrap();
    assert_eq!(r.get("/a").unwrap()["endpoint"], "/api/a");
    assert_eq!(fake.logins(), 2);
}

#[tokio::test]
async fn logs_in_again_on_http_401() {
    let (fake, url) = start().await;
    let c = Client::new(&url, "admin", PASSWORD).unwrap();
    c.call(Method::GET, "/thing", None).await.unwrap();
    fake.expire_tokens();
    assert_eq!(
        c.call(Method::GET, "/thing", None).await.unwrap()["ok"],
        true
    );
    assert_eq!(fake.logins(), 2);
}

#[tokio::test]
async fn reuses_the_token_while_it_is_valid() {
    let (fake, url) = start().await;
    let c = Client::new(&url, "admin", PASSWORD).unwrap();
    for _ in 0..3 {
        c.poll(&[("/a", FRESH)]).await.unwrap();
    }
    assert_eq!(fake.logins(), 1);
}

#[tokio::test]
async fn renews_the_token_only_once_per_request() {
    // A device that rejects every token must not make the client log in over and over.
    let (fake, url) = start().await;
    fake.reject_all.store(true, Ordering::SeqCst);
    let c = Client::new(&url, "admin", PASSWORD).unwrap();
    let r = c.poll(&[("/a", FRESH)]).await.unwrap();
    assert!(matches!(r.get("/a"), Err(Error::Api(m)) if m.contains("Expired bearer token")));
    assert_eq!(fake.logins(), 2);
}

#[tokio::test]
async fn reports_a_wrong_password_without_retrying() {
    let (fake, url) = start().await;
    let c = Client::new(&url, "admin", "wrong").unwrap();
    let err = c
        .poll(&[("/a", FRESH)])
        .await
        .err()
        .expect("login must fail");
    assert!(
        matches!(&err, Error::Api(m) if m.starts_with("login failed")),
        "{err}"
    );
    assert_eq!(fake.logins(), 1);
}

#[tokio::test]
async fn caches_by_max_age_and_drops_the_cache_after_a_change() {
    let (fake, url) = start().await;
    let c = Client::new(&url, "admin", PASSWORD).unwrap();
    let paths = [("/config", MINUTE), ("/status", FRESH)];

    c.poll(&paths).await.unwrap();
    // The second poll fetches only the path whose cached result is too old.
    let r = c.poll(&paths).await.unwrap();
    assert_eq!(r.get("/config").unwrap()["endpoint"], "/api/config");
    // A change on the device invalidates every cached result.
    c.call(Method::PUT, "/thing", Some(json!({ "enabled": "1" })))
        .await
        .unwrap();
    c.poll(&paths).await.unwrap();

    assert_eq!(
        fake.bulks(),
        vec![
            vec!["/api/config".to_string(), "/api/status".to_string()],
            vec!["/api/status".to_string()],
            vec!["/api/config".to_string(), "/api/status".to_string()],
        ]
    );
}

#[tokio::test]
async fn reports_an_unreachable_device_as_a_transport_error() {
    let c = Client::new("http://127.0.0.1:1", "admin", PASSWORD).unwrap();
    assert!(matches!(
        c.poll(&[("/a", FRESH)]).await,
        Err(Error::Transport(_))
    ));
}
