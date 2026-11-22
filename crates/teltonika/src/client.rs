//! Minimal client for the Teltonika Networks Web API (RutOS / TSWOS).
//! <https://developers.teltonika-networks.com/>

use std::collections::HashMap;
use std::time::{Duration, Instant};

use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

/// Deadline for one HTTP request to a device, including connect.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The device did not answer, or sent something that is not the API's JSON.
    #[error("{0}")]
    Transport(String),
    /// The device answered with `success: false`.
    #[error("{0}")]
    Api(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// One API reply: `{success, data, errors}`.
#[derive(Debug, Clone, Deserialize)]
pub struct Envelope {
    pub success: bool,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub errors: Vec<ApiErrorItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorItem {
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub source: String,
}

impl Envelope {
    fn message(&self) -> String {
        let text: Vec<&str> = self.errors.iter().map(|e| e.error.as_str()).collect();
        if text.is_empty() {
            "unknown error".into()
        } else {
            text.join("; ")
        }
    }

    // A rejected token fails with source "Authorization" (121 missing, 123 expired).
    fn auth_failed(&self) -> bool {
        self.errors.iter().any(|e| e.source == "Authorization")
    }
}

/// Results of one poll, keyed by endpoint path.
pub struct PollResults {
    items: HashMap<String, Envelope>,
}

impl PollResults {
    /// The endpoint's data, or its API error.
    pub fn get(&self, path: &str) -> Result<&Value> {
        match self.items.get(path) {
            Some(e) if e.success => Ok(&e.data),
            Some(e) => Err(Error::Api(format!("{path}: {}", e.message()))),
            None => Err(Error::Api(format!("{path}: not requested"))),
        }
    }

    /// The endpoint's data, or `None` when it failed, e.g. because an optional package is missing.
    pub fn optional(&self, path: &str) -> Option<&Value> {
        self.items.get(path).filter(|e| e.success).map(|e| &e.data)
    }
}

struct Session {
    token: Option<String>,
    // Last successful result per path, for endpoints polled with a max age.
    cache: HashMap<String, (Instant, Envelope)>,
}

/// A connection to one device. The device's web server stalls on parallel requests, so the
/// client sends one request at a time; concurrent callers wait their turn and share one login.
pub struct Client {
    /// Address as configured: "192.168.30.1", or "name:port" for a device mock.
    pub host: String,
    /// Host name or IP address without the port.
    pub hostname: String,
    base: String,
    username: String,
    password: String,
    http: reqwest::Client,
    session: Mutex<Session>,
}

impl Client {
    /// `host` is an address such as "192.168.30.1" or "localhost:8441", reached over HTTPS.
    /// A full URL such as "http://127.0.0.1:9000" is used as given.
    pub fn new(host: &str, username: &str, password: &str) -> Result<Self> {
        let origin = if host.contains("://") {
            host.trim_end_matches('/').to_string()
        } else {
            format!("https://{host}")
        };
        let url = reqwest::Url::parse(&origin)
            .map_err(|e| Error::Transport(format!("bad host {host}: {e}")))?;
        let hostname = url
            .host_str()
            .unwrap_or(host)
            .trim_matches(['[', ']'])
            .to_string();
        let http = reqwest::Client::builder()
            // The devices ship self-signed certificates.
            .danger_accept_invalid_certs(true)
            .timeout(REQUEST_TIMEOUT)
            // The devices close every connection, so keep-alive only costs a failed reuse.
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(Self {
            host: host.to_string(),
            hostname,
            base: format!("{origin}/api"),
            username: username.to_string(),
            password: password.to_string(),
            http,
            session: Mutex::new(Session {
                token: None,
                cache: HashMap::new(),
            }),
        })
    }

    /// Fetches endpoints in one `POST /api/bulk` request. Each path has the maximum age of a
    /// cached result; zero always fetches. The device runs bulk sub-requests one after another,
    /// so a poll costs it one connection and one auth check.
    pub async fn poll(&self, paths: &[(&str, Duration)]) -> Result<PollResults> {
        let mut s = self.session.lock().await;
        let now = Instant::now();
        let mut items = HashMap::new();
        let mut due = Vec::new();
        for &(path, max_age) in paths {
            match s.cache.get(path) {
                Some((at, item)) if now.duration_since(*at) < max_age => {
                    items.insert(path.to_string(), item.clone());
                }
                _ => due.push((path, max_age)),
            }
        }
        if !due.is_empty() {
            let body = json!({ "data": due.iter().map(|(p, _)| json!({ "endpoint": format!("/api{p}"), "method": "GET" })).collect::<Vec<_>>() });
            let reply = self
                .authed(&mut s, Method::POST, "/bulk", Some(body))
                .await?;
            if !reply.success || !reply.data.is_array() {
                return Err(Error::Api(format!("bulk: {}", reply.message())));
            }
            let Value::Array(results) = reply.data else {
                unreachable!()
            };
            for (i, (path, max_age)) in due.into_iter().enumerate() {
                let item = results
                    .get(i)
                    .and_then(|v| serde_json::from_value::<Envelope>(v.clone()).ok())
                    .unwrap_or(Envelope {
                        success: false,
                        data: Value::Null,
                        errors: vec![missing()],
                    });
                if item.success && !max_age.is_zero() {
                    s.cache.insert(path.to_string(), (now, item.clone()));
                }
                items.insert(path.to_string(), item);
            }
        }
        Ok(PollResults { items })
    }

    /// Sends one request and returns its data. Requests other than GET change device state,
    /// so they also drop cached results.
    pub async fn call(&self, method: Method, path: &str, data: Option<Value>) -> Result<Value> {
        let mut s = self.session.lock().await;
        let body = data.map(|d| json!({ "data": d }));
        let reply = self.authed(&mut s, method.clone(), path, body).await?;
        if method != Method::GET {
            s.cache.clear();
        }
        if !reply.success {
            return Err(Error::Api(format!("{method} {path}: {}", reply.message())));
        }
        Ok(reply.data)
    }

    // Sends a request with the session token, logging in again once when the device rejects it.
    // A rejected token in a bulk request still returns HTTP 207, with each item failing.
    async fn authed(
        &self,
        s: &mut Session,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Envelope> {
        for attempt in 0..2 {
            let token = match &s.token {
                Some(t) => t.clone(),
                None => self.login(s).await?,
            };
            let (status, reply) = self
                .send(method.clone(), path, body.as_ref(), Some(&token))
                .await?;
            let items_rejected = matches!(&reply.data, Value::Array(items) if items.iter().any(|i| {
                serde_json::from_value::<Envelope>(i.clone()).is_ok_and(|e| e.auth_failed())
            }));
            let expired =
                status == StatusCode::UNAUTHORIZED || reply.auth_failed() || items_rejected;
            if expired && attempt == 0 {
                s.token = None;
                continue;
            }
            return Ok(reply);
        }
        unreachable!("the second attempt always returns")
    }

    async fn login(&self, s: &mut Session) -> Result<String> {
        let creds = json!({ "username": self.username, "password": self.password });
        let (_, reply) = self
            .send(Method::POST, "/login", Some(&creds), None)
            .await?;
        if !reply.success {
            return Err(Error::Api(format!("login failed: {}", reply.message())));
        }
        let token = reply
            .data
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Api("login: no token".into()))?;
        s.token = Some(token.to_string());
        Ok(token.to_string())
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        token: Option<&str>,
    ) -> Result<(StatusCode, Envelope)> {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = req
            .send()
            .await
            .map_err(|e| Error::Transport(format!("{path}: {}", transport_message(&e))))?;
        let status = res.status();
        let text = res
            .text()
            .await
            .map_err(|e| Error::Transport(format!("{path}: {}", transport_message(&e))))?;
        let reply = serde_json::from_str(&text).map_err(|_| {
            Error::Transport(format!(
                "{path}: HTTP {}, non-JSON response",
                status.as_u16()
            ))
        })?;
        Ok((status, reply))
    }
}

fn missing() -> ApiErrorItem {
    ApiErrorItem {
        code: 0,
        error: "missing in bulk response".into(),
        source: "Request".into(),
    }
}

// reqwest's top-level message ("error sending request for url ...") hides the cause.
fn transport_message(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "timed out".into();
    }
    let mut cause: &dyn std::error::Error = e;
    while let Some(next) = cause.source() {
        cause = next;
    }
    cause.to_string()
}
