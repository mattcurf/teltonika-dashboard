//! The device-independent part of a mock: it speaks the Teltonika Web API the way the real
//! devices do. Login returns bearer tokens that expire, POST /api/bulk answers HTTP 207 with
//! one result per request, and errors use the devices' codes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::IntoResponse;
use serde_json::{Map, Value, json};

use crate::juggler::{Hook, Juggler};

/// One config section, such as a port: an object with an "id".
pub type Item = Map<String, Value>;

/// An API error, sent as `{"success": false, "errors": [...]}`. Codes match the devices.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: u32,
    pub message: String,
    pub source: &'static str,
}

impl ApiError {
    pub fn new(status: StatusCode, code: u32, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            source: "Request",
        }
    }
    fn auth(code: u32, message: &str) -> Self {
        ApiError {
            source: "Authorization",
            ..ApiError::new(StatusCode::UNAUTHORIZED, code, message)
        }
    }
    pub fn not_found() -> Self {
        ApiError::new(StatusCode::NOT_FOUND, 122, "Endpoint not implemented")
    }
    pub fn section_not_found(id: &str) -> Self {
        ApiError::new(
            StatusCode::NOT_FOUND,
            122,
            format!("Section {id} not found"),
        )
    }
    pub fn bad_data(message: &str) -> Self {
        ApiError::new(StatusCode::BAD_REQUEST, 101, message)
    }
    fn body(&self) -> Value {
        json!({ "success": false, "errors": [{ "code": self.code, "error": self.message, "source": self.source }] })
    }
}

pub type ApiResult = Result<Value, ApiError>;

/// One API call after login and routing: the method, the path without "/api", and "data".
pub struct Call<'a> {
    pub method: &'a Method,
    pub path: &'a str,
    pub data: &'a Value,
}

impl Call<'_> {
    /// True for `method` on exactly `path`.
    pub fn is(&self, method: Method, path: &str) -> bool {
        *self.method == method && self.path == path
    }
}

/// What a device does: answer calls, and drift its live values once a second.
pub trait Device: Send + 'static {
    /// Answers one call, or `None` when the device has no such endpoint. A change that the
    /// real device logs calls `juggler.fire`, which runs the matching Event Juggler actions.
    fn call(&mut self, call: &Call, juggler: &mut Juggler) -> Option<ApiResult>;
    fn tick(&mut self) {}
}

/// Login settings, the same on both devices.
#[derive(Clone, Debug)]
pub struct Auth {
    pub username: String,
    pub password: String,
    pub token_ttl: Duration,
}

impl Default for Auth {
    fn default() -> Self {
        Auth {
            username: "admin".into(),
            password: "admin01".into(),
            token_ttl: Duration::from_secs(299),
        }
    }
}

/// A list of config sections, such as /ports_settings/config. GET reads the list or one
/// item. PUT on `/{id}` merges one item; PUT on the list merges an array of items by id.
#[derive(Default)]
pub struct ConfigList {
    pub items: Vec<Item>,
}

/// The result of a write: the reply, and each changed item with its values from before.
pub type Changes = Vec<(Item, Item)>;

impl ConfigList {
    pub fn new(items: Vec<Value>) -> Self {
        ConfigList {
            items: items
                .into_iter()
                .map(|v| match v {
                    Value::Object(m) => m,
                    _ => panic!("config items are objects"),
                })
                .collect(),
        }
    }

