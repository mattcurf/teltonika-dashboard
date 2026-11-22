// Fetches raw API data from each device and normalizes it for the dashboard.
import type { PollResults, TeltonikaClient } from "./teltonika.ts";
import type {
  CellularStatus,
  PortStatus,
  RouterStatus,
  SwitchStatus,
  SystemInfo,
  TailscaleStatus,
  WifiRadio,
} from "./types.ts";

// RutOS sends "" instead of [] for empty lists (e.g. Tailscale `ip` while stopped), so never
// trust a list field to be an array. A non-empty string becomes a one-item list.
function list<T>(v: T[] | string | null | undefined): T[] {
  return Array.isArray(v) ? v : typeof v === "string" && v ? [v as unknown as T] : [];
}

/* Raw API shapes (only the fields used here). */
interface RawDeviceStatus {
  static: { model: string; hostname: string; fw_version: string; device_name: string };
  mnfinfo: { serial: string; mac: string };
  board?: { network?: { static?: { ports?: { name: string; position: "up" | "down"; type: string }[] } } };
}
interface RawUsage {
  uptime_seconds: number;
  load: { min1: number };
  memory: { ram_percentage: number; flash_percentage: number };
}
interface RawInterface {
  id: string;
  area_type: string | null;
  network_type: string;
  is_up: boolean | null;
  ipaddrs: string[];
}
interface RawInterfaceConfig {
  id: string;
  proto: string;
  enabled: string;
  sim?: string;
  modem?: string;
}
interface RawModem {
  id: string;
  name: string;
  model: string;
  state: string;
  data_conn_state: string;
  operator: string;
  conntype: string;
  band: string;
  simstate: string;
  active_sim: number;
  rssi?: number;
  rsrp?: number;
  rsrq?: number;
  sinr?: number;
  signal_quality?: number;
  ca_signal?: { band: string; bandwidth: string; primary: boolean }[];
  rxbytes: number;
  txbytes: number;
  temperature?: number;
}
interface RawWifi {
  ssid: string;
  band: string | string[]; // a multi-band SSID lists each band
  channel: number;
  mode: string;
  up: boolean;
  disabled: boolean;
  encryption: string;
  txpower: number;
  noise: number;
  clients: { hostname: string; ipaddr: string; macaddr: string; signal: string; standard: string }[];
}
interface RawRouterPort {
  id: string;
  name: string;
  position: number;
  state: string;
  enabled: string;
  speed?: number;
  duplex?: string;
  topology?: { MAC: string }[];
}
interface RawSwitchPort {
  id: string;
  link: string;
  enabled: string;
  speed: string;
  full_duplex: string;
  poe_enable: string;
  power: string;
  budget: string;
  tx_rate: string;
  rx_rate: string;
  tx_bytes: string;
  rx_bytes: string;
}

interface RawTailscaleStatus {
  status: string;
  url: string;
  ip: string[] | "";
  version: string;
  peers?: { total: string; online: string } | [];
  ram_usage?: { used: string };
  message?: unknown[] | "";
}
interface RawTailscaleConfig {
  id: string;
  enabled: string;
  auth_type: string;
}

// Cache lifetimes. Device info never changes while the device runs; config changes only on edits.
const ALWAYS = 0;
const STATIC_MS = 10 * 60_000;
const CONFIG_MS = 60_000;
const TOPOLOGY_MS = 15_000;

// Tailscale is an optional RutOS package; its endpoints fail when it is not installed.
// Transport errors fail the whole bulk request, so a slow router shows as offline, not as "no Tailscale".

function tailscale(r: PollResults): TailscaleStatus | null {
  const s = r.optional<RawTailscaleStatus[]>("/tailscale/status")?.[0];
  const config = r.optional<RawTailscaleConfig[]>("/tailscale/config") ?? [];
  const cfg = config.find((x) => x.id === "general") ?? config[0];
  // Keep the card (and its on/off slider) when the service is disabled and reports no status.
  if (!cfg) return null;
  const peers = s?.peers && !Array.isArray(s.peers) ? s.peers : undefined;
  return {
    enabled: cfg.enabled === "1",
    running: s?.status === "1",
    loginUrl: s?.url ?? "",
    addresses: list<unknown>(s?.ip).map(String),
    version: s?.version ?? "",
    peersOnline: Number(peers?.online) || 0,
    peersTotal: Number(peers?.total) || 0,
    memoryBytes: Number(s?.ram_usage?.used) || 0,
    authType: cfg.auth_type,
    messages: list<unknown>(s?.message).map(String),
  };
}

