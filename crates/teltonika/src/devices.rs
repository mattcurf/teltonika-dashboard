//! Reads router and switch status from the API and normalizes it into the model.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

use crate::client::{Client, PollResults, Result};
use crate::model::*;

// Cache lifetimes. Device info never changes while the device runs; config changes only on edits.
const ALWAYS: Duration = Duration::ZERO;
const STATIC: Duration = Duration::from_secs(600);
const CONFIG: Duration = Duration::from_secs(60);
const TOPOLOGY: Duration = Duration::from_secs(15);

const SYSTEM_PATHS: [(&str, Duration); 2] = [
    ("/system/device/status", STATIC),
    ("/system/device/usage/status", ALWAYS),
];

/// RutOS sends `""` instead of `[]` for empty lists (e.g. Tailscale `ip` while stopped), so
/// never trust a list field to be an array. A non-empty string becomes a one-item list.
pub fn list(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) if !s.is_empty() => vec![Value::String(s.clone())],
        _ => vec![],
    }
}

fn s(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// A number that the API may send as a JSON number or as a string.
fn num(v: &Value, key: &str) -> Option<f64> {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n| n.is_finite())
}

fn n0(v: &Value, key: &str) -> f64 {
    num(v, key).unwrap_or(0.0)
}

fn strings(v: Option<&Value>) -> Vec<String> {
    list(v)
        .iter()
        .map(|x| {
            x.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| x.to_string())
        })
        .collect()
}

fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// "209727E656DF" -> "20:97:27:E6:56:DF", the format ports and forwarding tables use.
pub fn format_mac(m: &str) -> String {
    let up = m.to_uppercase();
    if up.contains(':') || !up.len().is_multiple_of(2) {
        return up;
    }
    up.as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(":")
}

/// VLAN membership of one port. Each bridge VLAN names its ID `vid` (router) or `vlan`
/// (switch); each port key holds "u" (untagged), "t" (tagged) or "" (not a member).
/// OpenWrt may append "*" to mark the PVID, which wins when a port is untagged in several.
pub fn vlan_membership(vlans: &Value, port_key: &str) -> Vlan {
    let mut out = Vlan::default();
    for v in array(vlans) {
        let id = num(v, "vid").or_else(|| num(v, "vlan"));
        let Some(id) = id.map(|i| i as u32) else {
            continue;
        };
        let mode = s(v, port_key);
        if mode.starts_with('u') && (out.untagged.is_none() || mode.ends_with('*')) {
            out.untagged = Some(id);
        } else if mode.starts_with('t') {
            out.tagged.push(id);
        }
    }
    out.tagged.sort_unstable();
    out
}

struct System {
    info: SystemInfo,
    raw: Value,
}

fn system(r: &PollResults) -> Result<System> {
    let raw = r.get("/system/device/status")?.clone();
    let usage = r.get("/system/device/usage/status")?;
    let st = raw.get("static").cloned().unwrap_or_default();
    let mem = usage.get("memory").cloned().unwrap_or_default();
    let name = s(&st, "device_name");
    Ok(System {
        info: SystemInfo {
            model: if name.is_empty() {
                s(&st, "model")
            } else {
                name
            },
            hostname: s(&st, "hostname"),
            firmware: s(&st, "fw_version"),
            serial: s(raw.get("mnfinfo").unwrap_or(&Value::Null), "serial"),
            uptime_seconds: n0(usage, "uptime_seconds"),
            load1: usage.get("load").map(|l| n0(l, "min1")).unwrap_or(0.0),
            ram_percent: n0(&mem, "ram_percentage"),
            flash_percent: n0(&mem, "flash_percentage"),
        },
        raw,
    })
}

fn bridge_mac(raw: &Value) -> Option<String> {
    raw.get("mnfinfo")
        .map(|m| s(m, "mac"))
        .filter(|m| !m.is_empty())
        .map(|m| format_mac(&m))
}

fn up_ips(ifaces: &Value, keep: impl Fn(&Value) -> bool) -> Vec<String> {
    array(ifaces)
        .iter()
        .filter(|i| i.get("is_up") == Some(&Value::Bool(true)) && keep(i))
        .flat_map(|i| strings(i.get("ipaddrs")))
        .collect()
}

