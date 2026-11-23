//! Mock RUTM50 router. Response shapes come from a RUTM50 on firmware RUTM_R_00.07.25.3.
//! Serial numbers, MAC addresses and network names are fake.

use std::collections::HashMap;
use std::time::Instant;

use axum::http::Method;
use serde_json::{Value, json};

use crate::api::{ApiResult, Call, ConfigList, Device};
use crate::juggler::Juggler;
use crate::{drift, parse_cables, parse_neighbors, parse_vlans};

const MOBILE_IP: &str = "10.170.42.17/32";
const PORTS: [&str; 5] = ["lan1", "lan2", "lan3", "lan4", "wan"];
const LINK_MODES: [&str; 5] = ["10mh", "10mf", "100mh", "100mf", "1000mf"];

/// Settings, read from MOCK_* environment variables by `from_env`.
#[derive(Clone, Debug)]
pub struct Config {
    pub mac: String,
    pub lan_ip: String,
    pub wan_ip: String,
    /// MAC addresses each port learns while its link is up, as `lan1=MAC,MAC;wan=MAC`.
    pub neighbors: String,
    /// Plugged-in ports and their speed in Mbps, as `lan1=1000;wan=1000`.
    pub cables: String,
    /// LAN bridge VLANs, as `1=lan1:u,lan2:u;10=lan1:t`.
    pub vlans: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mac: "20:97:27:00:00:01".into(),
            lan_ip: "192.168.30.1/24".into(),
            wan_ip: "203.0.113.10/24".into(),
            // LAN1 cables to the switch, so it learns the switch and the hosts behind it.
            neighbors: "lan1=20:97:27:00:00:02,DC:A6:32:00:00:10,EC:1A:C3:00:00:20,9C:05:D6:00:00:30;wan=0C:EA:14:00:00:01".into(),
            cables: "lan1=1000;wan=1000".into(),
            vlans: "1=lan1:u,lan2:u,lan3:u,lan4:u;10=lan1:t".into(),
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let d = Config::default();
        Config {
            mac: crate::env_or("MOCK_MAC", d.mac),
            lan_ip: crate::env_or("MOCK_LAN_IP", d.lan_ip),
            wan_ip: crate::env_or("MOCK_WAN_IP", d.wan_ip),
            neighbors: crate::env_or("MOCK_NEIGHBORS", d.neighbors),
            cables: crate::env_or("MOCK_CABLES", d.cables),
            vlans: crate::env_or("MOCK_VLANS", d.vlans),
        }
    }
}

pub struct Rutm50 {
    mac: String,
    lan_ip: String,
    wan_ip: String,
    neighbors: HashMap<String, Vec<String>>,
    /// Link speed in Mbps of what is plugged into each port.
    cables: HashMap<String, u32>,
    ports: ConfigList,
    vlans: ConfigList,
    interfaces: ConfigList,
    radios: ConfigList,
    tailscale: ConfigList,
    started: Instant,
    rsrp: f64,
    rsrq: f64,
    sinr: f64,
    rssi: f64,
    load: f64,
    rx_bytes: f64,
    tx_bytes: f64,
}

