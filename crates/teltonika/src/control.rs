//! Changes device settings through the official config endpoints.

use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{Client, Result};

fn flag(enabled: bool) -> &'static str {
    if enabled { "1" } else { "0" }
}

/// Turns mobile data on or off on one wwan interface, e.g. "mob1s1a1".
pub async fn set_mobile_interface(c: &Client, interface_id: &str, enabled: bool) -> Result<()> {
    c.call(
        Method::PUT,
        &format!("/interfaces/config/{interface_id}"),
        Some(json!({ "enabled": flag(enabled) })),
    )
    .await
    .map(drop)
}

/// Turns Wi-Fi radios on or off, which takes down every SSID on them.
pub async fn set_wifi_radios(c: &Client, radios: &[String], enabled: bool) -> Result<()> {
    let items: Vec<Value> = radios
        .iter()
        .map(|id| json!({ "id": id, "enabled": flag(enabled) }))
        .collect();
    c.call(
        Method::PUT,
        "/wireless/devices/config",
        Some(Value::Array(items)),
    )
    .await
    .map(drop)
}

pub async fn set_tailscale(c: &Client, enabled: bool) -> Result<()> {
    c.call(
        Method::PUT,
        "/tailscale/config/general",
        Some(json!({ "enabled": flag(enabled) })),
    )
    .await
    .map(drop)
}

/// Turns one Ethernet or SFP port on or off. Status port IDs ("_lan1", "port1") are also the config IDs.
pub async fn set_port(c: &Client, port_id: &str, enabled: bool) -> Result<()> {
    c.call(
        Method::PUT,
        &format!("/ports_settings/config/{port_id}"),
        Some(json!({ "enabled": flag(enabled) })),
    )
    .await
    .map(drop)
}