// One bridge VLAN. The router names the ID `vid`, the switch `vlan`. Each port key holds
// "u" (untagged), "t" (tagged), or "" (not a member); OpenWrt may append "*" to mark the PVID.
type RawVlan = { vid?: string; vlan?: string } & Record<string, string>;

function vlanMembership(vlans: RawVlan[], portKey: string): PortStatus["vlan"] {
  let untagged: number | null = null;
  const tagged: number[] = [];
  for (const v of vlans) {
    const id = Number(v.vid ?? v.vlan);
    const mode = v[portKey] ?? "";
    if (mode.startsWith("u") && (untagged === null || mode.endsWith("*"))) untagged = id;
    else if (mode.startsWith("t")) tagged.push(id);
  }
  return { untagged, tagged: tagged.sort((a, b) => a - b) };
}

const SYSTEM_PATHS = { "/system/device/status": STATIC_MS, "/system/device/usage/status": ALWAYS };

function system(r: PollResults): { info: SystemInfo; raw: RawDeviceStatus } {
  const raw = r.get<RawDeviceStatus>("/system/device/status");
  const usage = r.get<RawUsage>("/system/device/usage/status");
  return {
    raw,
    info: {
      model: raw.static.device_name || raw.static.model,
      hostname: raw.static.hostname,
      firmware: raw.static.fw_version,
      serial: raw.mnfinfo.serial,
      uptimeSeconds: usage.uptime_seconds,
      load1: usage.load.min1,
      ramPercent: usage.memory.ram_percentage,
      flashPercent: usage.memory.flash_percentage,
    },
  };
}

// "209727E656DF" -> "20:97:27:E6:56:DF", the format ports and forwarding tables use.
const formatMac = (m: string) => m.toUpperCase().match(/../g)?.join(":") ?? m;

const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);

export async function routerStatus(c: TeltonikaClient): Promise<RouterStatus> {
  try {
    const r = await c.poll({
      ...SYSTEM_PATHS,
      "/interfaces/status": ALWAYS,
      "/modems/status": ALWAYS,
      "/wireless/interfaces/status": ALWAYS,
      "/ports_settings/status": ALWAYS,
      "/port_based_vlan/config": CONFIG_MS,
      "/interfaces/config": CONFIG_MS,
      "/tailscale/status": ALWAYS,
      "/tailscale/config": CONFIG_MS,
      // Radio settings only; /wireless/interfaces/config would also return the Wi-Fi passwords.
      "/wireless/devices/config": CONFIG_MS,
    });
    const radioConfig = r.get<{ id: string; enabled: string }[]>("/wireless/devices/config");
    const sys = system(r);
    const ifaces = r.get<RawInterface[]>("/interfaces/status");
    const modems = r.get<RawModem[]>("/modems/status");
    const wifi = r.get<RawWifi[]>("/wireless/interfaces/status");
    const ports = r.get<RawRouterPort[]>("/ports_settings/status");
    const vlans = r.get<RawVlan[]>("/port_based_vlan/config");
    const ts = tailscale(r);
    const ifaceConfig = r.get<RawInterfaceConfig[]>("/interfaces/config");
    const mobileIps = ifaces
      .filter((i) => i.network_type === "mobile" && i.is_up)
      .flatMap((i) => list(i.ipaddrs));
    const cellular: CellularStatus[] = modems.map((m) => {
      // Each SIM slot has its own wwan interface; the slider controls the active SIM's one.
      const iface = ifaceConfig.find((i) => i.proto === "wwan" && i.modem === m.id && i.sim === String(m.active_sim));
      return {
        interfaceId: iface?.id ?? null,
        dataEnabled: iface?.enabled !== "0",
        state: m.state,
        dataConnected: m.data_conn_state === "Connected",
        operator: m.operator,
        connectionType: m.conntype,
        band: m.band,
        simState: m.simstate,
        activeSim: m.active_sim,
        rssi: num(m.rssi),
        rsrp: num(m.rsrp),
        rsrq: num(m.rsrq),
        sinr: num(m.sinr),
        signalQuality: num(m.signal_quality),
        carriers: list(m.ca_signal).map((s) => ({ band: s.band, bandwidthMHz: s.bandwidth, primary: s.primary })),
        ipv4: mobileIps,
        rxBytes: m.rxbytes,
        txBytes: m.txbytes,
        temperatureC: num(m.temperature),
        modemModel: m.model,
      };
    });
    const radios: WifiRadio[] = wifi.map((w) => ({
      ssid: w.ssid,
      band: ([] as string[]).concat(w.band).join("/"),
      channel: w.channel,
      mode: w.mode,
      up: w.up && !w.disabled,
      encryption: w.encryption,
      txPowerDbm: w.txpower,
      noiseDbm: w.noise,
      clients: list(w.clients).map((cl) => ({
        hostname: cl.hostname,
        ip: cl.ipaddr,
        mac: cl.macaddr,
        signal: cl.signal,
        standard: cl.standard,
      })),
    }));
    const portList: PortStatus[] = [...ports]
      .sort((a, b) => a.position - b.position)
      .map((p) => {
        const isWan = p.name.toUpperCase() === "WAN";
        return {
          id: p.id,
          label: isWan ? "WAN" : `LAN${p.position}`,
          kind: isWan ? "wan" : "lan",
          row: "up",
          enabled: p.enabled === "1",
          linkUp: p.state === "up",
          speedMbps: p.speed ?? 0,
          fullDuplex: p.duplex === "true",
          neighbors: list(p.topology).length,
          macs: list(p.topology).map((t) => t.MAC.toUpperCase()),
          // Port IDs are "_lan1" / "_wan"; VLAN entries use "lan1" / "wan".
          vlan: vlanMembership(vlans, p.id.replace(/^_/, "")),
        };
      });
    return {
      kind: "router",
      host: c.host,
      online: true,
      system: sys.info,
      mac: formatMac(sys.raw.mnfinfo.mac),
      lanIpv4: ifaces.filter((i) => i.id === "lan" && i.is_up).flatMap((i) => list(i.ipaddrs)),
      wanIpv4: ifaces.filter((i) => i.id === "wan" && i.is_up).flatMap((i) => list(i.ipaddrs)),
      cellular,
      wifi: radios,
      wifiControl: radioConfig.length
        ? { enabled: radioConfig.some((d) => d.enabled !== "0"), radios: radioConfig.map((d) => d.id) }
        : null,
      tailscale: ts,
      ports: portList,
    };
  } catch (e) {
    return { kind: "router", host: c.host, online: false, error: String((e as Error).message), wanIpv4: [], cellular: [], wifi: [], wifiControl: null, tailscale: null, ports: [] };
  }
}

