// End-to-end test of the panel against the device mocks.
// Start the stack first: docker compose -f compose.mock.yaml up -d --build --wait
// Then run: npm test (PANEL_URL selects another panel, default http://localhost:8080).
import { before, test } from "node:test";
import assert from "node:assert/strict";

// The status JSON types, generated from the Rust backend by ts-rs.
import type { PanelStatus as DashboardStatus } from "../web/src/types/PanelStatus.ts";
import type { PortStatus } from "../web/src/types/PortStatus.ts";

const PANEL = process.env.PANEL_URL ?? "http://localhost:8080";

async function status(): Promise<DashboardStatus> {
  const res = await fetch(`${PANEL}/api/status`);
  assert.equal(res.status, 200);
  return res.json() as Promise<DashboardStatus>;
}

async function toggle(body: Record<string, unknown>): Promise<{ status: number; body: { ok?: boolean; error?: string } }> {
  const res = await fetch(`${PANEL}/api/toggle`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  return { status: res.status, body: (await res.json()) as { ok?: boolean; error?: string } };
}

// Polls until check passes, because the panel caches device data for a few seconds.
async function until(check: (s: DashboardStatus) => boolean, what: string, ms = 20_000): Promise<DashboardStatus> {
  const end = Date.now() + ms;
  for (;;) {
    const s = await status();
    if (check(s)) return s;
    if (Date.now() > end) assert.fail(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 500));
  }
}

const switchPort = (s: DashboardStatus, id: string): PortStatus => s.switches[0].ports.find((p) => p.id === id)!;

// The mocks keep their state while they run, so an aborted earlier run may have left things off.
before(async () => {
  await until((s) => s.router.online && s.switches[0].online, "both mocks online", 60_000);
  for (const target of ["cellular", "wifi", "tailscale"]) await toggle({ target, enabled: true });
  await toggle({ target: "port", device: "TSW202 #1", port: "port3", enabled: true });
  await until((s) => s.router.tailscale?.running === true && s.router.cellular[0].dataConnected && switchPort(s, "port3").linkUp, "baseline state");
});

test("serves the page and the client script", async () => {
  const page = await fetch(`${PANEL}/`);
  assert.equal(page.status, 200);
  assert.match(await page.text(), /<script type="module" src="\/app.js">/);
  const script = await fetch(`${PANEL}/app.js`);
  assert.equal(script.status, 200);
  assert.match(script.headers.get("content-type") ?? "", /javascript/);
  assert.match(await script.text(), /new EventSource\("\/api\/events"\)/);
});

test("reads every device and finds the cable between them", async () => {
  const s = await until((s) => s.router.online && s.switches[0].online, "both mocks online");
  const r = s.router;
  assert.equal(r.system?.model, "RUTM50");
  assert.deepEqual(r.lanIpv4, ["192.168.30.1/24"]);
  assert.equal(r.cellular[0].operator, "Mock Mobile");
  assert.deepEqual(r.wifi.map((w) => w.band), ["2.4GHz", "5GHz", "5GHz/2.4GHz"]);
  assert.equal(r.tailscale?.running, true);
  assert.deepEqual(r.ports.find((p) => p.label === "LAN1")?.vlan, { untagged: 1, tagged: [10] });

  const sw = s.switches[0];
  assert.equal(sw.ports.length, 10);
  assert.equal(switchPort(s, "port3").speedMbps, 100);
  assert.deepEqual(switchPort(s, "port5").poe, { enabled: true, active: true, watts: 4.2 });
  assert.deepEqual(switchPort(s, "port6").vlan, { untagged: 10, tagged: [] });

  // Panel state the server adds for the browser.
  assert.deepEqual(s.controls, { cellular: { on: true, pending: false }, wifi: { on: true, pending: false }, tailscale: { on: true, pending: false } });
  assert.equal(s.notice, null);
  assert.deepEqual(s.ports["TSW202 #1/port1"], { locked: false, pending: null });

  // The second switch is not in the mock stack, as on the physical network.
  assert.equal(s.switches[1].online, false);

  assert.deepEqual(s.links, [
    { a: { device: "RUTM50", port: "LAN1" }, b: { device: "TSW202 #1", port: "1" }, up: true, speedMbps: 1000, vlan: { untagged: 1, tagged: [10] } },
  ]);
});

test("turns a switch port off and on, and receives the Event Juggler hook", async () => {
  assert.equal((await toggle({ target: "port", device: "TSW202 #1", port: "port3", enabled: false })).status, 200);
  const off = await until((s) => !switchPort(s, "port3").enabled, "port 3 off");
  assert.equal(switchPort(off, "port3").linkUp, false);
  await until((s) => s.events.some((e) => e.device === "TSW202 #1" && e.text.includes("port 3 changed to DOWN")), "port 3 hook");

  assert.equal((await toggle({ target: "port", device: "TSW202 #1", port: "port3", enabled: true })).status, 200);
  await until((s) => switchPort(s, "port3").linkUp, "port 3 link up");
});

