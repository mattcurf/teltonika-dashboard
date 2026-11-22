// Mock Teltonika RUTM50 router. It serves the endpoints the panel reads and changes, with
// response shapes taken from a RUTM50 on firmware RUTM_R_00.07.25.3. Identifiers are fake.
import { MockDevice, drift, env, parseNeighbors, type Item } from "../common/api.ts";

const device = new MockDevice("RUTM50");
const MAC = env("MOCK_MAC", "20:97:27:00:00:01").toUpperCase();
const LAN_IP = env("MOCK_LAN_IP", "192.168.30.1/24");
const WAN_IP = env("MOCK_WAN_IP", "203.0.113.10/24");
const MOBILE_IP = "10.170.42.17/32";
const started = Date.now();

// MAC addresses each port learns while its link is up. The defaults match the TSW202 mock:
// LAN1 cables to the switch, so it learns the switch and the hosts behind it.
const neighbors = parseNeighbors(
  env("MOCK_NEIGHBORS", "lan1=20:97:27:00:00:02,DC:A6:32:00:00:10,EC:1A:C3:00:00:20,9C:05:D6:00:00:30;wan=0C:EA:14:00:00:01"),
);

// Physical ports. `cable` is the speed of what is plugged in; null means nothing is.
const ports = [
  { id: "_lan1", name: "LAN", position: 1, cable: 1000 as number | null },
  { id: "_lan2", name: "LAN", position: 2, cable: null },
  { id: "_lan3", name: "LAN", position: 3, cable: null },
  { id: "_lan4", name: "LAN", position: 4, cable: null },
  { id: "_wan", name: "WAN", position: 5, cable: 1000 },
];
const portConfig: Item[] = ports.map((p) => ({ id: p.id, ".type": "port", enabled: "1", autoneg: "on", advert: ["10mh", "10mf", "100mh", "100mf", "1000mf"] }));
const portLabel = (p: (typeof ports)[number]) => (p.name === "WAN" ? "WAN" : `LAN${p.position}`);
const linkUp = (p: (typeof ports)[number]) => p.cable !== null && portConfig.find((c) => c.id === p.id)?.enabled === "1";

device.configList("/ports_settings/config", portConfig, (item, before) => {
  const p = ports.find((x) => x.id === item.id)!;
  if (item.enabled !== before.enabled && p.cable !== null) {
    device.juggler.fire("Switch Events", `Port ${portLabel(p)} link state changed to ${item.enabled === "1" ? "UP" : "DOWN"}`);
  }
});
device.on("GET", "/ports_settings/status", () =>
  ports.map((p) => {
    const up = linkUp(p);
    const port: Record<string, unknown> = {
      id: p.id,
      name: p.name,
      position: p.position,
      num: p.position,
      mac: p.name === "WAN" ? MAC.replace(/..$/, "02") : MAC,
      enabled: portConfig.find((c) => c.id === p.id)!.enabled,
      state: up ? "up" : "down",
      link_supported: ["10mh", "10mf", "100mh", "100mf", "1000mf"],
    };
    if (up) {
      port.speed = p.cable;
      port.duplex = "true";
      port.topology = (neighbors[p.id.slice(1)] ?? []).map((m) => ({ MAC: m, VID: 0 }));
    }
    return port;
  }),
);

// LAN bridge VLANs: VLAN 1 untagged on every LAN port, VLAN 10 trunked to the switch on LAN1.
device.configList("/port_based_vlan/config", [
  { id: "cfg01", ".type": "bridge-vlan", vid: "1", device: "br-lan", lan1: "u", lan2: "u", lan3: "u", lan4: "u", wan: "" },
  { id: "cfg02", ".type": "bridge-vlan", vid: "10", device: "br-lan", lan1: "t", lan2: "", lan3: "", lan4: "", wan: "" },
]);

// Interfaces. Each SIM slot has its own mobile interface; SIM 1 is the active one.
const interfaces: Item[] = [
  { id: "lan", ".type": "interface", proto: "static", enabled: "1", area_type: "lan", ipaddr: LAN_IP.split("/")[0] },
  { id: "wan", ".type": "interface", proto: "dhcp", enabled: "1", area_type: "wan" },
  { id: "mob1s1a1", ".type": "interface", proto: "wwan", enabled: "1", area_type: "wan", modem: "2-1", sim: "1", pdptype: "ip" },
  { id: "mob1s2a1", ".type": "interface", proto: "wwan", enabled: "1", area_type: "wan", modem: "2-1", sim: "2", pdptype: "ip" },
];
const mobileOn = () => interfaces.find((i) => i.id === "mob1s1a1")!.enabled === "1";
device.configList("/interfaces/config", interfaces, (item, before) => {
  if (item.id === "mob1s1a1" && item.enabled !== before.enabled) {
    device.juggler.fire("Mobile Data", `Mobile data ${item.enabled === "1" ? "connected" : "disconnected"} (SIM 1)`);
  }
});
device.on("GET", "/interfaces/status", () => [
  { id: "lan", name: "lan", area_type: "lan", network_type: "wireless", proto: "static", is_up: true, up: true, ipaddrs: [LAN_IP], macaddr: MAC },
  { id: "loopback", name: "loopback", area_type: "lan", network_type: "wired", proto: "static", is_up: true, up: true, ipaddrs: ["127.0.0.1/8"] },
  { id: "mob1s1a1", name: "mob1s1a1", area_type: "wan", network_type: "mobile", proto: "wwan", is_up: mobileOn() ? true : null, ipaddrs: mobileOn() ? [MOBILE_IP] : [] },
  { id: "mob1s2a1", name: "mob1s2a1", area_type: "wan", network_type: "mobile", proto: "wwan", is_up: null, ipaddrs: [] },
  { id: "wan", name: "wan", area_type: "wan", network_type: "wired", proto: "dhcp", is_up: linkUp(ports[4]), ipaddrs: linkUp(ports[4]) ? [WAN_IP] : [] },
]);

