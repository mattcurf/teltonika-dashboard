import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { readFile } from "node:fs/promises";
import { networkInterfaces } from "node:os";
import { execFileSync } from "node:child_process";
import { lookup } from "node:dns/promises";
import { stripTypeScriptTypes } from "node:module";
import { router, switches } from "./config.ts";
import { routerStatus, switchStatus } from "./devices.ts";
import { findLinks } from "./topology.ts";
import type { DashboardStatus, DeviceEvent, RouterStatus, SwitchStatus } from "./types.ts";

// Most recent Event Juggler notifications, newest first.
const events: DeviceEvent[] = [];

const CACHE_MS = 3000;
const OFFLINE_RETRY_MS = 30_000;
// A first poll that is still running after this long shows the device as connecting.
const FIRST_POLL_WAIT_MS = 3000;

// Polls one device. Viewers share one poll, a new poll starts only after the previous one
// finished, and results stay fresh for 3 s. An unreachable device is retried at most every
// 30 s and serves its last result meanwhile, because a connection to an absent host can
// hang for 10 s and would otherwise hold up every other device on the dashboard.
class Poller<T extends { online: boolean }> {
  private readonly fetch: () => Promise<T>;
  private readonly placeholder: T;
  private last: { at: number; value: T } | null = null;
  private inflight: Promise<T> | null = null;

  constructor(fetch: () => Promise<T>, placeholder: T) {
    this.fetch = fetch;
    this.placeholder = placeholder;
  }

  get(): Promise<T> {
    const offline = this.last?.value.online === false;
    const fresh = this.last !== null && Date.now() - this.last.at <= (offline ? OFFLINE_RETRY_MS : CACHE_MS);
    if (this.last && fresh) return Promise.resolve(this.last.value);
    this.inflight ??= this.fetch()
      .then((value) => {
        this.last = { at: Date.now(), value };
        return value;
      })
      .finally(() => (this.inflight = null));
    if (this.last && offline) return Promise.resolve(this.last.value);
    if (!this.last) {
      const wait = new Promise<T>((r) => setTimeout(() => r(this.placeholder), FIRST_POLL_WAIT_MS));
      return Promise.race([this.inflight, wait]);
    }
    return this.inflight;
  }

  // Polls again now, after any poll in flight. Unreachable devices keep their retry schedule.
  async refresh(): Promise<void> {
    if (this.last?.value.online === false) return;
    await this.inflight?.catch(() => {});
    if (this.last) this.last.at = 0;
    await this.get().catch(() => {});
  }
}

const connecting = { online: false, error: "Connecting…", ports: [] };
const routerPoller = new Poller<RouterStatus>(() => routerStatus(router.client), {
  ...connecting, kind: "router", host: router.client.host, wanIpv4: [], cellular: [], wifi: [], wifiControl: null, tailscale: null,
});
const switchPollers = switches.map(
  (s) => new Poller<SwitchStatus>(() => switchStatus(s.client, s.label), { ...connecting, kind: "switch", label: s.label, host: s.client.host }),
);

// This host's MAC addresses. A port that has learned one of them carries the panel's own traffic.
// macOS hides real MACs from Node (02:00:00:00:00:00), so read them from ifconfig there.
function ownMacAddresses(): Set<string> {
  const macs = Object.values(networkInterfaces()).flat().map((i) => i?.mac ?? "");
  if (process.platform === "darwin") {
    try {
      macs.push(...(execFileSync("ifconfig", { encoding: "utf8" }).match(/ether [0-9a-f:]{17}/g) ?? []).map((m) => m.slice(6)));
    } catch {}
  }
  return new Set(macs.map((m) => m.toUpperCase()).filter((m) => m && !/^0[02]:00:00:00:00:00$/.test(m)));
}
const ownMacs = ownMacAddresses();

async function status(): Promise<DashboardStatus> {
  const [r, ...sw] = await Promise.all([routerPoller.get(), ...switchPollers.map((p) => p.get())]);
  for (const d of [r, ...sw]) for (const p of d.ports) p.panelPath = p.macs.some((m) => ownMacs.has(m));
  const links = findLinks([{ ...r, label: router.label }, ...sw]);
  return { fetchedAt: new Date().toISOString(), router: r, switches: sw, links, events };
}