test("streams the full status over Server-Sent Events", async () => {
  const ac = new AbortController();
  const res = await fetch(`${PANEL}/api/events`, { signal: ac.signal });
  assert.match(res.headers.get("content-type") ?? "", /text\/event-stream/);
  const reader = res.body!.getReader();
  let text = "";
  while (!text.includes("\n\n")) text += new TextDecoder().decode((await reader.read()).value);
  ac.abort();
  const data = JSON.parse(text.slice(text.indexOf("data:") + 5, text.indexOf("\n\n")));
  assert.equal(data.router.label, "RUTM50");
  assert.ok(data.controls);
});

test("shows a change as pending until the device reports it", async () => {
  // Watch the stream while the change runs: the server must report pending before the result.
  const ac = new AbortController();
  const res = await fetch(`${PANEL}/api/events`, { signal: ac.signal });
  const reader = res.body!.getReader();
  const seen: boolean[] = [];
  const watch = (async () => {
    let buf = "";
    for (;;) {
      const { value, done } = await reader.read().catch(() => ({ value: undefined, done: true }));
      if (done) return;
      buf += new TextDecoder().decode(value);
      let i;
      while ((i = buf.indexOf("\n\n")) >= 0) {
        const line = buf.slice(0, i).split("\n").find((l) => l.startsWith("data:"));
        buf = buf.slice(i + 2);
        if (line) seen.push(JSON.parse(line.slice(5)).controls.wifi.pending);
      }
    }
  })();
  assert.equal((await toggle({ target: "wifi", enabled: false })).status, 200);
  await until((s) => s.controls.wifi?.on === false && !s.controls.wifi.pending, "Wi-Fi off settled");
  assert.equal((await toggle({ target: "wifi", enabled: true })).status, 200);
  await until((s) => s.controls.wifi?.on === true && !s.controls.wifi.pending, "Wi-Fi on settled");
  ac.abort();
  await watch;
  assert.ok(seen.includes(true), "a pending state was streamed");
});

test("turns cellular data off and on, and receives the mobile hook", async () => {
  assert.equal((await toggle({ target: "cellular", enabled: false })).status, 200);
  const off = await until((s) => !s.router.cellular[0].dataEnabled, "cellular off");
  assert.equal(off.router.cellular[0].dataConnected, false);
  await until((s) => s.events.some((e) => e.device === "RUTM50" && e.text.includes("Mobile data disconnected")), "mobile hook");

  assert.equal((await toggle({ target: "cellular", enabled: true })).status, 200);
  await until((s) => s.router.cellular[0].dataConnected, "cellular connected");
});

test("turns Wi-Fi off and on", async () => {
  assert.equal((await toggle({ target: "wifi", enabled: false })).status, 200);
  const off = await until((s) => s.router.wifiControl?.enabled === false, "Wi-Fi off");
  assert.ok(off.router.wifi.every((w) => !w.up));
  assert.equal((await toggle({ target: "wifi", enabled: true })).status, 200);
  await until((s) => s.router.wifi.every((w) => w.up), "Wi-Fi on");
});

test("keeps the router online while Tailscale is stopped", async () => {
  // A stopped Tailscale reports "" instead of lists; this once took the whole router offline.
  assert.equal((await toggle({ target: "tailscale", enabled: false })).status, 200);
  const off = await until((s) => s.router.tailscale?.enabled === false, "Tailscale off");
  assert.equal(off.router.online, true);
  assert.deepEqual(off.router.tailscale?.addresses, []);
  assert.equal((await toggle({ target: "tailscale", enabled: true })).status, 200);
  await until((s) => s.router.tailscale?.running === true, "Tailscale running");
});

test("rejects invalid changes", async () => {
  assert.deepEqual(await toggle({ target: "port", device: "Nope", port: "port1", enabled: false }), { status: 409, body: { error: "unknown device" } });
  assert.deepEqual(await toggle({ target: "port", device: "TSW202 #1", port: "port99", enabled: false }), { status: 409, body: { error: "unknown port" } });
  assert.equal((await toggle({ target: "reboot", enabled: true })).status, 400);
  const form = await fetch(`${PANEL}/api/toggle`, { method: "POST", body: "target=wifi&enabled=false" });
  assert.equal(form.status, 415);
});

test("accepts hooks only from the devices", async () => {
  const res = await fetch(`${PANEL}/api/hook`, { method: "POST", body: "spoofed" });
  assert.equal(res.status, 403);
});
