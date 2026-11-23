//! The panel's HTTP server and the state behind it.
//!
//! Each device has a background poller. Pollers run only while someone uses the panel: an
//! open event stream, or a status request in the last minute. The server owns all panel
//! state (pending changes, locked ports, the last control error, device events), so the
//! browser only renders what `/api/status` and `/api/events` send.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use teltonika::control;
use teltonika::model::{DeviceBase, DeviceLink, RouterStatus, SwitchStatus, Vlan};
use tokio::sync::{Notify, RwLock, broadcast, watch};
use tokio_stream::wrappers::BroadcastStream;
use ts_rs::TS;

use crate::{Attached, Device, Devices};

/// Poll interval while a device answers.
const POLL: Duration = Duration::from_secs(3);
/// Retry interval for a device that does not answer. A connection to an absent host can
/// hang for many seconds, so it is not retried on every refresh.
const OFFLINE_RETRY: Duration = Duration::from_secs(30);
/// Pollers stop this long after the last status request when no event stream is open.
const IDLE_AFTER: Duration = Duration::from_secs(60);
/// A requested change shows as pending until the device reports it, or for at most this long.
const PENDING_FOR: Duration = Duration::from_secs(30);
const MAX_EVENTS: usize = 20;

/// What a control changes. Ports are keyed by device label and API port ID.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Target {
    Cellular,
    Wifi,
    Tailscale,
    Port(String, String),
}

impl Target {
    fn name(&self) -> String {
        match self {
            Target::Cellular => "Cellular".into(),
            Target::Wifi => "Wi-Fi".into(),
            Target::Tailscale => "Tailscale".into(),
            Target::Port(device, id) => format!("{device} {id}"),
        }
    }
}

struct Poller<T> {
    device: Device,
    status: RwLock<T>,
    /// When the device last finished a poll; `None` before the first one.
    polled_at: std::sync::Mutex<Option<time::OffsetDateTime>>,
    wake: Notify,
}

struct AppState {
    router: Poller<RouterStatus>,
    switches: Vec<Poller<SwitchStatus>>,
    own_macs: HashSet<String>,
    attached: Vec<Attached>,
    allow_remote_control: bool,
    pending: std::sync::Mutex<HashMap<Target, (bool, Instant)>>,
    notice: std::sync::Mutex<Option<String>>,
    events: std::sync::Mutex<Vec<DeviceEvent>>,
    viewers: AtomicUsize,
    last_request: std::sync::Mutex<Instant>,
    demand: Notify,
    changed: broadcast::Sender<()>,
    /// Becomes true on shutdown, which ends the open event streams.
    stopping: watch::Receiver<bool>,
}

type Shared = Arc<AppState>;

pub async fn run(
    devices: Devices,
    addr: SocketAddr,
    allow_remote_control: bool,
) -> std::io::Result<()> {
    let connecting = "Connecting…";
    let (stop, stopping) = watch::channel(false);
    let state = Arc::new(AppState {
        router: Poller {
            status: RwLock::new(RouterStatus::offline(
                &devices.router.label,
                &devices.router.client.host,
                connecting,
            )),
            device: devices.router,
            polled_at: Default::default(),
            wake: Notify::new(),
        },
        switches: devices
            .switches
            .into_iter()
            .map(|d| Poller {
                status: RwLock::new(SwitchStatus {
                    base: DeviceBase::offline(&d.label, &d.client.host, connecting),
                }),
                device: d,
                polled_at: Default::default(),
                wake: Notify::new(),
            })
            .collect(),
        own_macs: own_macs(),
        attached: devices.attached,
        allow_remote_control,
        pending: Default::default(),
        notice: Default::default(),
        events: Default::default(),
        viewers: AtomicUsize::new(0),
        last_request: std::sync::Mutex::new(Instant::now()),
        demand: Notify::new(),
        changed: broadcast::channel(16).0,
        stopping,
    });

    tokio::spawn(poll_router(state.clone()));
    for i in 0..state.switches.len() {
        tokio::spawn(poll_switch(state.clone(), i));
    }

    let app = Router::new()
        .route(
            "/",
            get(|| {
                asset(
                    include_str!("../../../web/index.html"),
                    "text/html; charset=utf-8",
                )
            }),
        )
        .route(
            "/styles.css",
            get(|| {
                asset(
                    include_str!("../../../web/styles.css"),
                    "text/css; charset=utf-8",
                )
            }),
        )
        .route(
            "/app.js",
            get(|| {
                asset(
                    include_str!(concat!(env!("OUT_DIR"), "/app.js")),
                    "text/javascript; charset=utf-8",
                )
            }),
        )
        .route("/api/status", get(status))
        .route("/api/events", get(events))
        // Event texts are short; the old server capped hook bodies at 8 KB too.
        .route("/api/hook", post(hook).layer(DefaultBodyLimit::max(8192)))
        .route("/api/toggle", post(toggle))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Teltonika panel on http://{addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    // Graceful shutdown waits for open connections, so the event streams must end first.
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        let _ = stop.send(true);
    })
    .await
}