// Modem with a slowly drifting 5G signal; traffic counts only while mobile data is on.
const signal = { rsrp: -94, rsrq: -10, sinr: 15, rssi: -72 };
const modemBytes = { rx: 812_000_000, tx: 54_000_000 };
device.on("GET", "/modems/status", () => [
  {
    id: "2-1",
    name: "Internal modem",
    model: "RG520N-NA",
    manufacturer: "Quectel",
    state: mobileOn() ? "Connected" : "Disconnected",
    data_conn_state: mobileOn() ? "Connected" : "Disconnected",
    operator: "Mock Mobile",
    oper: "Mock Mobile",
    conntype: "5G (SA)",
    ntype: "5G-SA",
    band: "5G N41",
    simstate: "Inserted",
    pinstate: "Inserted",
    netstate: "Registered, home",
    active_sim: 1,
    rssi: Math.round(signal.rssi),
    rsrp: Math.round(signal.rsrp),
    rsrq: Math.round(signal.rsrq),
    sinr: Math.round(signal.sinr),
    signal_quality: Math.round(((signal.rsrp + 140) / 96) * 100),
    ca_signal: [{ band: "5G N41", bandwidth: "100", primary: true, pcid: 101, frequency: 502110 }],
    rxbytes: Math.round(modemBytes.rx),
    txbytes: Math.round(modemBytes.tx),
    temperature: 31,
  },
]);

// Wi-Fi: two radios. "LAB" is one SSID on both bands, which the API reports as a band list.
const radios: Item[] = [
  { id: "radio0", ".type": "wifi-device", enabled: "1", channel: "auto", hwmode: "n", htmode: "HT20", country: "US" },
  { id: "radio1", ".type": "wifi-device", enabled: "1", channel: "36", hwmode: "ac", htmode: "VHT80", country: "US" },
];
const radioOn = (id: string) => radios.find((r) => r.id === id)!.enabled !== "0";
device.configList("/wireless/devices/config", radios);
const client = (hostname: string, ip: string, mac: string, signal: string) => ({ hostname, ipaddr: ip, macaddr: mac, signal, standard: "802.11ac" });
device.on("GET", "/wireless/interfaces/status", () => {
  const ssid = (ssid: string, band: string | string[], channel: number, radio: string[], clients: unknown[]) => {
    const up = radio.some(radioOn);
    return { ssid, band, channel, mode: "ap", up, disabled: false, encryption: "WPA2 PSK (CCMP)", txpower: band === "2.4GHz" ? 23 : 17, noise: band === "2.4GHz" ? -90 : -92, clients: up ? clients : [] };
  };
  return [
    ssid("RUT_MOCK_2G", "2.4GHz", 11, ["radio0"], []),
    ssid("RUT_MOCK_5G", "5GHz", 36, ["radio1"], [client("laptop", "192.168.30.140", "A4:83:E7:00:00:40", "-52 dBm")]),
    ssid("LAB", ["5GHz", "2.4GHz"], 36, ["radio0", "radio1"], [client("tablet", "192.168.30.141", "F0:18:98:00:00:41", "-61 dBm")]),
  ];
});

// Tailscale. While it is stopped the router reports "" instead of lists, as the real one does.
const tailscaleConfig: Item[] = [{ id: "general", ".type": "settings", enabled: "1", auth_type: "url", default_route: "0", memory_limit: "disabled" }];
device.configList("/tailscale/config", tailscaleConfig);
device.on("GET", "/tailscale/status", () =>
  tailscaleConfig[0].enabled === "1"
    ? [{ status: "1", url: "", ip: ["100.64.0.1", "fd7a:115c:a1e0::1"], version: "1.96.5", peers: { total: "5", online: "3" }, ram_usage: { total: "236883968", used: "34119680" }, message: [] }]
    : [{ status: "3", url: "", ip: "", version: "", peers: [], ram_usage: { total: "236883968", used: "0" }, message: "" }],
);

const load = { min1: 0.3 };
device.on("GET", "/system/device/status", () => ({
  static: { model: "Teltonika RUTM50", hostname: "RUTM50", fw_version: "RUTM_R_00.07.25.3", device_name: "RUTM50" },
  mnfinfo: { serial: "1100000001", mac: MAC.replaceAll(":", "") },
}));
device.on("GET", "/system/device/usage/status", () => ({
  uptime_seconds: Math.round((Date.now() - started) / 1000) + 3600,
  load: { min1: load.min1, min5: 0.25, min15: 0.2 },
  memory: { ram_percentage: 62.4, flash_percentage: 9.1 },
}));

setInterval(() => {
  signal.rsrp = drift(signal.rsrp, 1.5, -105, -85);
  signal.rsrq = drift(signal.rsrq, 0.5, -13, -8);
  signal.sinr = drift(signal.sinr, 1, 5, 22);
  signal.rssi = drift(signal.rssi, 1, -80, -65);
  load.min1 = drift(load.min1, 0.05, 0.05, 1.2);
  if (mobileOn()) {
    modemBytes.rx += 20_000 + Math.random() * 40_000;
    modemBytes.tx += 4_000 + Math.random() * 8_000;
  }
}, 1000).unref();

const hookUrl = process.env.MOCK_HOOK_URL;
if (hookUrl) device.juggler.seed(hookUrl, { panel_ports: "Switch Events", panel_mobile: "Mobile Data" });
device.listen();