impl Rutm50 {
    pub fn new(c: Config) -> Self {
        let mac = c.mac.to_uppercase();
        let vlans = parse_vlans(&c.vlans, &PORTS)
            .into_iter()
            .enumerate()
            .map(|(i, (vid, row))| {
                let mut item = json!({ "id": format!("cfg0{}", i + 1), ".type": "bridge-vlan", "vid": vid.to_string(), "device": "br-lan" });
                item.as_object_mut().unwrap().extend(row.into_iter().map(|(k, v)| (k, Value::from(v))));
                item
            })
            .collect();
        let lan_addr = c.lan_ip.split('/').next().unwrap_or("").to_string();
        Rutm50 {
            neighbors: parse_neighbors(&c.neighbors),
            cables: parse_cables(&c.cables).into_iter().map(|(k, (s, _))| (k, s)).collect(),
            ports: ConfigList::new(
                PORTS
                    .iter()
                    .map(|p| json!({ "id": format!("_{p}"), ".type": "port", "enabled": "1", "autoneg": "on", "advert": LINK_MODES }))
                    .collect(),
            ),
            vlans: ConfigList::new(vlans),
            // Each SIM slot has its own mobile interface; SIM 1 is the active one.
            interfaces: ConfigList::new(vec![
                json!({ "id": "lan", ".type": "interface", "proto": "static", "enabled": "1", "area_type": "lan", "ipaddr": lan_addr }),
                json!({ "id": "wan", ".type": "interface", "proto": "dhcp", "enabled": "1", "area_type": "wan" }),
                json!({ "id": "mob1s1a1", ".type": "interface", "proto": "wwan", "enabled": "1", "area_type": "wan", "modem": "2-1", "sim": "1", "pdptype": "ip" }),
                json!({ "id": "mob1s2a1", ".type": "interface", "proto": "wwan", "enabled": "1", "area_type": "wan", "modem": "2-1", "sim": "2", "pdptype": "ip" }),
            ]),
            radios: ConfigList::new(vec![
                json!({ "id": "radio0", ".type": "wifi-device", "enabled": "1", "channel": "auto", "hwmode": "n", "htmode": "HT20", "country": "US" }),
                json!({ "id": "radio1", ".type": "wifi-device", "enabled": "1", "channel": "36", "hwmode": "ac", "htmode": "VHT80", "country": "US" }),
            ]),
            tailscale: ConfigList::new(vec![
                json!({ "id": "general", ".type": "settings", "enabled": "1", "auth_type": "url", "default_route": "0", "memory_limit": "disabled" }),
            ]),
            mac,
            lan_ip: c.lan_ip,
            wan_ip: c.wan_ip,
            started: Instant::now(),
            rsrp: -94.0,
            rsrq: -10.0,
            sinr: 15.0,
            rssi: -72.0,
            load: 0.3,
            rx_bytes: 812_000_000.0,
            tx_bytes: 54_000_000.0,
        }
    }

    fn link_up(&self, port: &str) -> bool {
        self.cables.contains_key(port) && self.ports.str(&format!("_{port}"), "enabled") == "1"
    }

    fn mobile_on(&self) -> bool {
        self.interfaces.str("mob1s1a1", "enabled") == "1"
    }

    fn radio_on(&self, id: &str) -> bool {
        self.radios.str(id, "enabled") != "0"
    }

    fn port_status(&self) -> Value {
        let ports = PORTS.iter().enumerate().map(|(i, p)| {
            let wan = *p == "wan";
            let mac = if wan {
                format!("{}02", &self.mac[..self.mac.len() - 2])
            } else {
                self.mac.clone()
            };
            let up = self.link_up(p);
            let mut port = json!({
                "id": format!("_{p}"),
                "name": if wan { "WAN" } else { "LAN" },
                "position": i + 1,
                "num": i + 1,
                "mac": mac,
                "enabled": self.ports.str(&format!("_{p}"), "enabled"),
                "state": if up { "up" } else { "down" },
                "link_supported": LINK_MODES,
            });
            if up {
                let topology: Vec<Value> = self
                    .neighbors
                    .get(*p)
                    .into_iter()
                    .flatten()
                    .map(|m| json!({ "MAC": m, "VID": 0 }))
                    .collect();
                let o = port.as_object_mut().unwrap();
                o.insert("speed".into(), self.cables[*p].into());
                o.insert("duplex".into(), "true".into());
                o.insert("topology".into(), topology.into());
            }
            port
        });
        Value::Array(ports.collect())
    }

    fn interface_status(&self) -> Value {
        let mobile = self.mobile_on();
        let wan = self.link_up("wan");
        json!([
            { "id": "lan", "name": "lan", "area_type": "lan", "network_type": "wireless", "proto": "static", "is_up": true, "up": true, "ipaddrs": [self.lan_ip], "macaddr": self.mac },
            { "id": "loopback", "name": "loopback", "area_type": "lan", "network_type": "wired", "proto": "static", "is_up": true, "up": true, "ipaddrs": ["127.0.0.1/8"] },
            { "id": "mob1s1a1", "name": "mob1s1a1", "area_type": "wan", "network_type": "mobile", "proto": "wwan", "is_up": if mobile { json!(true) } else { Value::Null }, "ipaddrs": if mobile { json!([MOBILE_IP]) } else { json!([]) } },
            { "id": "mob1s2a1", "name": "mob1s2a1", "area_type": "wan", "network_type": "mobile", "proto": "wwan", "is_up": null, "ipaddrs": [] },
            { "id": "wan", "name": "wan", "area_type": "wan", "network_type": "wired", "proto": "dhcp", "is_up": wan, "ipaddrs": if wan { json!([self.wan_ip]) } else { json!([]) } },
        ])
    }