// Ctrl-C, or SIGTERM from `docker stop` and service managers.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.ok();
}

async fn asset(body: &'static str, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

// ---- Polling ----

impl AppState {
    fn demanded(&self) -> bool {
        self.viewers.load(Ordering::Relaxed) > 0
            || self.last_request.lock().unwrap().elapsed() < IDLE_AFTER
    }

    async fn wait_for_demand(&self) {
        while !self.demanded() {
            tokio::select! {
                _ = self.demand.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }

    fn mark_demand(&self) {
        *self.last_request.lock().unwrap() = Instant::now();
        self.demand.notify_waiters();
    }

    fn notify_changed(&self) {
        let _ = self.changed.send(());
    }

    /// Polls every answering device now, e.g. after an event or a config change.
    fn refresh_all(&self) {
        self.router.wake.notify_one();
        self.switches.iter().for_each(|s| s.wake.notify_one());
    }
}

// Waits until the next poll is due: POLL for an answering device, which a refresh cuts short,
// or OFFLINE_RETRY for one that does not answer.
async fn pause(wake: &Notify, online: bool) {
    if online {
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(POLL) => {}
        }
    } else {
        tokio::time::sleep(OFFLINE_RETRY).await;
    }
}

async fn poll_router(state: Shared) {
    let p = &state.router;
    loop {
        state.wait_for_demand().await;
        let st = teltonika::router_status(&p.device.client, &p.device.label).await;
        let online = st.base.online;
        *p.status.write().await = st;
        *p.polled_at.lock().unwrap() = Some(time::OffsetDateTime::now_utc());
        state.notify_changed();
        pause(&p.wake, online).await;
    }
}

async fn poll_switch(state: Shared, i: usize) {
    let p = &state.switches[i];
    loop {
        state.wait_for_demand().await;
        let st = teltonika::switch_status(&p.device.client, &p.device.label).await;
        let online = st.base.online;
        *p.status.write().await = st;
        *p.polled_at.lock().unwrap() = Some(time::OffsetDateTime::now_utc());
        state.notify_changed();
        pause(&p.wake, online).await;
    }
}

// ---- Status ----

/// Configured on/off state of a target in the latest status, if the device has it.
fn actual(target: &Target, r: &RouterStatus, switches: &[SwitchStatus]) -> Option<bool> {
    match target {
        Target::Cellular => r
            .cellular
            .first()
            .filter(|c| c.interface_id.is_some())
            .map(|c| c.data_enabled),
        Target::Wifi => r.wifi_control.as_ref().map(|w| w.enabled),
        Target::Tailscale => r.tailscale.as_ref().map(|t| t.enabled),
        Target::Port(device, id) => std::iter::once(&r.base)
            .chain(switches.iter().map(|s| &s.base))
            .find(|d| &d.label == device)
            .and_then(|d| d.ports.iter().find(|p| &p.id == id))
            .map(|p| p.enabled),
    }
}

impl AppState {
    async fn snapshot(&self) -> (RouterStatus, Vec<SwitchStatus>) {
        let r = self.router.status.read().await.clone();
        let mut sw = Vec::new();
        for s in &self.switches {
            sw.push(s.status.read().await.clone());
        }
        (r, sw)
    }

    /// The full panel status, with the server's panel state added.
    async fn panel_status(&self) -> PanelStatus {
        let (r, sw) = self.snapshot().await;
        let links = teltonika::find_links(
            &std::iter::once(&r.base)
                .chain(sw.iter().map(|s| &s.base))
                .collect::<Vec<_>>(),
        );

        let pending: HashMap<Target, bool> = {
            let mut map = self.pending.lock().unwrap();
            map.retain(|t, (want, at)| {
                at.elapsed() < PENDING_FOR && actual(t, &r, &sw) != Some(*want)
            });
            map.iter()
                .map(|(t, (want, _))| (t.clone(), *want))
                .collect()
        };
        let control = |t: Target| {
            actual(&t, &r, &sw).map(|on| Control {
                on: pending.get(&t).copied().unwrap_or(on),
                pending: pending.contains_key(&t),
            })
        };
        let controls = Controls {
            cellular: control(Target::Cellular),
            wifi: control(Target::Wifi),
            tailscale: control(Target::Tailscale),
        };

        let mut ports = BTreeMap::new();
        for d in std::iter::once(&r.base).chain(sw.iter().map(|s| &s.base)) {
            for p in &d.ports {
                let panel_path = p.macs.iter().any(|m| self.own_macs.contains(m));
                ports.insert(
                    port_key(&d.label, &p.id),
                    PortControl {
                        locked: panel_path && p.enabled,
                        pending: pending
                            .get(&Target::Port(d.label.clone(), p.id.clone()))
                            .copied(),
                    },
                );
            }
        }
        // The time of the newest poll, so data kept from before an idle period does not look
        // fresh. Before the first poll, now.
        let fetched_at = std::iter::once(&self.router.polled_at)
            .chain(self.switches.iter().map(|s| &s.polled_at))
            .filter_map(|t| *t.lock().unwrap())
            .max()
            .unwrap_or_else(time::OffsetDateTime::now_utc);
        let attached = self
            .attached
            .iter()
            .map(|a| {
                let port = r
                    .base
                    .ports
                    .iter()
                    .find(|p| p.label.eq_ignore_ascii_case(&a.port));
                AttachedDevice {
                    name: a.name.clone(),
                    port: a.port.clone(),
                    ip: a.ip.clone(),
                    link_up: port.is_some_and(|p| p.link_up),
                    speed_mbps: port.map_or(0.0, |p| p.speed_mbps),
                    mac: port.and_then(|p| p.macs.first().cloned()),
                    vlan: port.map(|p| p.vlan.clone()).unwrap_or_default(),
                }
            })
            .collect();
        PanelStatus {
            fetched_at: rfc3339(fetched_at),
            attached,
            router: r,
            switches: sw,
            links,
            events: self.events.lock().unwrap().clone(),
            controls,
            ports,
            notice: self.notice.lock().unwrap().clone(),
        }
    }
}

/// Key of a port in `PanelStatus.ports`: device label and API port ID, e.g. "TSW202 #1/port3".
fn port_key(device: &str, port_id: &str) -> String {
    format!("{device}/{port_id}")
}

/// Everything the browser draws, sent by `/api/status` and `/api/events`.
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PanelStatus {
    /// Time of the newest device poll, RFC 3339.
    fetched_at: String,
    router: RouterStatus,
    switches: Vec<SwitchStatus>,
    links: Vec<DeviceLink>,
    /// Configured devices on router ports, with the port's live state.
    attached: Vec<AttachedDevice>,
    /// Event Juggler notifications, newest first.
    events: Vec<DeviceEvent>,
    controls: Controls,
    /// Panel state of every port, keyed "device/portId".
    ports: BTreeMap<String, PortControl>,
    /// The last control error, until the next change succeeds.
    notice: Option<String>,
}

/// A configured device on a router port (`--rutm50-attached`).
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AttachedDevice {
    name: String,
    /// Router port label, e.g. "LAN4".
    port: String,
    /// Configured address, if any.
    ip: Option<String>,
    link_up: bool,
    speed_mbps: f64,
    /// First MAC address the router learned on the port.
    mac: Option<String>,
    vlan: Vlan,
}

/// On/off state of each router feature; `null` when the router does not have it.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct Controls {
    cellular: Option<Control>,
    wifi: Option<Control>,
    tailscale: Option<Control>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct Control {
    /// The requested state while a change is pending, else the configured state.
    on: bool,
    pending: bool,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct PortControl {
    /// The port carries the panel's own connection, so the panel keeps it on.
    locked: bool,
    /// The requested state while the device applies a change.
    pending: Option<bool>,
}

/// An Event Juggler notification received at /api/hook.
#[derive(Clone, Serialize, TS)]
#[ts(export)]
pub struct DeviceEvent {
    /// Arrival time, RFC 3339.
    at: String,
    /// Label of the device that sent it, e.g. "RUTM50".
    device: String,
    text: String,
}

fn rfc3339(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn now() -> String {
    rfc3339(time::OffsetDateTime::now_utc())
}

async fn status(State(state): State<Shared>) -> Response {
    state.mark_demand();
    no_store(axum::Json(state.panel_status().await))
}

fn no_store(r: impl IntoResponse) -> Response {
    let mut res = r.into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    res
}

// Counts an open event stream as a viewer until the stream is dropped.
struct Viewer(Shared);
impl Drop for Viewer {
    fn drop(&mut self) {
        self.0.viewers.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Server-Sent Events: the full status, sent now and whenever it changes.
async fn events(State(state): State<Shared>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    state.viewers.fetch_add(1, Ordering::Relaxed);
    state.mark_demand();
    let viewer = Viewer(state.clone());
    let mut stopping = state.stopping.clone();
    let stopped = async move {
        let _ = stopping.wait_for(|s| *s).await;
    };
    let changes = BroadcastStream::new(state.changed.subscribe()).map(|_| ());
    let stream = futures_util::stream::once(async {})
        .chain(changes)
        .then(move |_| {
            let state = viewer.0.clone();
            async move {
                Ok(Event::default()
                    .data(serde_json::to_string(&state.panel_status().await).unwrap_or_default()))
            }
        })
        .take_until(stopped);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(25)))
}

// ---- Device events ----

/// Event Juggler HTTP action. Only the configured devices may call it; others get 403.
/// Device hosts can be names (the mock stack uses "rutm50"), so they are resolved per hook.
async fn hook(
    State(state): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Response {
    let ip = peer.ip().to_canonical();
    let mut sender = None;
    for d in std::iter::once(&state.router.device).chain(state.switches.iter().map(|s| &s.device)) {
        if resolves_to(&d.client.hostname, ip).await {
            sender = Some(d.label.clone());
            break;
        }
    }
    let Some(device) = sender else {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({ "error": "hooks are accepted only from the configured devices" })),
        )
            .into_response();
    };
    let text: String = String::from_utf8_lossy(&body)
        .trim()
        .chars()
        .take(500)
        .collect();
    {
        let mut events = state.events.lock().unwrap();
        events.insert(
            0,
            DeviceEvent {
                at: now(),
                device,
                text,
            },
        );
        events.truncate(MAX_EVENTS);
    }
    state.refresh_all();
    state.notify_changed();
    axum::Json(json!({ "ok": true })).into_response()
}

async fn resolves_to(hostname: &str, ip: IpAddr) -> bool {
    if let Ok(literal) = hostname.parse::<IpAddr>() {
        return literal.to_canonical() == ip;
    }
    match tokio::net::lookup_host((hostname, 0)).await {
        Ok(mut addrs) => addrs.any(|a| a.ip().to_canonical() == ip),
        Err(_) => false,
    }
}

// ---- Control ----

#[derive(Deserialize)]
struct ToggleBody {
    target: Option<String>,
    enabled: Option<bool>,
    device: Option<String>,
    port: Option<String>,
}

fn error(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, axum::Json(json!({ "error": msg.into() }))).into_response()
}

/// Turns a router feature or a port on or off. Only loopback callers (this computer and the
/// Amp portal proxy) may change device settings, unless --allow-remote-control is set.
/// Requiring a JSON body also blocks cross-site form posts.
async fn toggle(
    State(state): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // A change needs fresh data to decide on and to show its result, so wake idle pollers.
    state.mark_demand();
    if !state.allow_remote_control && !peer.ip().to_canonical().is_loopback() {
        return error(
            StatusCode::FORBIDDEN,
            "device control is allowed only through the panel",
        );
    }
    let json_body = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !json_body {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/json",
        );
    }
    let usage = r#"body must be {"target": "cellular"|"wifi"|"tailscale"|"port", "enabled": true|false}, with "device" and "port" for a port"#;
    let Ok(b) = serde_json::from_slice::<ToggleBody>(&body) else {
        return error(StatusCode::BAD_REQUEST, usage);
    };
    let Some(enabled) = b.enabled else {
        return error(StatusCode::BAD_REQUEST, usage);
    };

    let (r, sw) = state.snapshot().await;
    // Resolve the request against the latest status: which client, and what to call.
    let (target, client, poller_wake): (Target, Arc<teltonika::Client>, &Notify) =
        match b.target.as_deref() {
            Some("cellular") => (
                Target::Cellular,
                state.router.device.client.clone(),
                &state.router.wake,
            ),
            Some("wifi") => (
                Target::Wifi,
                state.router.device.client.clone(),
                &state.router.wake,
            ),
            Some("tailscale") => (
                Target::Tailscale,
                state.router.device.client.clone(),
                &state.router.wake,
            ),
            Some("port") => {
                let device = b.device.unwrap_or_default();
                let (base, client, wake) = if device == state.router.device.label {
                    (
                        &r.base,
                        state.router.device.client.clone(),
                        &state.router.wake,
                    )
                } else if let Some(i) = state.switches.iter().position(|s| s.device.label == device)
                {
                    (
                        &sw[i].base,
                        state.switches[i].device.client.clone(),
                        &state.switches[i].wake,
                    )
                } else {
                    return error(StatusCode::CONFLICT, "unknown device");
                };
                let Some(port) = base.ports.iter().find(|p| Some(&p.id) == b.port.as_ref()) else {
                    return error(StatusCode::CONFLICT, "unknown port");
                };
                if !enabled && port.macs.iter().any(|m| state.own_macs.contains(m)) {
                    let name = if port.label.chars().all(|c| c.is_ascii_digit()) {
                        format!("Port {}", port.label)
                    } else {
                        port.label.clone()
                    };
                    return error(
                        StatusCode::CONFLICT,
                        format!("{name} carries the panel's own connection to {device}"),
                    );
                }
                (Target::Port(device, port.id.clone()), client, wake)
            }
            _ => return error(StatusCode::BAD_REQUEST, usage),
        };

    // Check that the device has the feature before showing the change as pending.
    let problem = match &target {
        Target::Cellular
            if r.cellular
                .first()
                .and_then(|c| c.interface_id.as_ref())
                .is_none() =>
        {
            Some("no mobile interface found for the active SIM")
        }
        Target::Wifi if r.wifi_control.as_ref().is_none_or(|w| w.radios.is_empty()) => {
            Some("no Wi-Fi radios found")
        }
        Target::Tailscale if r.tailscale.is_none() => Some("Tailscale is not installed"),
        _ => None,
    };
    if let Some(p) = problem {
        return error(StatusCode::CONFLICT, p);
    }
    state.begin(&target, enabled);
    let result = match &target {
        Target::Cellular => {
            let iface = r.cellular[0].interface_id.as_deref().unwrap_or_default();
            control::set_mobile_interface(&client, iface, enabled).await
        }
        Target::Wifi => {
            control::set_wifi_radios(&client, &r.wifi_control.as_ref().unwrap().radios, enabled)
                .await
        }
        Target::Tailscale => control::set_tailscale(&client, enabled).await,
        Target::Port(_, id) => control::set_port(&client, id, enabled).await,
    };

    match result {
        Ok(()) => {
            poller_wake.notify_one();
            state.notify_changed();
            axum::Json(json!({ "ok": true, "target": b.target, "enabled": enabled }))
                .into_response()
        }
        Err(e) => {
            state.pending.lock().unwrap().remove(&target);
            let msg = format!("Could not change {}: {e}", target.name());
            *state.notice.lock().unwrap() = Some(msg.clone());
            state.notify_changed();
            error(StatusCode::BAD_GATEWAY, msg)
        }
    }
}

impl AppState {
    // Shows the change as pending and clears the previous error, before the device applies it.
    fn begin(&self, target: &Target, enabled: bool) {
        self.pending
            .lock()
            .unwrap()
            .insert(target.clone(), (enabled, Instant::now()));
        *self.notice.lock().unwrap() = None;
        self.notify_changed();
    }
}

// ---- Own MAC addresses ----

/// This computer's MAC addresses, upper case. A port that has learned one of them carries the
/// panel's own traffic. Linux lists them in /sys/class/net; macOS needs ifconfig.
fn own_macs() -> HashSet<String> {
    let mut text = String::new();
    if let Ok(dir) = std::fs::read_dir("/sys/class/net") {
        for e in dir.flatten() {
            if let Ok(mac) = std::fs::read_to_string(e.path().join("address")) {
                text.push_str(&format!("ether {}\n", mac.trim()));
            }
        }
    }
    if let Ok(out) = std::process::Command::new("ifconfig").output() {
        text.push_str(&String::from_utf8_lossy(&out.stdout));
    }
    parse_ether(&text)
}

fn parse_ether(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .filter(|w| w[0] == "ether")
        .map(|w| w[1].to_uppercase())
        .filter(|m| m.len() == 17 && m != "00:00:00:00:00:00" && m != "02:00:00:00:00:00")
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ifconfig_and_skips_placeholder_macs() {
        let text = "en16: flags=8863\n\tether ec:1a:c3:00:00:20\nlo0: ether 00:00:00:00:00:00\nen0: ether 02:00:00:00:00:00";
        assert_eq!(
            parse_ether(text),
            HashSet::from(["EC:1A:C3:00:00:20".to_string()])
        );
    }
}
