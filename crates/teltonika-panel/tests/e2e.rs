//! End-to-end tests of the panel against the device mocks.
//!
//! By default the tests start the mock network in this process (a RUTM50 and two TSW202
//! mocks, wired like compose.mock.yaml) and run the panel binary against it. Set PANEL_URL to
//! test a panel that already runs instead, such as the Docker mock stack:
//! `PANEL_URL=http://localhost:8080 cargo test -p teltonika-panel --test e2e`.
//!
//! The mocks keep their state, so the tests run one at a time and restore what they change.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teltonika::Client;
use teltonika_mock::{Auth, Juggler, Mock, rutm50, serve, tsw202};
use teltonika_panel::{Attached, Device, Devices, server};
use tokio::sync::{Mutex, MutexGuard};

const PASSWORD: &str = "mock-password";

struct Stack {
    /// Where the tests reach the panel.
    url: String,
    /// The same panel at an address that is not a device, for the hook check.
    outsider_url: String,
    /// True when each device has its own address, so the panel can tell which one sent a
    /// hook. In-process this needs extra loopback addresses: Linux has them, macOS does not.
    hooks_name_the_device: bool,
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Loopback addresses for the router and the two switches: 127.0.0.1 to 127.0.0.3 where the
/// OS has them, else 127.0.0.1 for all three.
fn device_ips() -> [Ipv4Addr; 3] {
    if TcpListener::bind("127.0.0.2:0").is_ok() {
        [1, 2, 3].map(|n| Ipv4Addr::new(127, 0, 0, n))
    } else {
        [Ipv4Addr::LOCALHOST; 3]
    }
}

/// Starts the mocks and the panel on a runtime of their own, which outlives each test's
/// runtime and ends with the test process.
fn start(panel_port: u16, ips: [Ipv4Addr; 3]) {
    let hook_url = &format!("http://127.0.0.1:{panel_port}/api/hook");
    let auth = Auth {
        password: PASSWORD.into(),
        ..Auth::default()
    };
    let juggler = |rules: &[(&str, &str)], ip: Ipv4Addr| {
        let mut j = Juggler::default();
        j.source = Some(IpAddr::V4(ip));
        j.seed(hook_url, rules);
        j
    };
    let ports_rules = [("panel_ports", "Switch Events")];
    let router = Mock::new(
        "RUTM50",
        rutm50::Rutm50::new(rutm50::Config {
            cables: "lan1=1000;lan2=1000;lan4=100;wan=1000".into(),
            vlans: "1=lan1:u,lan2:u,lan3:u,lan4:u;10=lan1:t;20=lan2:t".into(),
            neighbors: "lan1=20:97:27:00:00:02,DC:A6:32:00:00:10,EC:1A:C3:00:00:20,9C:05:D6:00:00:30;lan2=20:97:27:00:00:03,9C:05:D6:00:00:31,DC:A6:32:00:00:11;lan4=00:30:1A:00:00:50;wan=0C:EA:14:00:00:01".into(),
            ..rutm50::Config::default()
        }),
        auth.clone(),
        juggler(&[("panel_ports", "Switch Events"), ("panel_mobile", "Mobile Data")], ips[0]),
    );
    let it = Mock::new(
        "TSW202",
        tsw202::Tsw202::new(tsw202::Config {
            hostname: "TSW202-IT".into(),
            vlans: "1=port1:u;10=port1:t,*:u".into(),
            neighbors: "port1=20:97:27:00:00:01,A4:83:E7:00:00:40,F0:18:98:00:00:41,20:97:27:00:00:03;port3=DC:A6:32:00:00:10;port5=9C:05:D6:00:00:30;port6=EC:1A:C3:00:00:20".into(),
            ..tsw202::Config::default()
        }),
        auth.clone(),
        juggler(&ports_rules, ips[1]),
    );
    let ot = Mock::new(
        "TSW202",
        tsw202::Tsw202::new(tsw202::Config {
            mac: "20:97:27:00:00:03".into(),
            lan_ip: "192.168.30.3/24".into(),
            serial: "1100000003".into(),
            hostname: "TSW202-OT".into(),
            cables: "port1=1000;port2=1000/6500;port4=100".into(),
            vlans: "1=port1:u;20=port1:t,*:u".into(),
            neighbors: "port1=20:97:27:00:00:01,20:97:27:00:00:02;port2=9C:05:D6:00:00:31;port4=DC:A6:32:00:00:11".into(),
        }),
        auth,
        juggler(&ports_rules, ips[2]),
    );
    let [l1, l2, l3] = ips.map(|ip| TcpListener::bind((ip, 0)).unwrap());
    let device = |label: &str, l: &TcpListener| Device {
        label: label.into(),
        client: Arc::new(
            Client::new(&l.local_addr().unwrap().to_string(), "admin", PASSWORD).unwrap(),
        ),
    };
    let devices = Devices {
        router: device("RUTM50", &l1),
        switches: vec![device("TSW202 #1", &l2), device("TSW202 #2", &l3)],
        attached: vec![
            "LAN4=Packet radio@192.168.30.50"
                .parse::<Attached>()
                .unwrap(),
        ],
    };
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            tokio::spawn(serve(router, l1));
            tokio::spawn(serve(it, l2));
            tokio::spawn(serve(ot, l3));
            // The panel listens on IPv4 and IPv6. The mocks reach it over 127.0.0.1, their
            // device address; requests from ::1 come from no device.
            server::run(devices, SocketAddr::from(([0u16; 8], panel_port)), false)
                .await
                .unwrap();
        });
    });
}

