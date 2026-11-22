// Creates the Event Juggler rules that notify the panel about port and mobile data events.
// Run with `npm run setup-hooks`. It replaces rules named panel_* and leaves other rules alone.
// Devices that do not answer are skipped; run it again once they are installed.
import { router, switches } from "./config.ts";

const hookUrl = process.env.PANEL_HOOK_URL;
if (!hookUrl) throw new Error("Set PANEL_HOOK_URL, e.g. http://192.168.30.180:31893/api/hook");

interface JugglerEvent {
  id: string;
  name: string;
  actions?: string[];
}

// Event log types from GET /event_juggler/events/options on each device.
const portRule = { name: "panel_ports", log_event: "Switch Events" };
const devices = [
  { ...router, rules: [portRule, { name: "panel_mobile", log_event: "Mobile Data" }] },
  ...switches.map((s) => ({ ...s, rules: [portRule] })),
];

let failed = 0;
for (const { label, client, rules } of devices) {
  try {
    const existing = await client.call<JugglerEvent[]>("GET", "/event_juggler/events/config");
    for (const e of existing.filter((e) => e.name.startsWith("panel_"))) {
      await client.call("DELETE", `/event_juggler/events/config/${e.id}`);
      console.log(`${label} (${client.host}): deleted old rule ${e.name}`);
    }
    for (const rule of rules) {
      // Creating an event also creates its first action, which is then set to an HTTP POST.
      const event = await client.call<JugglerEvent>("POST", "/event_juggler/events/config", {
        name: rule.name,
        enabled: "1",
        plugin: "log",
        log_event: rule.log_event,
        log_event_mark: "all",
      });
      const actionId = event.actions?.[0];
      if (!actionId) throw new Error(`rule ${rule.name} has no action`);
      await client.call("PUT", `/event_juggler/operations/config/${actionId}`, {
        plugin: "http",
        http_post: "1",
        http_url: hookUrl,
        http_ui_params: "0",
        // %et is the event type and %ex the event text, e.g. "Port LAN2 link state changed to UP".
        http_text: "%et: %ex",
        http_timeout: "5",
      });
      console.log(`${label} (${client.host}): created ${rule.name} (${rule.log_event}) -> POST ${hookUrl}`);
    }
  } catch (e) {
    failed++;
    console.log(`${label} (${client.host}): skipped, ${(e as Error).message}`);
  }
}
process.exitCode = failed ? 1 : 0;
