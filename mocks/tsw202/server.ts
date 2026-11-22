// Mock Teltonika TSW202 switch: 8 PoE Ethernet ports and 2 SFP ports. Response shapes are
// taken from a TSW202 on firmware TSW2_R_00.01.11. Identifiers are fake.
import { MockDevice, drift, env, parseNeighbors, type Item } from "../common/api.ts";

const device = new MockDevice("TSW202");
const MAC = env("MOCK_MAC", "20:97:27:00:00:02").toUpperCase();
const LAN_IP = env("MOCK_LAN_IP", "192.168.30.2/24");
const started = Date.now();

// MAC addresses each port learns while its link is up. The defaults match the RUTM50 mock:
// port 1 is the uplink, so it learns the router and the router's Wi-Fi clients.
const neighbors = parseNeighbors(
  env("MOCK_NEIGHBORS", "port1=20:97:27:00:00:01,A4:83:E7:00:00:40,F0:18:98:00:00:41;port3=DC:A6:32:00:00:10;port5=9C:05:D6:00:00:30;port6=EC:1A:C3:00:00:20"),
);

// What is plugged in: link speed in Mbps (null: nothing) and PoE draw in mW.
// Port 3 runs at 100 Mbps and port 5 powers a 4.2 W camera, so the panel shows both states.
const cables: Record<string, { speed: number | null; poeMw: number }> = {
  port1: { speed: 1000, poeMw: 0 },
  port2: { speed: null, poeMw: 0 },
  port3: { speed: 100, poeMw: 0 },
  port4: { speed: null, poeMw: 0 },
  port5: { speed: 1000, poeMw: 4200 },
  port6: { speed: 1000, poeMw: 0 },
  port7: { speed: null, poeMw: 0 },
  port8: { speed: null, poeMw: 0 },
  sfp1: { speed: null, poeMw: 0 },
  sfp2: { speed: null, poeMw: 0 },
};
const ids = Object.keys(cables);
const isSfp = (id: string) => id.startsWith("sfp");

// Faceplate layout: odd ports on the top row, even ports below; SFP2 sits above SFP1.
const layout = ids.map((id) => ({
  name: id,
  type: isSfp(id) ? "sfp" : "eth",
  block: isSfp(id) ? "1" : "0",
  num: id.replace(/\D/g, ""),
  position: id === "sfp2" || (!isSfp(id) && Number(id.slice(4)) % 2 === 1) ? "up" : "down",
}));

const portConfig: Item[] = ids.map((id) => ({
  id,
  ".type": "switch_port",
  enabled: "1",
  poe_enable: isSfp(id) ? "0" : "1",
  autoneg: "on",
  eee_enable: "1",
  stp_prio: "128",
}));
const config = (id: string) => portConfig.find((c) => c.id === id)!;
const linkUp = (id: string) => cables[id].speed !== null && config(id).enabled === "1";
const portName = (id: string) => (isSfp(id) ? id.toUpperCase() : `port ${id.slice(4)}`);

device.configList("/ports_settings/config", portConfig, (item, before) => {
  if (item.enabled !== before.enabled && cables[item.id].speed !== null) {
    device.juggler.fire("Switch Events", `Link state of ${portName(item.id)} changed to ${item.enabled === "1" ? "UP" : "DOWN"}`);
  }
});

// Traffic per port: rates in bytes/s and running totals.
const traffic = Object.fromEntries(ids.map((id) => [id, { rx: 0, tx: 0, rxBytes: 0, txBytes: 0 }]));
device.on("GET", "/ports_settings/status", () =>
  ids.map((id) => {
    const up = linkUp(id);
    const t = traffic[id];
    const powered = up && config(id).poe_enable === "1" && cables[id].poeMw > 0;
    return {
      id,
      enabled: config(id).enabled,
      link: up ? "1" : "0",
      speed: up ? String(cables[id].speed) : "0",
      full_duplex: up ? "1" : "0",
      poe_enable: config(id).poe_enable,
      power: powered ? "1" : "0",
      budget: powered ? String(cables[id].poeMw) : "0",
      rx_rate: up ? String(Math.round(t.rx)) : "0",
      tx_rate: up ? String(Math.round(t.tx)) : "0",
      rx_bytes: String(Math.round(t.rxBytes)),
      tx_bytes: String(Math.round(t.txBytes)),
      traffic_load: "0.00",
    };
  }),
);

// MAC address table, rebuilt from the ports whose link is up.
device.on("GET", "/forwarding_table/status", () =>
  ids.filter(linkUp).flatMap((id) => (neighbors[id] ?? []).map((mac) => ({ ".type": "forwarding", mac, vlan: "vlan1", port: id }))),
);

// VLAN 1 untagged everywhere; VLAN 10 trunked on the uplink and untagged on port 6.
const vlan10: Item = { id: "vlan10", ".type": "bridge-vlan", vlan: "10", name: "vlan10", port1: "t", port6: "u" };
const vlan1: Item = { id: "vlan1", ".type": "bridge-vlan", vlan: "1", name: "vlan1" };
for (const id of ids) {
  vlan1[id] = id === "port6" ? "" : "u";
  vlan10[id] ??= "";
}
device.configList("/port_based_vlan/config", [vlan1, vlan10]);

device.on("GET", "/interfaces/status", () => [
  { id: "static", name: "static", proto: "static", network_type: "wired", is_up: true, up: true, ipaddrs: [LAN_IP], macaddr: MAC },
  { id: "dhcp6", name: "dhcp6", proto: "dhcpv6", network_type: "wired", is_up: true, up: true, ipaddrs: [] },
]);

const load = { min1: 0.6 };
device.on("GET", "/system/device/status", () => ({
  static: { model: "Teltonika TSW2XX Switch", hostname: "TSW202", fw_version: "TSW2_R_00.01.11", device_name: "TSW202" },
  mnfinfo: { serial: "1100000002", mac: MAC.replaceAll(":", "") },
  board: { network: { static: { protocol: "static", macaddr: MAC.toLowerCase(), ports: layout } } },
}));
device.on("GET", "/system/device/usage/status", () => ({
  uptime_seconds: Math.round((Date.now() - started) / 1000) + 3000,
  load: { min1: load.min1, min5: 0.5, min15: 0.45 },
  memory: { ram_percentage: 55.8, flash_percentage: 12.8 },
}));

setInterval(() => {
  load.min1 = drift(load.min1, 0.05, 0.1, 1.5);
  for (const id of ids.filter(linkUp)) {
    const t = traffic[id];
    const cap = (cables[id].speed! * 1_000_000) / 8 / 200;
    t.rx = drift(t.rx || cap / 4, cap / 10, 200, cap);
    t.tx = drift(t.tx || cap / 6, cap / 10, 200, cap);
    t.rxBytes += t.rx;
    t.txBytes += t.tx;
  }
}, 1000).unref();

const hookUrl = process.env.MOCK_HOOK_URL;
if (hookUrl) device.juggler.seed(hookUrl, { panel_ports: "Switch Events" });
device.listen();