export async function switchStatus(c: TeltonikaClient, label: string): Promise<SwitchStatus> {
  try {
    const r = await c.poll({
      ...SYSTEM_PATHS,
      "/ports_settings/status": ALWAYS,
      "/port_based_vlan/config": CONFIG_MS,
      // MAC address table: which port each MAC was learned on. Cabling changes rarely.
      "/forwarding_table/status": TOPOLOGY_MS,
      "/interfaces/status": CONFIG_MS,
    });
    const fdb = r.get<{ mac: string; port: string }[]>("/forwarding_table/status");
    const ifaces = r.get<RawInterface[]>("/interfaces/status");
    const sys = system(r);
    const ports = r.get<RawSwitchPort[]>("/ports_settings/status");
    const vlans = r.get<RawVlan[]>("/port_based_vlan/config");
    const layout = new Map(list(sys.raw.board?.network?.static?.ports).map((p) => [p.name, p]));
    const portList: PortStatus[] = ports.map((p) => {
      const isSfp = p.id.startsWith("sfp");
      return {
        id: p.id,
        label: isSfp ? p.id.toUpperCase() : p.id.replace("port", ""),
        kind: isSfp ? "sfp" : "lan",
        row: layout.get(p.id)?.position ?? "up",
        enabled: p.enabled === "1",
        linkUp: p.link === "1",
        speedMbps: Number(p.speed) || 0,
        fullDuplex: p.full_duplex === "1",
        // `power` flags an active PoE load; `budget` is the draw in mW (board budget is 30000 per port).
        poe: isSfp
          ? undefined
          : { enabled: p.poe_enable === "1", active: p.power === "1", watts: (Number(p.budget) || 0) / 1000 },
        rxRate: Number(p.rx_rate) || 0,
        txRate: Number(p.tx_rate) || 0,
        rxBytes: Number(p.rx_bytes) || 0,
        txBytes: Number(p.tx_bytes) || 0,
        vlan: vlanMembership(vlans, p.id),
        macs: fdb.filter((f) => f.port === p.id).map((f) => f.mac.toUpperCase()),
      };
    });
    return {
      kind: "switch",
      label,
      host: c.host,
      online: true,
      system: sys.info,
      mac: formatMac(sys.raw.mnfinfo.mac),
      // A switch can hold a static and a DHCP address on the same bridge.
      lanIpv4: [...new Set(ifaces.filter((i) => i.is_up).flatMap((i) => list(i.ipaddrs)))],
      ports: portList,
    };
  } catch (e) {
    return { kind: "switch", label, host: c.host, online: false, error: String((e as Error).message), ports: [] };
  }
}