// Tailscale is an optional RutOS package; its endpoints fail when it is not installed.
fn tailscale(r: &PollResults) -> Option<TailscaleStatus> {
    let config = r
        .optional("/tailscale/config")
        .map(array)
        .unwrap_or_default();
    let cfg = config
        .iter()
        .find(|c| s(c, "id") == "general")
        .or(config.first())?;
    let st = r
        .optional("/tailscale/status")
        .and_then(|v| array(v).first())
        .cloned()
        .unwrap_or_default();
    // A stopped service sends `peers: []` instead of an object.
    let peers = st
        .get("peers")
        .filter(|p| p.is_object())
        .cloned()
        .unwrap_or_default();
    Some(TailscaleStatus {
        enabled: s(cfg, "enabled") == "1",
        running: s(&st, "status") == "1",
        login_url: s(&st, "url"),
        addresses: strings(st.get("ip")),
        version: s(&st, "version"),
        peers_online: n0(&peers, "online"),
        peers_total: n0(&peers, "total"),
        memory_bytes: st.get("ram_usage").map(|m| n0(m, "used")).unwrap_or(0.0),
        auth_type: s(cfg, "auth_type"),
        messages: strings(st.get("message")),
    })
}

/// Reads the router. A device that does not answer is reported offline, not as an error.
pub async fn router_status(c: &Client, label: &str) -> RouterStatus {
    match read_router(c, label).await {
        Ok(r) => r,
        Err(e) => RouterStatus::offline(label, &c.host, e.to_string()),
    }
}