// Browsers listening on /api/events; each gets a message when fresh data is ready.
const listeners = new Set<ServerResponse>();

// Polls again right away after a device event or a config change, then tells the browsers.
// Events often arrive in bursts (link state and speed), so they share one refresh.
let dirty = false;
let refreshing = false;
function refreshSoon(): void {
  dirty = true;
  if (!refreshing) void runRefresh();
}
async function runRefresh(): Promise<void> {
  refreshing = true;
  try {
    await new Promise((r) => setTimeout(r, 300));
    while (dirty) {
      dirty = false;
      await Promise.all([routerPoller, ...switchPollers].map((p) => p.refresh()));
      for (const res of listeners) res.write("data: refresh\n\n");
    }
  } finally {
    refreshing = false;
  }
}

const LOOPBACK = new Set(["127.0.0.1", "::1", "::ffff:127.0.0.1"]);
// Lets any host change device settings. Only for the mock stack, where the browser reaches
// the panel container through Docker's network and never from loopback.
const ALLOW_REMOTE_CONTROL = process.env.PANEL_ALLOW_REMOTE_CONTROL === "1";

// Label of the device at this IP address. Device hosts can be names (the mock stack uses
// "rutm50"), so they are resolved on each hook; a name that does not resolve matches nothing.
async function deviceAt(ip: string): Promise<string | undefined> {
  for (const d of [router, ...switches]) {
    const addresses = await lookup(d.client.hostname, { all: true }).catch(() => []);
    if (addresses.some((a) => a.address === ip)) return d.label;
  }
  return undefined;
}
const remote = (req: IncomingMessage) => (req.socket.remoteAddress ?? "").replace(/^::ffff:/, "");

function readBody(req: IncomingMessage, limit = 8192): Promise<string> {
  return new Promise((resolve, reject) => {
    let text = "";
    req.setEncoding("utf8");
    req.on("data", (chunk: string) => {
      text += chunk;
      if (text.length > limit) req.destroy(new Error("body too large"));
    });
    req.on("end", () => resolve(text));
    req.on("error", reject);
  });
}

function json(res: ServerResponse, code: number, body: unknown): void {
  res.writeHead(code, { "Content-Type": "application/json", "Cache-Control": "no-store" });
  res.end(JSON.stringify(body));
}

// Event Juggler HTTP action. Only the two devices may call it; any other LAN host gets 403.
async function hook(req: IncomingMessage, res: ServerResponse): Promise<void> {
  const device = await deviceAt(remote(req));
  if (!device) return json(res, 403, { error: "hooks are accepted only from the configured devices" });
  const text = (await readBody(req)).trim().slice(0, 500);
  events.unshift({ at: new Date().toISOString(), device, text });
  events.length = Math.min(events.length, 20);
  refreshSoon();
  json(res, 200, { ok: true });
}

// Router features the panel can turn on or off, each mapped to its official config endpoint.
type ToggleBody = { device?: unknown; port?: unknown };
const toggles: Record<string, (s: DashboardStatus, value: string, body: ToggleBody) => Promise<void> | string> = {
  // Mobile data on the active SIM's wwan interface.
  cellular: (s, value) => {
    const iface = s.router.cellular[0]?.interfaceId;
    if (!iface) return "no mobile interface found for the active SIM";
    return router.client.call("PUT", `/interfaces/config/${iface}`, { enabled: value }).then(() => {});
  },
  // Every radio, which takes down all SSIDs on it.
  wifi: (s, value) => {
    const radios = s.router.wifiControl?.radios ?? [];
    if (!radios.length) return "no Wi-Fi radios found";
    return router.client.call("PUT", "/wireless/devices/config", radios.map((id) => ({ id, enabled: value }))).then(() => {});
  },
  tailscale: (s, value) => {
    if (!s.router.tailscale) return "Tailscale is not installed";
    return router.client.call("PUT", "/tailscale/config/general", { enabled: value }).then(() => {});
  },
  // One Ethernet or SFP port on any device. Status port IDs ("_lan1", "port1") are also the config IDs.
  port: (s, value, body) => {
    const i = switches.findIndex((d) => d.label === body.device);
    const dev = body.device === router.label ? { client: router.client, ports: s.router.ports } : i >= 0 ? { client: switches[i].client, ports: s.switches[i].ports } : null;
    if (!dev) return "unknown device";
    const port = dev.ports.find((p) => p.id === body.port);
    if (!port) return "unknown port";
    if (value === "0" && port.panelPath) return `${/^\d+$/.test(port.label) ? `Port ${port.label}` : port.label} carries the panel's own connection to ${body.device}`;
    return dev.client.call("PUT", `/ports_settings/config/${port.id}`, { enabled: value }).then(() => {});
  },
};