fn stack() -> &'static Stack {
    static STACK: OnceLock<Stack> = OnceLock::new();
    STACK.get_or_init(|| {
        if let Ok(url) = std::env::var("PANEL_URL") {
            // From the host, a container stack sees a Docker gateway address, not a device.
            return Stack {
                outsider_url: url.clone(),
                url,
                hooks_name_the_device: true,
            };
        }
        let port = free_port();
        let ips = device_ips();
        start(port, ips);
        Stack {
            url: format!("http://127.0.0.1:{port}"),
            outsider_url: format!("http://[::1]:{port}"),
            hooks_name_the_device: ips[0] != ips[1],
        }
    })
}

/// Runs the tests one at a time, from the same baseline: everything on and all mocks online.
async fn setup() -> (MutexGuard<'static, ()>, &'static str) {
    // A tokio Mutex works across the tests' runtimes and is not poisoned by a failed test.
    static LOCK: Mutex<()> = Mutex::const_new(());
    let guard = LOCK.lock().await;
    let url = stack().url.as_str();
    until(url, all_online, "all mocks online", 60).await;
    for target in ["cellular", "wifi", "tailscale"] {
        toggle(url, json!({ "target": target, "enabled": true })).await;
    }
    toggle(
        url,
        json!({ "target": "port", "device": "TSW202 #1", "port": "port3", "enabled": true }),
    )
    .await;
    toggle(
        url,
        json!({ "target": "port", "device": "TSW202 #2", "port": "port4", "enabled": true }),
    )
    .await;
    until(
        url,
        |s| {
            s["router"]["tailscale"]["running"] == true
                && s["router"]["cellular"][0]["dataConnected"] == true
                && port(s, 0, "port3")["linkUp"] == true
        },
        "baseline state",
        20,
    )
    .await;
    (guard, url)
}

