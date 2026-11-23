//! Mock Teltonika devices for testing the panel without hardware: a RUTM50 router and a
//! TSW202 switch. Each serves the Web API endpoints the panel uses, over HTTPS with a
//! self-signed certificate, with response shapes taken from real devices.

pub mod api;
pub mod juggler;
pub mod rutm50;
pub mod tsw202;

use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub use api::{Auth, Device, Mock};
pub use juggler::Juggler;

/// A random number from the standard library's hash seeds. Good enough for tokens and
/// drifting values in a test mock.
pub fn random_u64() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.finish()
}

/// The environment variable `name`, or `fallback` when it is unset or empty.
pub fn env_or(name: &str, fallback: String) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or(fallback)
}

/// A random walk for live-looking values, kept between `min` and `max`.
pub fn drift(value: f64, step: f64, min: f64, max: f64) -> f64 {
    let r = (random_u64() as f64 / u64::MAX as f64) * 2.0 - 1.0;
    (value + r * step).clamp(min, max)
}

/// Splits "a=x;b=y" into lowercase keys and trimmed values. Empty parts are skipped.
fn pairs(spec: &str) -> impl Iterator<Item = (String, &str)> {
    spec.split(';')
        .filter(|p| !p.trim().is_empty())
        .map(|part| {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            (k.trim().to_lowercase(), v.trim())
        })
}

/// "lan1=AA:..,BB:..;wan=CC:.." -> MAC addresses learned per port. Ports are lowercase and
/// MACs uppercase.
pub fn parse_neighbors(spec: &str) -> HashMap<String, Vec<String>> {
    pairs(spec)
        .map(|(port, macs)| {
            let macs = macs
                .split(',')
                .map(|m| m.trim().to_uppercase())
                .filter(|m| !m.is_empty())
                .collect();
            (port, macs)
        })
        .collect()
}

/// "port1=1000;port5=1000/4200" -> link speed in Mbps and PoE draw in mW per port.
pub fn parse_cables(spec: &str) -> HashMap<String, (u32, u32)> {
    pairs(spec)
        .map(|(port, v)| {
            let (speed, mw) = v.split_once('/').unwrap_or((v, "0"));
            (
                port,
                (
                    speed.trim().parse().unwrap_or(0),
                    mw.trim().parse().unwrap_or(0),
                ),
            )
        })
        .collect()
}

/// "1=port1:u,*:u;10=port1:t,port6:u" -> per VLAN, the mode of every port ("u", "t" or "").
/// "*" stands for every port the spec does not name anywhere. Ports in no entry get "".
pub fn parse_vlans(spec: &str, ports: &[&str]) -> BTreeMap<u32, BTreeMap<String, String>> {
    let entries: Vec<(u32, Vec<(String, String)>)> = pairs(spec)
        .filter_map(|(vid, members)| {
            let members = members
                .split(',')
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(|m| {
                    let (port, mode) = m.split_once(':').unwrap_or((m, "u"));
                    (port.trim().to_lowercase(), mode.trim().to_lowercase())
                })
                .collect();
            Some((vid.parse().ok()?, members))
        })
        .collect();
    let named: Vec<&str> = entries
        .iter()
        .flat_map(|(_, m)| m.iter().map(|(p, _)| p.as_str()))
        .filter(|p| *p != "*")
        .collect();
    entries
        .iter()
        .map(|(vid, members)| {
            let mut row: BTreeMap<String, String> = ports
                .iter()
                .map(|p| (p.to_string(), String::new()))
                .collect();
            for (port, mode) in members {
                if port == "*" {
                    for p in ports.iter().filter(|p| !named.contains(p)) {
                        row.insert(p.to_string(), mode.clone());
                    }
                } else {
                    row.insert(port.clone(), mode.clone());
                }
            }
            (*vid, row)
        })
        .collect()
}

/// Serves a mock over HTTPS on `listener`, with a fresh self-signed certificate as the real
/// devices have, and drifts its live values once a second.
pub async fn serve<D: Device>(mock: Arc<Mock<D>>, listener: TcpListener) -> std::io::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert = rcgen::generate_simple_self_signed(vec![mock.name().to_string()])
        .map_err(std::io::Error::other)?;
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        cert.cert.pem().into_bytes(),
        cert.key_pair.serialize_pem().into_bytes(),
    )
    .await?;
    let ticker = mock.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(Duration::from_secs(1));
        loop {
            every.tick().await;
            ticker.tick();
        }
    });
    listener.set_nonblocking(true)?;
    println!("{} mock on https://{}", mock.name(), listener.local_addr()?);
    axum_server::from_tcp_rustls(listener, tls)
        .serve(mock.router().into_make_service())
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighbors_are_case_normalised() {
        let n = parse_neighbors(" LAN1 = aa:bb , cc:dd ;wan=;;");
        assert_eq!(n["lan1"], vec!["AA:BB", "CC:DD"]);
        assert!(n["wan"].is_empty());
        assert_eq!(n.len(), 2);
    }

    #[test]
    fn cables_take_speed_and_optional_poe() {
        let c = parse_cables("port1=1000;Port5=1000/4200");
        assert_eq!(c["port1"], (1000, 0));
        assert_eq!(c["port5"], (1000, 4200));
    }

    #[test]
    fn vlan_star_covers_only_ports_named_nowhere() {
        // port1 is named in VLAN 1, so "*" in VLAN 10 skips it, even though VLAN 10 has it too.
        let v = parse_vlans("1=port1:u;10=port1:t,*:u", &["port1", "port2", "port3"]);
        assert_eq!(v[&1]["port1"], "u");
        assert_eq!(v[&1]["port2"], "");
        assert_eq!(v[&10]["port1"], "t");
        assert_eq!(v[&10]["port2"], "u");
        assert_eq!(v[&10]["port3"], "u");
        // A member without a mode is untagged.
        assert_eq!(
            parse_vlans("5=port2", &["port1", "port2"])[&5]["port2"],
            "u"
        );
    }

    #[test]
    fn drift_stays_in_range() {
        for _ in 0..1000 {
            let v = drift(5.0, 10.0, 0.0, 6.0);
            assert!((0.0..=6.0).contains(&v));
        }
    }
}