// Turns a router feature on or off. Only loopback callers (this Mac and the Amp portal proxy)
// may change device config; other LAN hosts get 403. Requiring a JSON body also blocks
// cross-site form posts.
async function setToggle(req: IncomingMessage, res: ServerResponse): Promise<void> {
  if (!ALLOW_REMOTE_CONTROL && !LOOPBACK.has(req.socket.remoteAddress ?? "")) return json(res, 403, { error: "device control is allowed only through the panel" });
  if (!req.headers["content-type"]?.startsWith("application/json")) return json(res, 415, { error: "expected application/json" });
  const body = JSON.parse(await readBody(req)) as ToggleBody & { target?: unknown; enabled?: unknown };
  const { target, enabled } = body;
  const apply = typeof target === "string" && Object.hasOwn(toggles, target) ? toggles[target] : undefined;
  if (!apply || typeof enabled !== "boolean") {
    return json(res, 400, { error: `body must be {"target": "${Object.keys(toggles).join('"|"')}", "enabled": true|false}` });
  }
  const problem = await apply(await status(), enabled ? "1" : "0", body);
  if (typeof problem === "string") return json(res, 409, { error: problem });
  refreshSoon();
  json(res, 200, { ok: true, target, enabled });
}

function subscribe(req: IncomingMessage, res: ServerResponse): void {
  res.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-store", Connection: "keep-alive" });
  res.write(": connected\n\n");
  listeners.add(res);
  // Comments keep proxies from closing an idle stream.
  const ping = setInterval(() => res.write(": ping\n\n"), 25_000);
  req.on("close", () => {
    clearInterval(ping);
    listeners.delete(res);
  });
}

const root = new URL("../", import.meta.url);
const assets: Record<string, { file: string; type: string; ts?: boolean }> = {
  "/": { file: "public/index.html", type: "text/html; charset=utf-8" },
  "/styles.css": { file: "public/styles.css", type: "text/css; charset=utf-8" },
  "/app.js": { file: "src/client/app.ts", type: "text/javascript; charset=utf-8", ts: true },
};

const server = createServer(async (req, res) => {
  const path = new URL(req.url ?? "/", "http://x").pathname;
  try {
    if (path === "/api/status") return json(res, 200, await status());
    if (path === "/api/events") return subscribe(req, res);
    if (path === "/api/hook" && req.method === "POST") return await hook(req, res);
    if (path === "/api/toggle" && req.method === "POST") return await setToggle(req, res);
    const asset = assets[path];
    if (!asset) {
      res.writeHead(404).end("Not found");
      return;
    }
    let body = await readFile(new URL(asset.file, root), "utf8");
    if (asset.ts) body = stripTypeScriptTypes(body);
    res.writeHead(200, { "Content-Type": asset.type, "Cache-Control": "no-cache" });
    res.end(body);
  } catch (e) {
    if (res.headersSent) res.end();
    else json(res, 500, { error: (e as Error).message });
  }
});

const port = Number(process.env.PORT ?? 8080);
server.listen(port, () => console.log(`Teltonika panel on http://localhost:${port}`));
