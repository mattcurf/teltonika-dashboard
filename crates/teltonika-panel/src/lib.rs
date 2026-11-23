//! The panel server, for the `teltonika-panel` binary and the end-to-end tests.

pub mod server;

use std::sync::Arc;

use teltonika::Client;

/// One device on the panel.
pub struct Device {
    pub label: String,
    pub client: Arc<Client>,
}

/// The router and the switches, in panel order.
pub struct Devices {
    pub router: Device,
    pub switches: Vec<Device>,
    pub attached: Vec<Attached>,
}

/// A device on a router port, from `--rutm50-attached`. The panel cannot ask it anything, so
/// its name and address come from configuration; link state comes from the router port.
#[derive(Clone, Debug, PartialEq)]
pub struct Attached {
    /// Router port label, e.g. "LAN4".
    pub port: String,
    pub name: String,
    pub ip: Option<String>,
}

impl std::str::FromStr for Attached {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let (port, rest) = s
            .split_once('=')
            .ok_or_else(|| format!("expected PORT=NAME[@IP], got {s:?}"))?;
        let (name, ip) = match rest.rsplit_once('@') {
            Some((name, ip)) => (name, Some(ip.trim().to_string())),
            None => (rest, None),
        };
        let (port, name) = (port.trim(), name.trim());
        if port.is_empty() || name.is_empty() {
            return Err(format!("expected PORT=NAME[@IP], got {s:?}"));
        }
        Ok(Attached {
            port: port.to_uppercase(),
            name: name.into(),
            ip,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Attached;

    #[test]
    fn parses_attached_devices() {
        let a: Attached = " lan4 = Packet radio @ 192.168.30.50".parse().unwrap();
        assert_eq!(
            a,
            Attached {
                port: "LAN4".into(),
                name: "Packet radio".into(),
                ip: Some("192.168.30.50".into()),
            }
        );
        let b: Attached = "LAN3=Modem".parse().unwrap();
        assert_eq!(b.ip, None);
        assert!("Packet radio".parse::<Attached>().is_err());
        assert!("LAN4=".parse::<Attached>().is_err());
    }
}