    pub fn get(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i["id"] == id)
    }

    /// The string value of `key` in item `id`, or "" when either is missing.
    pub fn str(&self, id: &str, key: &str) -> &str {
        self.get(id)
            .and_then(|i| i.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    fn merge(&mut self, id: Option<&str>, values: &Value) -> Result<(Item, Item), ApiError> {
        let id = id.unwrap_or("");
        let item = self
            .items
            .iter_mut()
            .find(|i| i["id"] == id)
            .ok_or_else(|| ApiError::section_not_found(id))?;
        let Value::Object(values) = values else {
            return Err(ApiError::bad_data("data must be an object"));
        };
        let before = item.clone();
        for (k, v) in values {
            if k != "id" {
                item.insert(k.clone(), v.clone());
            }
        }
        Ok((item.clone(), before))
    }

    /// Serves `base` and `base/{id}`. Returns `None` when the call is for another path.
    pub fn route(&mut self, call: &Call, base: &str) -> Option<Result<(Value, Changes), ApiError>> {
        if call.path == base {
            return match *call.method {
                Method::GET => Some(Ok((Value::from(self.list()), vec![]))),
                Method::PUT => Some((|| {
                    let Value::Array(updates) = call.data else {
                        return Err(ApiError::bad_data("data must be an array"));
                    };
                    let mut changes = vec![];
                    for u in updates {
                        changes.push(self.merge(u.get("id").and_then(Value::as_str), u)?);
                    }
                    let reply = changes
                        .iter()
                        .map(|(a, _)| Value::Object(a.clone()))
                        .collect();
                    Ok((Value::Array(reply), changes))
                })()),
                _ => None,
            };
        }
        let id = item_id(call.path, base)?;
        match *call.method {
            Method::GET => Some(
                self.get(&id)
                    .map(|i| (Value::Object(i.clone()), vec![]))
                    .ok_or_else(|| ApiError::section_not_found(&id)),
            ),
            Method::PUT => Some(
                self.merge(Some(&id), call.data)
                    .map(|(after, before)| (Value::Object(after.clone()), vec![(after, before)])),
            ),
            _ => None,
        }
    }

    pub fn list(&self) -> Vec<Value> {
        self.items.iter().cloned().map(Value::Object).collect()
    }
}

/// The `{id}` of `base/{id}`, percent-decoded, or `None` for any other path.
pub fn item_id(path: &str, base: &str) -> Option<String> {
    let rest = path.strip_prefix(base)?.strip_prefix('/')?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (
            bytes[i],
            bytes.get(i + 1).copied().and_then(hex),
            bytes.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

struct State<D> {
    device: D,
    juggler: Juggler,
    tokens: HashMap<String, Instant>,
}

/// A device behind the API: login, tokens, bulk requests and the Event Juggler.
pub struct Mock<D> {
    name: &'static str,
    auth: Auth,
    state: Mutex<State<D>>,
}

impl<D: Device> Mock<D> {
    pub fn new(name: &'static str, device: D, auth: Auth, juggler: Juggler) -> Arc<Self> {
        Arc::new(Mock {
            name,
            auth,
            state: Mutex::new(State {
                device,
                juggler,
                tokens: HashMap::new(),
            }),
        })
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Drifts the live values; the server calls it once a second.
    pub fn tick(&self) {
        self.state.lock().unwrap().device.tick();
    }

    /// The HTTP routes. Every path goes through one handler, as on the devices.
    pub fn router(self: &Arc<Self>) -> Router {
        let mock = self.clone();
        Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let mock = mock.clone();
                async move {
                    let (status, body, hooks) = mock.handle(&method, uri.path(), &headers, &body);
                    for hook in hooks {
                        tokio::spawn(hook.send());
                    }
                    // The devices close every connection; the panel relies on that.
                    let mut res = (status, axum::Json(body)).into_response();
                    res.headers_mut()
                        .insert(header::CONNECTION, "close".parse().unwrap());
                    res
                }
            },
        )
    }

    /// Answers one HTTP request: its status, its JSON body, and the hooks it triggered.
    pub fn handle(
        &self,
        method: &Method,
        path: &str,
        headers: &HeaderMap,
        body: &[u8],
    ) -> (StatusCode, Value, Vec<Hook>) {
        let mut state = self.state.lock().unwrap();
        let reply = self.handle_locked(&mut state, method, path, headers, body);
        let hooks = state.juggler.take_hooks();
        match reply {
            Ok((status, value)) => (status, value, hooks),
            Err(e) => (e.status, e.body(), hooks),
        }
    }

    fn handle_locked(
        &self,
        state: &mut State<D>,
        method: &Method,
        path: &str,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<(StatusCode, Value), ApiError> {
        let body: Value = if body.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(body).map_err(|_| {
                ApiError::new(StatusCode::BAD_REQUEST, 100, "Response not implemented")
            })?
        };
        // Login takes its fields at the top level; every other request has them in "data".
        if path == "/api/login" && method == Method::POST {
            return Ok((StatusCode::OK, ok(self.login(state, &body)?)));
        }
        self.check_token(state, headers)?;
        if path == "/api/bulk" && method == Method::POST {
            // Each request has its own result; the bulk request itself succeeds with 207.
            let calls = body["data"].as_array().cloned().unwrap_or_default();
            let results: Vec<Value> = calls
                .iter()
                .map(|c| {
                    let method = c["method"]
                        .as_str()
                        .unwrap_or("GET")
                        .parse()
                        .unwrap_or(Method::GET);
                    let path = c["endpoint"].as_str().unwrap_or("");
                    match dispatch(state, &method, path, &c["data"]) {
                        Ok(v) => ok(v),
                        Err(e) => e.body(),
                    }
                })
                .collect();
            return Ok((StatusCode::MULTI_STATUS, ok(Value::Array(results))));
        }
        Ok((
            StatusCode::OK,
            ok(dispatch(state, method, path, &body["data"])?),
        ))
    }

    fn login(&self, state: &mut State<D>, body: &Value) -> ApiResult {
        let user = body["username"].as_str();
        if user != Some(&self.auth.username)
            || body["password"].as_str() != Some(&self.auth.password)
        {
            return Err(ApiError::auth(
                120,
                "Invalid username and/or password! Please try again.",
            ));
        }
        let token = format!("{:016x}{:016x}", crate::random_u64(), crate::random_u64());
        state
            .tokens
            .insert(token.clone(), Instant::now() + self.auth.token_ttl);
        Ok(
            json!({ "token": token, "group": "root", "expires": self.auth.token_ttl.as_secs(), "username": user }),
        )
    }

    fn check_token(&self, state: &State<D>, headers: &HeaderMap) -> Result<(), ApiError> {
        let header = headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        let token = header
            .strip_prefix("Bearer ")
            .ok_or_else(|| ApiError::auth(121, "Missing bearer token"))?;
        match state.tokens.get(token) {
            Some(expires) if *expires > Instant::now() => Ok(()),
            _ => Err(ApiError::auth(123, "Expired bearer token")),
        }
    }
}

fn ok(data: Value) -> Value {
    json!({ "success": true, "data": data })
}

fn dispatch<D: Device>(
    state: &mut State<D>,
    method: &Method,
    path: &str,
    data: &Value,
) -> ApiResult {
    let path = path.strip_prefix("/api").unwrap_or(path);
    let path = path.strip_suffix('/').unwrap_or(path);
    let call = Call { method, path, data };
    if let Some(r) = state.juggler.call(&call) {
        return r;
    }
    state
        .device
        .call(&call, &mut state.juggler)
        .unwrap_or_else(|| Err(ApiError::not_found()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// A device with one config list and one read-only endpoint.
    struct Lamp {
        config: ConfigList,
    }
    impl Device for Lamp {
        fn call(&mut self, call: &Call, juggler: &mut Juggler) -> Option<ApiResult> {
            if call.is(Method::GET, "/lamp/status") {
                return Some(Ok(json!({ "on": self.config.str("lamp", "on") == "1" })));
            }
            let (reply, changes) = match self.config.route(call, "/lamp/config")? {
                Ok(r) => r,
                Err(e) => return Some(Err(e)),
            };
            for (after, before) in changes {
                if after["on"] != before["on"] {
                    juggler.fire(
                        "Lamp Events",
                        &format!("lamp {}", after["on"].as_str().unwrap_or("")),
                    );
                }
            }
            Some(Ok(reply))
        }
    }

    fn lamp(ttl: Duration) -> Arc<Mock<Lamp>> {
        let device = Lamp {
            config: ConfigList::new(vec![json!({ "id": "lamp", "on": "1", "color": "red" })]),
        };
        let auth = Auth {
            token_ttl: ttl,
            ..Auth::default()
        };
        Mock::new("Lamp", device, auth, Juggler::default())
    }

    fn call(
        mock: &Mock<Lamp>,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut headers = HeaderMap::new();
        if let Some(t) = token {
            headers.insert(
                header::AUTHORIZATION,
                format!("Bearer {t}").parse().unwrap(),
            );
        }
        let (status, value, _) = mock.handle(&method, path, &headers, body.to_string().as_bytes());
        (status, value)
    }

    fn login(mock: &Mock<Lamp>) -> String {
        let (status, body) = call(
            mock,
            Method::POST,
            "/api/login",
            None,
            json!({ "username": "admin", "password": "admin01" }),
        );
        assert_eq!(status, StatusCode::OK);
        body["data"]["token"].as_str().unwrap().to_string()
    }

    fn error_code(body: &Value) -> u64 {
        body["errors"][0]["code"].as_u64().unwrap()
    }

    #[test]
    fn rejects_wrong_password_missing_and_expired_tokens() {
        let mock = lamp(Duration::from_secs(60));
        let (status, body) = call(
            &mock,
            Method::POST,
            "/api/login",
            None,
            json!({ "username": "admin", "password": "nope" }),
        );
        assert_eq!((status, error_code(&body)), (StatusCode::UNAUTHORIZED, 120));
        assert_eq!(body["errors"][0]["source"], "Authorization");

        let (status, body) = call(&mock, Method::GET, "/api/lamp/status", None, json!({}));
        assert_eq!((status, error_code(&body)), (StatusCode::UNAUTHORIZED, 121));
        let (status, body) = call(
            &mock,
            Method::GET,
            "/api/lamp/status",
            Some("made-up"),
            json!({}),
        );
        assert_eq!((status, error_code(&body)), (StatusCode::UNAUTHORIZED, 123));

        let token = login(&mock);
        let (status, body) = call(
            &mock,
            Method::GET,
            "/api/lamp/status",
            Some(&token),
            json!({}),
        );
        assert_eq!(
            (status, &body["data"]),
            (StatusCode::OK, &json!({ "on": true }))
        );

        // A token past its lifetime fails like an unknown one.
        let expired = lamp(Duration::ZERO);
        let token = login(&expired);
        let (status, body) = call(
            &expired,
            Method::GET,
            "/api/lamp/status",
            Some(&token),
            json!({}),
        );
        assert_eq!((status, error_code(&body)), (StatusCode::UNAUTHORIZED, 123));
    }

    #[test]
    fn bulk_answers_207_with_one_result_per_request() {
        let mock = lamp(Duration::from_secs(60));
        let token = login(&mock);
        let (status, body) = call(
            &mock,
            Method::POST,
            "/api/bulk",
            Some(&token),
            json!({ "data": [
                { "endpoint": "/api/lamp/status", "method": "GET" },
                { "endpoint": "/api/nothing/here", "method": "GET" },
                { "endpoint": "/api/lamp/config/lamp", "method": "PUT", "data": { "on": "0" } },
                { "endpoint": "/api/lamp/status", "method": "GET" }
            ] }),
        );
        assert_eq!(status, StatusCode::MULTI_STATUS);
        assert_eq!(body["success"], true);
        let r = body["data"].as_array().unwrap();
        assert_eq!(r[0], json!({ "success": true, "data": { "on": true } }));
        assert_eq!(
            (r[1]["success"].clone(), error_code(&r[1])),
            (json!(false), 122)
        );
        assert_eq!(r[2]["data"]["on"], "0");
        // Requests run in order, so the last one sees the change.
        assert_eq!(r[3]["data"]["on"], false);
    }

    #[test]
    fn config_lists_merge_items_and_keep_their_id() {
        let mock = lamp(Duration::from_secs(60));
        let token = login(&mock);
        let (status, body) = call(
            &mock,
            Method::PUT,
            "/api/lamp/config/lamp",
            Some(&token),
            json!({ "data": { "color": "blue", "id": "other" } }),
        );
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["data"],
            json!({ "id": "lamp", "on": "1", "color": "blue" })
        );

        // PUT on the list takes an array and matches items by id.
        let (_, body) = call(
            &mock,
            Method::PUT,
            "/api/lamp/config",
            Some(&token),
            json!({ "data": [{ "id": "lamp", "on": "0" }] }),
        );
        assert_eq!(
            body["data"],
            json!([{ "id": "lamp", "on": "0", "color": "blue" }])
        );
        let (status, body) = call(
            &mock,
            Method::PUT,
            "/api/lamp/config",
            Some(&token),
            json!({ "data": { "id": "lamp" } }),
        );
        assert_eq!((status, error_code(&body)), (StatusCode::BAD_REQUEST, 101));

        let (status, body) = call(
            &mock,
            Method::GET,
            "/api/lamp/config/missing",
            Some(&token),
            json!({}),
        );
        assert_eq!((status, error_code(&body)), (StatusCode::NOT_FOUND, 122));
        let (_, body) = call(
            &mock,
            Method::GET,
            "/api/lamp/config/",
            Some(&token),
            json!({}),
        );
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_change_runs_the_event_juggler_hook() {
        let mock = lamp(Duration::from_secs(60));
        mock.state
            .lock()
            .unwrap()
            .juggler
            .seed("http://panel/api/hook", &[("panel_lamp", "Lamp Events")]);
        let token = login(&mock);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        let body = json!({ "data": { "on": "0" } }).to_string();
        let (_, _, hooks) = mock.handle(
            &Method::PUT,
            "/api/lamp/config/lamp",
            &headers,
            body.as_bytes(),
        );
        assert_eq!(
            hooks,
            vec![Hook {
                url: "http://panel/api/hook".into(),
                post: true,
                body: "Lamp Events: lamp 0".into(),
                source: None
            }]
        );
        // Writing the same value again changes nothing, so nothing fires.
        let (_, _, hooks) = mock.handle(
            &Method::PUT,
            "/api/lamp/config/lamp",
            &headers,
            body.as_bytes(),
        );
        assert!(hooks.is_empty());
    }

    #[tokio::test]
    async fn http_replies_close_the_connection() {
        let mock = lamp(Duration::from_secs(60));
        let res = mock
            .router()
            .oneshot(
                Request::post("/api/login")
                    .body(Body::from(r#"{"username":"admin","password":"admin01"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONNECTION], "close");
        assert_eq!(res.headers()[header::CONTENT_TYPE], "application/json");

        let res = mock
            .router()
            .oneshot(
                Request::post("/api/login")
                    .body(Body::from("not json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn item_ids_are_percent_decoded() {
        assert_eq!(
            item_id("/a/config/x%20y", "/a/config").as_deref(),
            Some("x y")
        );
        assert_eq!(item_id("/a/config/x/y", "/a/config"), None);
        assert_eq!(item_id("/a/configx", "/a/config"), None);
    }
}