    fn modem_status(&self) -> Value {
        let state = if self.mobile_on() {
            "Connected"
        } else {
            "Disconnected"
        };
        json!([{
            "id": "2-1", "name": "Internal modem", "model": "RG520N-NA", "manufacturer": "Quectel",
            "state": state, "data_conn_state": state,
            "operator": "Mock Mobile", "oper": "Mock Mobile", "conntype": "5G (SA)", "ntype": "5G-SA", "band": "5G N41",
            "simstate": "Inserted", "pinstate": "Inserted", "netstate": "Registered, home", "active_sim": 1,
            "rssi": self.rssi.round(), "rsrp": self.rsrp.round(), "rsrq": self.rsrq.round(), "sinr": self.sinr.round(),
            "signal_quality": ((self.rsrp + 140.0) / 96.0 * 100.0).round(),
            "ca_signal": [{ "band": "5G N41", "bandwidth": "100", "primary": true, "pcid": 101, "frequency": 502110 }],
            "rxbytes": self.rx_bytes.round(), "txbytes": self.tx_bytes.round(), "temperature": 31,
        }])
    }

    /// Two radios. "LAB" is one SSID on both bands, which the API reports as a band list.
    fn wifi_status(&self) -> Value {
        let client = |hostname: &str, ip: &str, mac: &str, signal: &str| json!({ "hostname": hostname, "ipaddr": ip, "macaddr": mac, "signal": signal, "standard": "802.11ac" });
        let ssid = |ssid: &str, band: Value, channel: u32, radios: &[&str], clients: Vec<Value>| {
            let up = radios.iter().any(|r| self.radio_on(r));
            let single_2g = band == "2.4GHz";
            json!({
                "ssid": ssid, "band": band, "channel": channel, "mode": "ap", "up": up, "disabled": false,
                "encryption": "WPA2 PSK (CCMP)", "txpower": if single_2g { 23 } else { 17 }, "noise": if single_2g { -90 } else { -92 },
                "clients": if up { clients } else { vec![] },
            })
        };
        json!([
            ssid("RUT_MOCK_2G", json!("2.4GHz"), 11, &["radio0"], vec![]),
            ssid(
                "RUT_MOCK_5G",
                json!("5GHz"),
                36,
                &["radio1"],
                vec![client(
                    "laptop",
                    "192.168.30.140",
                    "A4:83:E7:00:00:40",
                    "-52 dBm"
                )]
            ),
            ssid(
                "LAB",
                json!(["5GHz", "2.4GHz"]),
                36,
                &["radio0", "radio1"],
                vec![client(
                    "tablet",
                    "192.168.30.141",
                    "F0:18:98:00:00:41",
                    "-61 dBm"
                )]
            ),
        ])
    }

    /// While Tailscale is stopped the router reports "" instead of lists, as the real one does.
    fn tailscale_status(&self) -> Value {
        if self.tailscale.str("general", "enabled") == "1" {
            json!([{ "status": "1", "url": "", "ip": ["100.64.0.1", "fd7a:115c:a1e0::1"], "version": "1.96.5", "peers": { "total": "5", "online": "3" }, "ram_usage": { "total": "236883968", "used": "34119680" }, "message": [] }])
        } else {
            json!([{ "status": "3", "url": "", "ip": "", "version": "", "peers": [], "ram_usage": { "total": "236883968", "used": "0" }, "message": "" }])
        }
    }
}

/// Routes a call to a config list. Returns the changed items with their values from before.
macro_rules! config {
    ($list:expr, $call:expr, $path:expr) => {
        match $list.route($call, $path) {
            Some(Ok(r)) => Some(r),
            Some(Err(e)) => return Some(Err(e)),
            None => None,
        }
    };
}