async fn read_router(c: &Client, label: &str) -> Result<RouterStatus> {
    let r = c
        .poll(&[
            SYSTEM_PATHS[0],
            SYSTEM_PATHS[1],
            ("/interfaces/status", ALWAYS),
            ("/modems/status", ALWAYS),
            ("/wireless/interfaces/status", ALWAYS),
            ("/ports_settings/status", ALWAYS),
            ("/port_based_vlan/config", CONFIG),
            ("/interfaces/config", CONFIG),
            ("/tailscale/status", ALWAYS),
            ("/tailscale/config", CONFIG),
            // Radio settings only; /wireless/interfaces/config would also return the Wi-Fi passwords.
            ("/wireless/devices/config", CONFIG),
        ])
        .await?;
    let sys = system(&r)?;
    let ifaces = r.get("/interfaces/status")?;
    let modems = r.get("/modems/status")?;
    let wifi = r.get("/wireless/interfaces/status")?;
    let ports = r.get("/ports_settings/status")?;
    let vlans = r.get("/port_based_vlan/config")?;
    let iface_config = r.get("/interfaces/config")?;
    let radio_config = r.get("/wireless/devices/config")?;

    let mobile_ips = up_ips(ifaces, |i| s(i, "network_type") == "mobile");
    let cellular = array(modems)
        .iter()
        .map(|m| {
            let active_sim = num(m, "active_sim").unwrap_or(1.0) as i64;
            // Each SIM slot has its own wwan interface; the control changes the active SIM's one.
            let iface = array(iface_config).iter().find(|i| {
                s(i, "proto") == "wwan"
                    && s(i, "modem") == s(m, "id")
                    && s(i, "sim") == active_sim.to_string()
            });
            CellularStatus {
                interface_id: iface.map(|i| s(i, "id")),
                data_enabled: iface.is_none_or(|i| s(i, "enabled") != "0"),
                state: s(m, "state"),
                data_connected: s(m, "data_conn_state") == "Connected",
                operator: s(m, "operator"),
                connection_type: s(m, "conntype"),
                band: s(m, "band"),
                sim_state: s(m, "simstate"),
                active_sim,
                rssi: num(m, "rssi"),
                rsrp: num(m, "rsrp"),
                rsrq: num(m, "rsrq"),
                sinr: num(m, "sinr"),
                signal_quality: num(m, "signal_quality"),
                carriers: list(m.get("ca_signal"))
                    .iter()
                    .map(|c| Carrier {
                        band: s(c, "band"),
                        bandwidth_mhz: s(c, "bandwidth"),
                        primary: c.get("primary") == Some(&Value::Bool(true)),
                    })
                    .collect(),
                ipv4: mobile_ips.clone(),
                rx_bytes: n0(m, "rxbytes"),
                tx_bytes: n0(m, "txbytes"),
                temperature_c: num(m, "temperature"),
                modem_model: s(m, "model"),
            }
        })
        .collect();

    let radios = array(wifi)
        .iter()
        .map(|w| WifiRadio {
            ssid: s(w, "ssid"),
            band: strings(w.get("band")).join("/"),
            channel: n0(w, "channel"),
            mode: s(w, "mode"),
            up: w.get("up") == Some(&Value::Bool(true))
                && w.get("disabled") != Some(&Value::Bool(true)),
            encryption: s(w, "encryption"),
            tx_power_dbm: n0(w, "txpower"),
            noise_dbm: n0(w, "noise"),
            clients: list(w.get("clients"))
                .iter()
                .map(|c| WifiClient {
                    hostname: s(c, "hostname"),
                    ip: s(c, "ipaddr"),
                    mac: s(c, "macaddr"),
                    signal: s(c, "signal"),
                    standard: s(c, "standard"),
                })
                .collect(),
        })
        .collect();

    let mut port_list: Vec<(f64, PortStatus)> = array(ports)
        .iter()
        .map(|p| {
            let position = n0(p, "position");
            let is_wan = s(p, "name").eq_ignore_ascii_case("WAN");
            let id = s(p, "id");
            let topology = list(p.get("topology"));
            let port = PortStatus {
                label: if is_wan {
                    "WAN".into()
                } else {
                    format!("LAN{position}")
                },
                kind: if is_wan { PortKind::Wan } else { PortKind::Lan },
                row: "up".into(),
                enabled: s(p, "enabled") == "1",
                link_up: s(p, "state") == "up",
                speed_mbps: n0(p, "speed"),
                full_duplex: s(p, "duplex") == "true",
                poe: None,
                rx_rate: None,
                tx_rate: None,
                rx_bytes: None,
                tx_bytes: None,
                neighbors: Some(topology.len()),
                macs: topology
                    .iter()
                    .map(|t| s(t, "MAC").to_uppercase())
                    .collect(),
                // Port IDs are "_lan1" / "_wan"; VLAN entries use "lan1" / "wan".
                vlan: vlan_membership(vlans, id.trim_start_matches('_')),
                id,
            };
            (position, port)
        })
        .collect();
    port_list.sort_by(|a, b| a.0.total_cmp(&b.0));

    let radio_ids: Vec<String> = array(radio_config).iter().map(|d| s(d, "id")).collect();
    Ok(RouterStatus {
        base: DeviceBase {
            label: label.into(),
            host: c.host.clone(),
            online: true,
            error: None,
            mac: bridge_mac(&sys.raw),
            system: Some(sys.info),
            lan_ipv4: up_ips(ifaces, |i| s(i, "id") == "lan"),
            ports: port_list.into_iter().map(|(_, p)| p).collect(),
        },
        wan_ipv4: up_ips(ifaces, |i| s(i, "id") == "wan"),
        cellular,
        wifi: radios,
        wifi_control: (!radio_ids.is_empty()).then(|| WifiControl {
            enabled: array(radio_config).iter().any(|d| s(d, "enabled") != "0"),
            radios: radio_ids,
        }),
        tailscale: tailscale(&r),
    })
}

/// Reads a switch. A device that does not answer is reported offline, not as an error.
pub async fn switch_status(c: &Client, label: &str) -> SwitchStatus {
    match read_switch(c, label).await {
        Ok(s) => s,
        Err(e) => SwitchStatus {
            base: DeviceBase::offline(label, &c.host, e.to_string()),
        },
    }
}

