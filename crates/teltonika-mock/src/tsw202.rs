//! Mock TSW202 switch: 8 PoE Ethernet ports and 2 SFP ports. Response shapes come from a
//! TSW202 on firmware TSW2_R_00.01.11. Serial numbers and MAC addresses are fake.

use std::collections::HashMap;
use std::time::Instant;

use axum::http::Method;
use serde_json::{Value, json};

use crate::api::{ApiResult, Call, ConfigList, Device};
use crate::juggler::Juggler;
use crate::{drift, parse_cables, parse_neighbors, parse_vlans};

const PORTS: [&str; 10] = [
    "port1", "port2", "port3", "port4", "port5", "port6", "port7", "port8", "sfp1", "sfp2",
];

/// Settings, read from MOCK_* environment variables by `from_env`.
#[derive(Clone, Debug)]
pub struct Config {
    pub mac: String,
    pub lan_ip: String,
    pub serial: String,
    pub hostname: String,
    /// MAC addresses each port learns while its link is up, as `port1=MAC,MAC;port3=MAC`.
    pub neighbors: String,
    /// Plugged-in ports, as `port1=1000;port5=1000/4200` (speed in Mbps / PoE draw in mW).
    pub cables: String,
    /// Port VLANs, as `1=port1:u,*:u;10=port1:t,port6:u`.
    pub vlans: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mac: "20:97:27:00:00:02".into(),
            lan_ip: "192.168.30.2/24".into(),
            serial: "1100000002".into(),
            hostname: "TSW202".into(),
            // Port 1 is the uplink, so it learns the router and the router's Wi-Fi clients.
            neighbors: "port1=20:97:27:00:00:01,A4:83:E7:00:00:40,F0:18:98:00:00:41;port3=DC:A6:32:00:00:10;port5=9C:05:D6:00:00:30;port6=EC:1A:C3:00:00:20".into(),
            // Port 3 runs at 100 Mbps and port 5 powers a 4.2 W camera.
            cables: "port1=1000;port3=100;port5=1000/4200;port6=1000".into(),
            vlans: "1=port1:u,*:u;10=port1:t,port6:u".into(),
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let d = Config::default();
        Config {
            mac: crate::env_or("MOCK_MAC", d.mac),
            lan_ip: crate::env_or("MOCK_LAN_IP", d.lan_ip),
            serial: crate::env_or("MOCK_SERIAL", d.serial),
            hostname: crate::env_or("MOCK_HOSTNAME", d.hostname),
            neighbors: crate::env_or("MOCK_NEIGHBORS", d.neighbors),
            cables: crate::env_or("MOCK_CABLES", d.cables),
            vlans: crate::env_or("MOCK_VLANS", d.vlans),
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Traffic {
    rx: f64,
    tx: f64,
    rx_bytes: f64,
    tx_bytes: f64,
}

pub struct Tsw202 {
    mac: String,
    lan_ip: String,
    serial: String,
    hostname: String,
    neighbors: HashMap<String, Vec<String>>,
    /// Speed in Mbps and PoE draw in mW of what is plugged into each port.
    cables: HashMap<String, (u32, u32)>,
    ports: ConfigList,
    vlans: ConfigList,
    traffic: HashMap<&'static str, Traffic>,
    started: Instant,
    load: f64,
}

fn is_sfp(id: &str) -> bool {
    id.starts_with("sfp")
}

impl Tsw202 {
    pub fn new(c: Config) -> Self {
        let vlans = parse_vlans(&c.vlans, &PORTS)
            .into_iter()
            .map(|(vid, row)| {
                let mut item = json!({ "id": format!("vlan{vid}"), ".type": "bridge-vlan", "vlan": vid.to_string(), "name": format!("vlan{vid}") });
                item.as_object_mut().unwrap().extend(row.into_iter().map(|(k, v)| (k, Value::from(v))));
                item
            })
            .collect();
        Tsw202 {
            mac: c.mac.to_uppercase(),
            lan_ip: c.lan_ip,
            serial: c.serial,
            hostname: c.hostname,
            neighbors: parse_neighbors(&c.neighbors),
            cables: parse_cables(&c.cables),
            ports: ConfigList::new(
                PORTS
                    .iter()
                    .map(|id| json!({ "id": id, ".type": "switch_port", "enabled": "1", "poe_enable": if is_sfp(id) { "0" } else { "1" }, "autoneg": "on", "eee_enable": "1", "stp_prio": "128" }))
                    .collect(),
            ),
            vlans: ConfigList::new(vlans),
            traffic: PORTS.iter().map(|p| (*p, Traffic::default())).collect(),
            started: Instant::now(),
            load: 0.6,
        }
    }

    fn link_up(&self, id: &str) -> bool {
        self.cables.contains_key(id) && self.ports.str(id, "enabled") == "1"
    }

    fn port_status(&self) -> Value {
        let ports = PORTS.iter().map(|id| {
            let up = self.link_up(id);
            let t = self.traffic[id];
            let (speed, poe_mw) = self.cables.get(*id).copied().unwrap_or((0, 0));
            let poe_on = self.ports.str(id, "poe_enable");
            let powered = up && poe_on == "1" && poe_mw > 0;
            let s = |on: bool, v: String| if on { v } else { "0".into() };
            json!({
                "id": id,
                "enabled": self.ports.str(id, "enabled"),
                "link": if up { "1" } else { "0" },
                "speed": s(up, speed.to_string()),
                "full_duplex": if up { "1" } else { "0" },
                "poe_enable": poe_on,
                "power": if powered { "1" } else { "0" },
                "budget": s(powered, poe_mw.to_string()),
                "rx_rate": s(up, t.rx.round().to_string()),
                "tx_rate": s(up, t.tx.round().to_string()),
                "rx_bytes": t.rx_bytes.round().to_string(),
                "tx_bytes": t.tx_bytes.round().to_string(),
                "traffic_load": "0.00",
            })
        });
        Value::Array(ports.collect())
    }

    /// The MAC address table, rebuilt from the ports whose link is up.
    fn forwarding(&self) -> Value {
        let rows = PORTS.iter().filter(|id| self.link_up(id)).flat_map(|id| {
            self.neighbors
                .get(*id)
                .into_iter()
                .flatten()
                .map(move |mac| json!({ ".type": "forwarding", "mac": mac, "vlan": "vlan1", "port": id }))
        });
        Value::Array(rows.collect())
    }

    /// Faceplate layout: odd ports on the top row, even ports below; SFP2 sits above SFP1.
    fn layout() -> Value {
        let rows = PORTS.iter().map(|id| {
            let num: String = id.chars().filter(char::is_ascii_digit).collect();
            let up = *id == "sfp2" || (!is_sfp(id) && num.parse::<u32>().unwrap_or(0) % 2 == 1);
            json!({
                "name": id,
                "type": if is_sfp(id) { "sfp" } else { "eth" },
                "block": if is_sfp(id) { "1" } else { "0" },
                "num": num,
                "position": if up { "up" } else { "down" },
            })
        });
        Value::Array(rows.collect())
    }
}

fn port_name(id: &str) -> String {
    if is_sfp(id) {
        id.to_uppercase()
    } else {
        format!("port {}", &id[4..])
    }
}

impl Device for Tsw202 {
    fn call(&mut self, call: &Call, juggler: &mut Juggler) -> Option<ApiResult> {
        if *call.method == Method::GET {
            let reply = match call.path {
                "/ports_settings/status" => self.port_status(),
                "/forwarding_table/status" => self.forwarding(),
                "/interfaces/status" => json!([
                    { "id": "static", "name": "static", "proto": "static", "network_type": "wired", "is_up": true, "up": true, "ipaddrs": [self.lan_ip], "macaddr": self.mac },
                    { "id": "dhcp6", "name": "dhcp6", "proto": "dhcpv6", "network_type": "wired", "is_up": true, "up": true, "ipaddrs": [] },
                ]),
                "/system/device/status" => json!({
                    "static": { "model": "Teltonika TSW2XX Switch", "hostname": self.hostname, "fw_version": "TSW2_R_00.01.11", "device_name": "TSW202" },
                    "mnfinfo": { "serial": self.serial, "mac": self.mac.replace(':', "") },
                    "board": { "network": { "static": { "protocol": "static", "macaddr": self.mac.to_lowercase(), "ports": Self::layout() } } },
                }),
                "/system/device/usage/status" => json!({
                    "uptime_seconds": self.started.elapsed().as_secs() + 3000,
                    "load": { "min1": self.load, "min5": 0.5, "min15": 0.45 },
                    "memory": { "ram_percentage": 55.8, "flash_percentage": 12.8 },
                }),
                _ => Value::Null,
            };
            if !reply.is_null() {
                return Some(Ok(reply));
            }
        }
        if let Some(r) = self.ports.route(call, "/ports_settings/config") {
            let (reply, changes) = match r {
                Ok(r) => r,
                Err(e) => return Some(Err(e)),
            };
            for (after, before) in changes {
                let id = after["id"].as_str().unwrap_or("");
                if after["enabled"] != before["enabled"] && self.cables.contains_key(id) {
                    let up = if after["enabled"] == "1" {
                        "UP"
                    } else {
                        "DOWN"
                    };
                    juggler.fire(
                        "Switch Events",
                        &format!("Link state of {} changed to {up}", port_name(id)),
                    );
                }
            }
            return Some(Ok(reply));
        }
        self.vlans
            .route(call, "/port_based_vlan/config")
            .map(|r| r.map(|(reply, _)| reply))
    }

    fn tick(&mut self) {
        self.load = drift(self.load, 0.05, 0.1, 1.5);
        for id in PORTS {
            if !self.link_up(id) {
                continue;
            }
            let cap = self.cables[id].0 as f64 * 1_000_000.0 / 8.0 / 200.0;
            let t = self.traffic.get_mut(id).unwrap();
            t.rx = drift(
                if t.rx == 0.0 { cap / 4.0 } else { t.rx },
                cap / 10.0,
                200.0,
                cap,
            );
            t.tx = drift(
                if t.tx == 0.0 { cap / 6.0 } else { t.tx },
                cap / 10.0,
                200.0,
                cap,
            );
            t.rx_bytes += t.rx;
            t.tx_bytes += t.tx;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(d: &mut Tsw202, path: &str) -> Value {
        d.call(
            &Call {
                method: &Method::GET,
                path,
                data: &Value::Null,
            },
            &mut Juggler::default(),
        )
        .unwrap()
        .unwrap()
    }

    fn port<'a>(ports: &'a Value, id: &str) -> &'a Value {
        ports
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()
    }

    #[test]
    fn reports_speed_poe_and_unplugged_ports() {
        let mut d = Tsw202::new(Config::default());
        let p = get(&mut d, "/ports_settings/status");
        assert_eq!(port(&p, "port3")["speed"], "100");
        assert_eq!(
            (
                port(&p, "port5")["power"].clone(),
                port(&p, "port5")["budget"].clone()
            ),
            (json!("1"), json!("4200"))
        );
        assert_eq!(
            (
                port(&p, "port2")["link"].clone(),
                port(&p, "port2")["speed"].clone()
            ),
            (json!("0"), json!("0"))
        );
        assert_eq!(port(&p, "sfp1")["poe_enable"], "0");
    }

    #[test]
    fn poe_off_stops_the_power_but_keeps_the_link() {
        let mut d = Tsw202::new(Config::default());
        let data = json!({ "poe_enable": "0" });
        d.call(
            &Call {
                method: &Method::PUT,
                path: "/ports_settings/config/port5",
                data: &data,
            },
            &mut Juggler::default(),
        );
        let p = get(&mut d, "/ports_settings/status");
        assert_eq!(
            (
                port(&p, "port5")["link"].clone(),
                port(&p, "port5")["power"].clone()
            ),
            (json!("1"), json!("0"))
        );
    }

    #[test]
    fn a_disabled_port_leaves_the_mac_table_and_fires_the_hook() {
        let mut d = Tsw202::new(Config::default());
        let mut j = Juggler::default();
        j.seed("http://panel/hook", &[("panel_ports", "Switch Events")]);
        let has = |d: &mut Tsw202, mac: &str| {
            get(d, "/forwarding_table/status")
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["mac"] == mac)
        };
        assert!(has(&mut d, "DC:A6:32:00:00:10"));
        let data = json!({ "enabled": "0" });
        d.call(
            &Call {
                method: &Method::PUT,
                path: "/ports_settings/config/port3",
                data: &data,
            },
            &mut j,
        );
        assert!(!has(&mut d, "DC:A6:32:00:00:10"));
        assert_eq!(
            j.take_hooks()[0].body,
            "Switch Events: Link state of port 3 changed to DOWN"
        );
        // SFP names are uppercase in the log text, as on the device.
        let mut d = Tsw202::new(Config {
            cables: "sfp1=1000".into(),
            ..Config::default()
        });
        d.call(
            &Call {
                method: &Method::PUT,
                path: "/ports_settings/config/sfp1",
                data: &data,
            },
            &mut j,
        );
        assert_eq!(
            j.take_hooks()[0].body,
            "Switch Events: Link state of SFP1 changed to DOWN"
        );
    }

    #[test]
    fn identity_layout_and_vlans_follow_the_config() {
        let mut d = Tsw202::new(Config {
            hostname: "TSW202-OT".into(),
            serial: "1100000003".into(),
            vlans: "1=port1:u;20=port1:t,*:u".into(),
            ..Config::default()
        });
        let s = get(&mut d, "/system/device/status");
        assert_eq!(
            (
                s["static"]["hostname"].clone(),
                s["mnfinfo"]["serial"].clone()
            ),
            (json!("TSW202-OT"), json!("1100000003"))
        );
        let layout = &s["board"]["network"]["static"]["ports"];
        assert_eq!(
            (layout[0]["position"].clone(), layout[1]["position"].clone()),
            (json!("up"), json!("down"))
        );
        assert_eq!(
            (
                layout[8]["position"].clone(),
                layout[9]["position"].clone(),
                layout[9]["num"].clone()
            ),
            (json!("down"), json!("up"), json!("2"))
        );
        let v = get(&mut d, "/port_based_vlan/config");
        assert_eq!(v[1]["id"], "vlan20");
        assert_eq!(
            (
                v[1]["port1"].clone(),
                v[1]["port4"].clone(),
                v[0]["port4"].clone()
            ),
            (json!("t"), json!("u"), json!(""))
        );
    }

    #[test]
    fn traffic_counts_only_on_linked_ports() {
        let mut d = Tsw202::new(Config::default());
        d.tick();
        let p = get(&mut d, "/ports_settings/status");
        assert_ne!(port(&p, "port1")["rx_bytes"], "0");
        assert_eq!(port(&p, "port2")["rx_bytes"], "0");
    }
}