impl Device for Rutm50 {
    fn call(&mut self, call: &Call, juggler: &mut Juggler) -> Option<ApiResult> {
        if *call.method == Method::GET {
            let reply = match call.path {
                "/ports_settings/status" => self.port_status(),
                "/interfaces/status" => self.interface_status(),
                "/modems/status" => self.modem_status(),
                "/wireless/interfaces/status" => self.wifi_status(),
                "/tailscale/status" => self.tailscale_status(),
                "/system/device/status" => json!({
                    "static": { "model": "Teltonika RUTM50", "hostname": "RUTM50", "fw_version": "RUTM_R_00.07.25.3", "device_name": "RUTM50" },
                    "mnfinfo": { "serial": "1100000001", "mac": self.mac.replace(':', "") },
                }),
                "/system/device/usage/status" => json!({
                    "uptime_seconds": self.started.elapsed().as_secs() + 3600,
                    "load": { "min1": self.load, "min5": 0.25, "min15": 0.2 },
                    "memory": { "ram_percentage": 62.4, "flash_percentage": 9.1 },
                }),
                _ => Value::Null,
            };
            if !reply.is_null() {
                return Some(Ok(reply));
            }
        }
        if let Some((reply, changes)) = config!(self.ports, call, "/ports_settings/config") {
            for (after, before) in changes {
                let port = after["id"]
                    .as_str()
                    .unwrap_or("")
                    .trim_start_matches('_')
                    .to_string();
                if after["enabled"] != before["enabled"] && self.cables.contains_key(&port) {
                    let up = if after["enabled"] == "1" {
                        "UP"
                    } else {
                        "DOWN"
                    };
                    juggler.fire(
                        "Switch Events",
                        &format!("Port {} link state changed to {up}", port.to_uppercase()),
                    );
                }
            }
            return Some(Ok(reply));
        }
        if let Some((reply, changes)) = config!(self.interfaces, call, "/interfaces/config") {
            for (after, before) in changes {
                if after["id"] == "mob1s1a1" && after["enabled"] != before["enabled"] {
                    let state = if after["enabled"] == "1" {
                        "connected"
                    } else {
                        "disconnected"
                    };
                    juggler.fire("Mobile Data", &format!("Mobile data {state} (SIM 1)"));
                }
            }
            return Some(Ok(reply));
        }
        for (list, path) in [
            (&mut self.vlans, "/port_based_vlan/config"),
            (&mut self.radios, "/wireless/devices/config"),
            (&mut self.tailscale, "/tailscale/config"),
        ] {
            if let Some((reply, _)) = config!(list, call, path) {
                return Some(Ok(reply));
            }
        }
        None
    }

