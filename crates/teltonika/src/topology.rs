//! Finds the cables between devices from the MAC addresses each port has learned.

use crate::model::{DeviceBase, DeviceLink, LinkEnd, PortStatus};

fn port_toward<'a>(from: &'a DeviceBase, to: &DeviceBase) -> Option<&'a PortStatus> {
    let mac = to.mac.as_ref()?;
    from.ports.iter().find(|p| p.macs.contains(mac))
}

/// Devices A and B share a cable when A learned B's MAC on port pa and B learned A's MAC on
/// port pb, and no third device was learned on both pa and pb. A switch between them would
/// show up on both ports, so router -> #1 -> #2 gives two links, not three.
pub fn find_links(nodes: &[&DeviceBase]) -> Vec<DeviceLink> {
    let up: Vec<&DeviceBase> = nodes
        .iter()
        .copied()
        .filter(|n| n.online && n.mac.is_some())
        .collect();
    let mut links = Vec::new();
    for (i, a) in up.iter().enumerate() {
        for b in &up[i + 1..] {
            let (Some(pa), Some(pb)) = (port_toward(a, b), port_toward(b, a)) else {
                continue;
            };
            let between = up.iter().any(|c| {
                !std::ptr::eq(*c, *a)
                    && !std::ptr::eq(*c, *b)
                    && c.mac
                        .as_ref()
                        .is_some_and(|m| pa.macs.contains(m) && pb.macs.contains(m))
            });
            if between {
                continue;
            }
            links.push(DeviceLink {
                a: LinkEnd {
                    device: a.label.clone(),
                    port: pa.label.clone(),
                },
                b: LinkEnd {
                    device: b.label.clone(),
                    port: pb.label.clone(),
                },
                up: pa.link_up,
                speed_mbps: pa.speed_mbps,
                vlan: pa.vlan.clone(),
            });
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PortKind, Vlan};

    fn port(label: &str, macs: &[&str]) -> PortStatus {
        PortStatus {
            id: label.into(),
            label: label.into(),
            kind: PortKind::Lan,
            row: "up".into(),
            enabled: true,
            link_up: true,
            speed_mbps: 1000.0,
            full_duplex: true,
            poe: None,
            rx_rate: None,
            tx_rate: None,
            rx_bytes: None,
            tx_bytes: None,
            neighbors: None,
            macs: macs.iter().map(|m| m.to_string()).collect(),
            vlan: Vlan::default(),
        }
    }
    fn dev(label: &str, mac: &str, ports: Vec<PortStatus>) -> DeviceBase {
        DeviceBase {
            label: label.into(),
            host: label.into(),
            online: true,
            error: None,
            system: None,
            mac: Some(mac.into()),
            lan_ipv4: vec![],
            ports,
        }
    }

    #[test]
    fn chain_gives_two_links_not_three() {
        // R.LAN1 - S1.p1, S1.p2 - S2.p1. R and S2 see each other only through S1.
        let r = dev("R", "R", vec![port("LAN1", &["S1", "S2"])]);
        let s1 = dev("S1", "S1", vec![port("1", &["R"]), port("2", &["S2"])]);
        let s2 = dev("S2", "S2", vec![port("1", &["S1", "R"])]);
        let links = find_links(&[&r, &s1, &s2]);
        let pairs: Vec<_> = links
            .iter()
            .map(|l| (l.a.port.as_str(), l.b.device.as_str(), l.b.port.as_str()))
            .collect();
        assert_eq!(pairs, [("LAN1", "S1", "1"), ("2", "S2", "1")]);
    }

    #[test]
    fn offline_devices_have_no_links() {
        let r = dev("R", "R", vec![port("LAN1", &["S1"])]);
        let mut s1 = dev("S1", "S1", vec![port("1", &["R"])]);
        s1.online = false;
        assert!(find_links(&[&r, &s1]).is_empty());
    }
}