async fn read_switch(c: &Client, label: &str) -> Result<SwitchStatus> {
    let r = c
        .poll(&[
            SYSTEM_PATHS[0],
            SYSTEM_PATHS[1],
            ("/ports_settings/status", ALWAYS),
            ("/port_based_vlan/config", CONFIG),
            // MAC address table: which port each MAC was learned on. Cabling changes rarely.
            ("/forwarding_table/status", TOPOLOGY),
            ("/interfaces/status", CONFIG),
        ])
        .await?;
    let sys = system(&r)?;
    let fdb = r.get("/forwarding_table/status")?;
    let ifaces = r.get("/interfaces/status")?;
    let ports = r.get("/ports_settings/status")?;
    let vlans = r.get("/port_based_vlan/config")?;
    let layout: HashMap<String, String> = list(sys.raw.pointer("/board/network/static/ports"))
        .iter()
        .map(|p| (s(p, "name"), s(p, "position")))
        .collect();
    let port_list = array(ports)
        .iter()
        .map(|p| {
            let id = s(p, "id");
            let sfp = id.starts_with("sfp");
            PortStatus {
                label: if sfp {
                    id.to_uppercase()
                } else {
                    id.replace("port", "")
                },
                kind: if sfp { PortKind::Sfp } else { PortKind::Lan },
                row: layout
                    .get(&id)
                    .cloned()
                    .filter(|r| !r.is_empty())
                    .unwrap_or_else(|| "up".into()),
                enabled: s(p, "enabled") == "1",
                link_up: s(p, "link") == "1",
                speed_mbps: n0(p, "speed"),
                full_duplex: s(p, "full_duplex") == "1",
                // `power` flags an active PoE load; `budget` is the draw in mW.
                poe: (!sfp).then(|| Poe {
                    enabled: s(p, "poe_enable") == "1",
                    active: s(p, "power") == "1",
                    watts: n0(p, "budget") / 1000.0,
                }),
                rx_rate: Some(n0(p, "rx_rate")),
                tx_rate: Some(n0(p, "tx_rate")),
                rx_bytes: Some(n0(p, "rx_bytes")),
                tx_bytes: Some(n0(p, "tx_bytes")),
                neighbors: None,
                vlan: vlan_membership(vlans, &id),
                macs: array(fdb)
                    .iter()
                    .filter(|f| s(f, "port") == id)
                    .map(|f| s(f, "mac").to_uppercase())
                    .collect(),
                id,
            }
        })
        .collect();
    // A switch can hold a static and a DHCP address on the same bridge.
    let mut seen = std::collections::HashSet::new();
    let lan: Vec<String> = up_ips(ifaces, |_| true)
        .into_iter()
        .filter(|ip| seen.insert(ip.clone()))
        .collect();
    Ok(SwitchStatus {
        base: DeviceBase {
            label: label.into(),
            host: c.host.clone(),
            online: true,
            error: None,
            mac: bridge_mac(&sys.raw),
            system: Some(sys.info),
            lan_ipv4: lan,
            ports: port_list,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn list_accepts_the_empty_string_quirk() {
        assert!(list(Some(&json!(""))).is_empty());
        assert_eq!(list(Some(&json!("x"))), vec![json!("x")]);
        assert_eq!(list(Some(&json!([1, 2]))).len(), 2);
        assert!(list(None).is_empty());
    }

    #[test]
    fn vlan_prefers_the_pvid_marker_and_sorts_trunks() {
        let vlans = json!([
            { "vid": "20", "lan1": "t" },
            { "vid": "1", "lan1": "u" },
            { "vid": "5", "lan1": "u*" },
            { "vid": "3", "lan1": "t" },
            { "vid": "9", "lan1": "" }
        ]);
        assert_eq!(
            vlan_membership(&vlans, "lan1"),
            Vlan {
                untagged: Some(5),
                tagged: vec![3, 20]
            }
        );
        let switch = json!([{ "vlan": "10", "port6": "u" }]);
        assert_eq!(vlan_membership(&switch, "port6").untagged, Some(10));
        assert_eq!(vlan_membership(&switch, "port1"), Vlan::default());
    }

    #[test]
    fn formats_compact_macs() {
        assert_eq!(format_mac("2097270000aa"), "20:97:27:00:00:AA");
        assert_eq!(format_mac("20:97:27:00:00:aa"), "20:97:27:00:00:AA");
    }
}