    /// Drifts the 5G signal and the load; traffic counts only while mobile data is on.
    fn tick(&mut self) {
        self.rsrp = drift(self.rsrp, 1.5, -105.0, -85.0);
        self.rsrq = drift(self.rsrq, 0.5, -13.0, -8.0);
        self.sinr = drift(self.sinr, 1.0, 5.0, 22.0);
        self.rssi = drift(self.rssi, 1.0, -80.0, -65.0);
        self.load = drift(self.load, 0.05, 0.05, 1.2);
        if self.mobile_on() {
            self.rx_bytes += drift(40_000.0, 20_000.0, 20_000.0, 60_000.0);
            self.tx_bytes += drift(8_000.0, 4_000.0, 4_000.0, 12_000.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(d: &mut Rutm50, path: &str) -> Value {
        let mut j = Juggler::default();
        d.call(
            &Call {
                method: &Method::GET,
                path,
                data: &Value::Null,
            },
            &mut j,
        )
        .unwrap()
        .unwrap()
    }

    fn put(d: &mut Rutm50, j: &mut Juggler, path: &str, data: Value) -> Value {
        d.call(
            &Call {
                method: &Method::PUT,
                path,
                data: &data,
            },
            j,
        )
        .unwrap()
        .unwrap()
    }

    fn seeded() -> Juggler {
        let mut j = Juggler::default();
        j.seed(
            "http://panel/hook",
            &[
                ("panel_ports", "Switch Events"),
                ("panel_mobile", "Mobile Data"),
            ],
        );
        j
    }

    #[test]
    fn ports_report_cables_neighbors_and_wan_mac() {
        let mut d = Rutm50::new(Config {
            cables: "lan1=1000;lan4=100;wan=1000".into(),
            neighbors: "lan4=00:30:1a:00:00:50".into(),
            ..Config::default()
        });
        let ports = get(&mut d, "/ports_settings/status");
        let p = |i: usize| &ports[i];
        assert_eq!(p(3)["speed"], 100);
        assert_eq!(
            p(3)["topology"],
            json!([{ "MAC": "00:30:1A:00:00:50", "VID": 0 }])
        );
        // An unplugged port reports no speed and no topology.
        assert_eq!(p(1)["state"], "down");
        assert!(p(1).get("speed").is_none() && p(1).get("topology").is_none());
        assert_eq!(p(4)["name"], "WAN");
        assert_eq!(p(4)["mac"], "20:97:27:00:00:02");
        assert_eq!(p(0)["mac"], "20:97:27:00:00:01");
    }

    #[test]
    fn disabling_a_plugged_port_drops_its_link_and_fires_the_hook() {
        let mut d = Rutm50::new(Config::default());
        let mut j = seeded();
        put(
            &mut d,
            &mut j,
            "/ports_settings/config/_lan1",
            json!({ "enabled": "0" }),
        );
        assert_eq!(get(&mut d, "/ports_settings/status")[0]["state"], "down");
        assert_eq!(
            j.take_hooks()[0].body,
            "Switch Events: Port LAN1 link state changed to DOWN"
        );
        // An unplugged port changes no link, so the router logs nothing.
        put(
            &mut d,
            &mut j,
            "/ports_settings/config/_lan3",
            json!({ "enabled": "0" }),
        );
        assert!(j.take_hooks().is_empty());
    }

    #[test]
    fn mobile_data_off_disconnects_the_modem_and_fires_the_hook() {
        let mut d = Rutm50::new(Config::default());
        let mut j = seeded();
        put(
            &mut d,
            &mut j,
            "/interfaces/config",
            json!([{ "id": "mob1s1a1", "enabled": "0" }]),
        );
        assert_eq!(
            get(&mut d, "/modems/status")[0]["data_conn_state"],
            "Disconnected"
        );
        let mob = &get(&mut d, "/interfaces/status")[2];
        assert_eq!(
            (mob["is_up"].clone(), mob["ipaddrs"].clone()),
            (Value::Null, json!([]))
        );
        assert_eq!(
            j.take_hooks()[0].body,
            "Mobile Data: Mobile data disconnected (SIM 1)"
        );
        // Traffic stops counting while data is off.
        let before = get(&mut d, "/modems/status")[0]["rxbytes"].clone();
        d.tick();
        assert_eq!(get(&mut d, "/modems/status")[0]["rxbytes"], before);
    }

    #[test]
    fn wifi_radios_off_take_every_ssid_down() {
        let mut d = Rutm50::new(Config::default());
        let mut j = Juggler::default();
        put(
            &mut d,
            &mut j,
            "/wireless/devices/config/radio1",
            json!({ "enabled": "0" }),
        );
        let w = get(&mut d, "/wireless/interfaces/status");
        // LAB is on both radios, so it stays up with one; RUT_MOCK_5G is on radio1 only.
        assert_eq!(
            (w[0]["up"].clone(), w[1]["up"].clone(), w[2]["up"].clone()),
            (json!(true), json!(false), json!(true))
        );
        assert_eq!(w[1]["clients"], json!([]));
        assert_eq!(w[2]["band"], json!(["5GHz", "2.4GHz"]));
        put(
            &mut d,
            &mut j,
            "/wireless/devices/config/radio0",
            json!({ "enabled": "0" }),
        );
        assert!(
            get(&mut d, "/wireless/interfaces/status")
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["up"] == false)
        );
    }

    #[test]
    fn stopped_tailscale_reports_empty_strings_instead_of_lists() {
        let mut d = Rutm50::new(Config::default());
        assert_eq!(get(&mut d, "/tailscale/status")[0]["ip"][0], "100.64.0.1");
        put(
            &mut d,
            &mut Juggler::default(),
            "/tailscale/config/general",
            json!({ "enabled": "0" }),
        );
        let t = &get(&mut d, "/tailscale/status")[0];
        assert_eq!(
            (t["ip"].clone(), t["message"].clone(), t["peers"].clone()),
            (json!(""), json!(""), json!([]))
        );
    }

    #[test]
    fn vlans_come_from_the_spec() {
        let mut d = Rutm50::new(Config {
            vlans: "1=lan1:u,lan2:u;20=lan2:t".into(),
            ..Config::default()
        });
        let v = get(&mut d, "/port_based_vlan/config");
        assert_eq!(
            v[0],
            json!({ "id": "cfg01", ".type": "bridge-vlan", "vid": "1", "device": "br-lan", "lan1": "u", "lan2": "u", "lan3": "", "lan4": "", "wan": "" })
        );
        assert_eq!(
            (
                v[1]["vid"].clone(),
                v[1]["lan2"].clone(),
                v[1]["lan1"].clone()
            ),
            (json!("20"), json!("t"), json!(""))
        );
    }

    #[test]
    fn unknown_paths_are_not_routed() {
        let mut d = Rutm50::new(Config::default());
        let call = Call {
            method: &Method::GET,
            path: "/nothing",
            data: &Value::Null,
        };
        assert!(d.call(&call, &mut Juggler::default()).is_none());
    }
}