fn all_online(s: &Value) -> bool {
    s["router"]["online"] == true
        && s["switches"]
            .as_array()
            .is_some_and(|a| a.iter().all(|w| w["online"] == true))
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

async fn status(url: &str) -> Value {
    let res = http()
        .get(format!("{url}/api/status"))
        .send()
        .await
        .expect("panel answers");
    assert_eq!(res.status(), 200);
    res.json().await.unwrap()
}

async fn toggle(url: &str, body: Value) -> (u16, Value) {
    let res = http()
        .post(format!("{url}/api/toggle"))
        .json(&body)
        .send()
        .await
        .unwrap();
    (
        res.status().as_u16(),
        res.json().await.unwrap_or(Value::Null),
    )
}

/// Polls until `check` passes, because device changes take a poll or two to show.
async fn until(url: &str, check: impl Fn(&Value) -> bool, what: &str, secs: u64) -> Value {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        // The panel may still be starting; treat a refused connection like a failed check.
        if let Ok(res) = http().get(format!("{url}/api/status")).send().await
            && let Ok(s) = res.json::<Value>().await
            && check(&s)
        {
            return s;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn port<'a>(s: &'a Value, switch: usize, id: &str) -> &'a Value {
    s["switches"][switch]["ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .unwrap()
}

fn router_port<'a>(s: &'a Value, label: &str) -> &'a Value {
    s["router"]["ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["label"] == label)
        .unwrap()
}

/// True when an event with `text` arrived from `device`. Without a separate address per
/// device, the panel cannot tell the devices apart, so only the text is checked.
fn has_event(s: &Value, device: &str, text: &str) -> bool {
    let any_device = !stack().hooks_name_the_device;
    s["events"].as_array().unwrap().iter().any(|e| {
        (any_device || e["device"] == device) && e["text"].as_str().unwrap_or("").contains(text)
    })
}

#[tokio::test]
async fn serves_the_page_and_the_client_script() {
    let (_g, url) = setup().await;
    let page = http().get(format!("{url}/")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(
        page.text()
            .await
            .unwrap()
            .contains(r#"<script type="module" src="/app.js">"#)
    );
    let script = http().get(format!("{url}/app.js")).send().await.unwrap();
    assert_eq!(script.status(), 200);
    assert!(
        script.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("javascript")
    );
    let js = script.text().await.unwrap();
    assert!(js.contains(r#"new EventSource("/api/events")"#));
    // The build stripped the TypeScript: no type imports or annotations reach the browser.
    assert!(!js.contains("import type") && !js.contains(": PanelStatus"));
}

#[tokio::test]
async fn reads_every_device_and_finds_the_cables_between_them() {
    let (_g, url) = setup().await;
    let s = status(url).await;
    let r = &s["router"];
    assert_eq!(r["system"]["model"], "RUTM50");
    assert_eq!(r["lanIpv4"], json!(["192.168.30.1/24"]));
    assert_eq!(r["cellular"][0]["operator"], "Mock Mobile");
    let bands: Vec<&Value> = r["wifi"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| &w["band"])
        .collect();
    assert_eq!(
        bands,
        [&json!("2.4GHz"), &json!("5GHz"), &json!("5GHz/2.4GHz")]
    );
    assert_eq!(r["tailscale"]["running"], true);

    // The router trunks VLAN 10 (IT) to switch #1 and VLAN 20 (OT) to switch #2.
    assert_eq!(
        router_port(&s, "LAN1")["vlan"],
        json!({ "untagged": 1, "tagged": [10] })
    );
    assert_eq!(
        router_port(&s, "LAN2")["vlan"],
        json!({ "untagged": 1, "tagged": [20] })
    );

    assert_eq!(s["switches"][0]["ports"].as_array().unwrap().len(), 10);
    assert_eq!(port(&s, 0, "port3")["speedMbps"], 100.0);
    assert_eq!(
        port(&s, 0, "port3")["vlan"],
        json!({ "untagged": 10, "tagged": [] })
    );
    assert_eq!(
        port(&s, 0, "port5")["poe"],
        json!({ "enabled": true, "active": true, "watts": 4.2 })
    );
    assert_eq!(s["switches"][1]["lanIpv4"], json!(["192.168.30.3/24"]));
    assert_eq!(s["switches"][1]["system"]["hostname"], "TSW202-OT");
    assert_eq!(
        port(&s, 1, "port2")["poe"],
        json!({ "enabled": true, "active": true, "watts": 6.5 })
    );
    assert_eq!(
        port(&s, 1, "port2")["vlan"],
        json!({ "untagged": 20, "tagged": [] })
    );

    // Panel state the server adds for the browser.
    let on = json!({ "on": true, "pending": false });
    assert_eq!(
        s["controls"],
        json!({ "cellular": on, "wifi": on, "tailscale": on })
    );
    assert_eq!(s["notice"], Value::Null);
    assert_eq!(
        s["ports"]["TSW202 #1/port1"],
        json!({ "locked": false, "pending": null })
    );

    // The configured packet radio on LAN4, with the port's live state.
    assert_eq!(
        s["attached"],
        json!([{ "name": "Packet radio", "port": "LAN4", "ip": "192.168.30.50", "linkUp": true, "speedMbps": 100.0, "mac": "00:30:1A:00:00:50", "vlan": { "untagged": 1, "tagged": [] } }])
    );

    // Two links, not three: each switch learns the other through the router, not directly.
    assert_eq!(
        s["links"],
        json!([
            { "a": { "device": "RUTM50", "port": "LAN1" }, "b": { "device": "TSW202 #1", "port": "1" }, "up": true, "speedMbps": 1000.0, "vlan": { "untagged": 1, "tagged": [10] } },
            { "a": { "device": "RUTM50", "port": "LAN2" }, "b": { "device": "TSW202 #2", "port": "1" }, "up": true, "speedMbps": 1000.0, "vlan": { "untagged": 1, "tagged": [20] } },
        ])
    );
}

#[tokio::test]
async fn turns_a_switch_port_off_and_on_and_receives_the_hook() {
    let (_g, url) = setup().await;
    for (sw, device, id, text) in [
        (0, "TSW202 #1", "port3", "port 3 changed to DOWN"),
        (1, "TSW202 #2", "port4", "port 4 changed to DOWN"),
    ] {
        let (code, _) = toggle(
            url,
            json!({ "target": "port", "device": device, "port": id, "enabled": false }),
        )
        .await;
        assert_eq!(code, 200);
        let off = until(url, |s| port(s, sw, id)["enabled"] == false, "port off", 20).await;
        assert_eq!(port(&off, sw, id)["linkUp"], false);
        until(url, |s| has_event(s, device, text), "port hook", 20).await;

        let (code, _) = toggle(
            url,
            json!({ "target": "port", "device": device, "port": id, "enabled": true }),
        )
        .await;
        assert_eq!(code, 200);
        until(
            url,
            |s| port(s, sw, id)["linkUp"] == true,
            "port link up",
            20,
        )
        .await;
    }
}

/// Reads Server-Sent Events from the panel and sends each JSON payload to `seen`.
async fn read_events(url: &str, seen: tokio::sync::mpsc::UnboundedSender<Value>) {
    let mut res = reqwest::get(format!("{url}/api/events")).await.unwrap();
    assert!(
        res.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("text/event-stream")
    );
    let mut buf = String::new();
    while let Ok(Some(chunk)) = res.chunk().await {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(i) = buf.find("\n\n") {
            let event: String = buf.drain(..i + 2).collect();
            if let Some(data) = event.lines().find_map(|l| l.strip_prefix("data:")) {
                let _ = seen.send(serde_json::from_str(data.trim()).unwrap());
            }
        }
    }
}

#[tokio::test]
async fn streams_the_full_status_over_server_sent_events() {
    let (_g, url) = setup().await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let reader = tokio::spawn(read_events(url, tx));
    let first = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    reader.abort();
    assert_eq!(first["router"]["label"], "RUTM50");
    assert!(first["controls"].is_object());
}

#[tokio::test]
async fn shows_a_change_as_pending_until_the_device_reports_it() {
    let (_g, url) = setup().await;
    // Watch the stream while the change runs: the server must report pending before the result.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let reader = tokio::spawn(read_events(url, tx));
    assert_eq!(
        toggle(url, json!({ "target": "wifi", "enabled": false }))
            .await
            .0,
        200
    );
    until(
        url,
        |s| s["controls"]["wifi"] == json!({ "on": false, "pending": false }),
        "Wi-Fi off settled",
        20,
    )
    .await;
    assert_eq!(
        toggle(url, json!({ "target": "wifi", "enabled": true }))
            .await
            .0,
        200
    );
    until(
        url,
        |s| s["controls"]["wifi"] == json!({ "on": true, "pending": false }),
        "Wi-Fi on settled",
        20,
    )
    .await;
    reader.abort();
    let mut pending = false;
    while let Ok(s) = rx.try_recv() {
        pending |= s["controls"]["wifi"]["pending"] == true;
    }
    assert!(pending, "a pending state was streamed");
}

#[tokio::test]
async fn turns_cellular_data_off_and_on_and_receives_the_mobile_hook() {
    let (_g, url) = setup().await;
    assert_eq!(
        toggle(url, json!({ "target": "cellular", "enabled": false }))
            .await
            .0,
        200
    );
    let off = until(
        url,
        |s| s["router"]["cellular"][0]["dataEnabled"] == false,
        "cellular off",
        20,
    )
    .await;
    assert_eq!(off["router"]["cellular"][0]["dataConnected"], false);
    until(
        url,
        |s| has_event(s, "RUTM50", "Mobile data disconnected"),
        "mobile hook",
        20,
    )
    .await;
    assert_eq!(
        toggle(url, json!({ "target": "cellular", "enabled": true }))
            .await
            .0,
        200
    );
    until(
        url,
        |s| s["router"]["cellular"][0]["dataConnected"] == true,
        "cellular connected",
        20,
    )
    .await;
}

#[tokio::test]
async fn turns_wifi_off_and_on() {
    let (_g, url) = setup().await;
    assert_eq!(
        toggle(url, json!({ "target": "wifi", "enabled": false }))
            .await
            .0,
        200
    );
    let off = until(
        url,
        |s| s["router"]["wifiControl"]["enabled"] == false,
        "Wi-Fi off",
        20,
    )
    .await;
    assert!(
        off["router"]["wifi"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["up"] == false)
    );
    assert_eq!(
        toggle(url, json!({ "target": "wifi", "enabled": true }))
            .await
            .0,
        200
    );
    until(
        url,
        |s| {
            s["router"]["wifi"]
                .as_array()
                .unwrap()
                .iter()
                .all(|w| w["up"] == true)
        },
        "Wi-Fi on",
        20,
    )
    .await;
}

#[tokio::test]
async fn keeps_the_router_online_while_tailscale_is_stopped() {
    let (_g, url) = setup().await;
    // A stopped Tailscale reports "" instead of lists; this once took the whole router offline.
    assert_eq!(
        toggle(url, json!({ "target": "tailscale", "enabled": false }))
            .await
            .0,
        200
    );
    let off = until(
        url,
        |s| s["router"]["tailscale"]["enabled"] == false,
        "Tailscale off",
        20,
    )
    .await;
    assert_eq!(off["router"]["online"], true);
    assert_eq!(off["router"]["tailscale"]["addresses"], json!([]));
    assert_eq!(
        toggle(url, json!({ "target": "tailscale", "enabled": true }))
            .await
            .0,
        200
    );
    until(
        url,
        |s| s["router"]["tailscale"]["running"] == true,
        "Tailscale running",
        20,
    )
    .await;
}

#[tokio::test]
async fn rejects_invalid_changes() {
    let (_g, url) = setup().await;
    assert_eq!(
        toggle(
            url,
            json!({ "target": "port", "device": "Nope", "port": "port1", "enabled": false })
        )
        .await,
        (409, json!({ "error": "unknown device" }))
    );
    assert_eq!(
        toggle(
            url,
            json!({ "target": "port", "device": "TSW202 #1", "port": "port99", "enabled": false })
        )
        .await,
        (409, json!({ "error": "unknown port" }))
    );
    assert_eq!(
        toggle(url, json!({ "target": "reboot", "enabled": true }))
            .await
            .0,
        400
    );
    let form = http()
        .post(format!("{url}/api/toggle"))
        .body("target=wifi&enabled=false")
        .send()
        .await
        .unwrap();
    assert_eq!(form.status(), 415);
}

#[tokio::test]
async fn accepts_hooks_only_from_the_devices() {
    let (_g, _) = setup().await;
    let res = http()
        .post(format!("{}/api/hook", stack().outsider_url))
        .body("spoofed")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
}
