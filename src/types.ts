// Normalized payload served at /api/status and rendered by the client.

export interface SystemInfo {
  model: string;
  hostname: string;
  firmware: string;
  serial: string;
  uptimeSeconds: number;
  load1: number;
  ramPercent: number;
  flashPercent: number;
}

export interface CellularStatus {
  // Mobile WAN interface of the active SIM, and whether it is enabled in the config.
  interfaceId: string | null;
  dataEnabled: boolean;
  state: string;
  dataConnected: boolean;
  operator: string;
  connectionType: string;
  band: string;
  simState: string;
  activeSim: number;
  rssi: number | null;
  rsrp: number | null;
  rsrq: number | null;
  sinr: number | null;
  signalQuality: number | null;
  carriers: { band: string; bandwidthMHz: string; primary: boolean }[];
  ipv4: string[];
  rxBytes: number;
  txBytes: number;
  temperatureC: number | null;
  modemModel: string;
}

export interface WifiClient {
  hostname: string;
  ip: string;
  mac: string;
  signal: string;
  standard: string;
}

export interface WifiRadio {
  ssid: string;
  band: string;
  channel: number;
  mode: string;
  up: boolean;
  encryption: string;
  txPowerDbm: number;
  noiseDbm: number;
  clients: WifiClient[];
}

export interface TailscaleStatus {
  enabled: boolean;
  running: boolean;
  // Set while the device waits for someone to open the login URL.
  loginUrl: string;
  addresses: string[];
  version: string;
  peersOnline: number;
  peersTotal: number;
  memoryBytes: number;
  authType: string;
  messages: string[];
}

export interface PortStatus {
  id: string;
  label: string;
  kind: "wan" | "lan" | "sfp";
  row: "up" | "down";
  enabled: boolean;
  linkUp: boolean;
  speedMbps: number;
  fullDuplex: boolean;
  poe?: { enabled: boolean; active: boolean; watts: number };
  rxRate?: number;
  txRate?: number;
  rxBytes?: number;
  txBytes?: number;
  neighbors?: number;
  // MAC addresses learned on this port, upper case. Used to find links between devices.
  macs: string[];
  // Default (untagged) VLAN and tagged (trunked) VLANs from /port_based_vlan/config.
  vlan: { untagged: number | null; tagged: number[] };
  // Set by the panel server when this Mac's MAC is learned on the port: turning it off would cut the panel off.
  panelPath?: boolean;
}

export interface DeviceBase {
  host: string;
  online: boolean;
  error?: string;
  system?: SystemInfo;
  // Bridge MAC (upper case, colon-separated) and LAN IPv4 addresses with prefix length.
  mac?: string;
  lanIpv4?: string[];
  ports: PortStatus[];
}

export interface RouterStatus extends DeviceBase {
  kind: "router";
  wanIpv4: string[];
  cellular: CellularStatus[];
  wifi: WifiRadio[];
  // Radio on/off from /wireless/devices/config; enabled when any radio is on. null without radios.
  wifiControl: { enabled: boolean; radios: string[] } | null;
  // null when the Tailscale package is not installed.
  tailscale: TailscaleStatus | null;
}

export interface SwitchStatus extends DeviceBase {
  kind: "switch";
  label: string;
}

// An Event Juggler notification received at /api/hook.
export interface DeviceEvent {
  at: string;
  // Label of the device that sent it, e.g. "RUTM50" or "TSW202 #2".
  device: string;
  text: string;
}

// A cable between two panel devices, found from the MAC addresses each port has learned.
export interface LinkEnd {
  device: string;
  port: string;
}
export interface DeviceLink {
  a: LinkEnd;
  b: LinkEnd;
  up: boolean;
  speedMbps: number;
  // VLAN membership of the link, read from end a's port.
  vlan: { untagged: number | null; tagged: number[] };
}

export interface DashboardStatus {
  fetchedAt: string;
  router: RouterStatus;
  switches: SwitchStatus[];
  links: DeviceLink[];
  events: DeviceEvent[];
}
