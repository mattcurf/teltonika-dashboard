//! Normalized device status. Field names serialize in camelCase for the panel's browser code,
//! and `cargo test` exports matching TypeScript types (see .cargo/config.toml).

use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub model: String,
    pub hostname: String,
    pub firmware: String,
    pub serial: String,
    pub uptime_seconds: f64,
    pub load1: f64,
    pub ram_percent: f64,
    pub flash_percent: f64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Carrier {
    pub band: String,
    #[serde(rename = "bandwidthMHz")]
    pub bandwidth_mhz: String,
    pub primary: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CellularStatus {
    /// Mobile WAN interface of the active SIM; the on/off control changes this interface.
    pub interface_id: Option<String>,
    /// Whether that interface is enabled in the config.
    pub data_enabled: bool,
    pub state: String,
    pub data_connected: bool,
    pub operator: String,
    pub connection_type: String,
    pub band: String,
    pub sim_state: String,
    pub active_sim: i64,
    pub rssi: Option<f64>,
    pub rsrp: Option<f64>,
    pub rsrq: Option<f64>,
    pub sinr: Option<f64>,
    pub signal_quality: Option<f64>,
    pub carriers: Vec<Carrier>,
    pub ipv4: Vec<String>,
    pub rx_bytes: f64,
    pub tx_bytes: f64,
    pub temperature_c: Option<f64>,
    pub modem_model: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct WifiClient {
    pub hostname: String,
    pub ip: String,
    pub mac: String,
    pub signal: String,
    pub standard: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WifiRadio {
    pub ssid: String,
    /// "2.4GHz", or "5GHz/2.4GHz" for one SSID on both bands.
    pub band: String,
    pub channel: f64,
    pub mode: String,
    pub up: bool,
    pub encryption: String,
    pub tx_power_dbm: f64,
    pub noise_dbm: f64,
    pub clients: Vec<WifiClient>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct WifiControl {
    /// True when any radio is on.
    pub enabled: bool,
    pub radios: Vec<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleStatus {
    pub enabled: bool,
    pub running: bool,
    /// Set while the device waits for someone to open the login URL.
    pub login_url: String,
    pub addresses: Vec<String>,
    pub version: String,
    pub peers_online: f64,
    pub peers_total: f64,
    pub memory_bytes: f64,
    pub auth_type: String,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct Vlan {
    /// Default (untagged) VLAN.
    pub untagged: Option<u32>,
    /// Tagged (trunked) VLANs, ascending.
    pub tagged: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct Poe {
    pub enabled: bool,
    pub active: bool,
    pub watts: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum PortKind {
    Wan,
    Lan,
    Sfp,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PortStatus {
    /// Port ID in the device API, e.g. "_lan1" or "port1". Status and config use the same ID.
    pub id: String,
    /// Faceplate label, e.g. "LAN1", "WAN", "3" or "SFP1".
    pub label: String,
    pub kind: PortKind,
    /// Faceplate row: "up" or "down".
    pub row: String,
    pub enabled: bool,
    pub link_up: bool,
    pub speed_mbps: f64,
    pub full_duplex: bool,
    pub poe: Option<Poe>,
    pub rx_rate: Option<f64>,
    pub tx_rate: Option<f64>,
    pub rx_bytes: Option<f64>,
    pub tx_bytes: Option<f64>,
    pub neighbors: Option<usize>,
    /// MAC addresses learned on this port, upper case.
    pub macs: Vec<String>,
    pub vlan: Vlan,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DeviceBase {
    /// Panel label, e.g. "RUTM50" or "TSW202 #1".
    pub label: String,
    pub host: String,
    pub online: bool,
    pub error: Option<String>,
    pub system: Option<SystemInfo>,
    /// Bridge MAC, upper case and colon-separated.
    pub mac: Option<String>,
    /// LAN IPv4 addresses with prefix length.
    pub lan_ipv4: Vec<String>,
    pub ports: Vec<PortStatus>,
}

impl DeviceBase {
    pub fn offline(label: &str, host: &str, error: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            host: host.into(),
            online: false,
            error: Some(error.into()),
            system: None,
            mac: None,
            lan_ipv4: vec![],
            ports: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RouterStatus {
    #[serde(flatten)]
    pub base: DeviceBase,
    pub wan_ipv4: Vec<String>,
    pub cellular: Vec<CellularStatus>,
    pub wifi: Vec<WifiRadio>,
    /// `None` when the router has no radios.
    pub wifi_control: Option<WifiControl>,
    /// `None` when the Tailscale package is not installed.
    pub tailscale: Option<TailscaleStatus>,
}

impl RouterStatus {
    pub fn offline(label: &str, host: &str, error: impl Into<String>) -> Self {
        Self {
            base: DeviceBase::offline(label, host, error),
            wan_ipv4: vec![],
            cellular: vec![],
            wifi: vec![],
            wifi_control: None,
            tailscale: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct SwitchStatus {
    #[serde(flatten)]
    pub base: DeviceBase,
}

/// One end of a cable between two devices.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct LinkEnd {
    pub device: String,
    pub port: String,
}

/// A cable between two devices, found from the MAC addresses each port has learned.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DeviceLink {
    pub a: LinkEnd,
    pub b: LinkEnd,
    pub up: bool,
    pub speed_mbps: f64,
    /// VLAN membership of the link, read from end a's port.
    pub vlan: Vlan,
}
