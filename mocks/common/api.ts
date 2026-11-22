// Shared core of the device mocks. It speaks the Teltonika Web API over HTTPS the way the
// real devices do: login with bearer tokens, single requests, POST /api/bulk (HTTP 207),
// config lists, and the Event Juggler, whose HTTP actions post to the panel's hook.
import { createServer } from "node:https";
import type { IncomingMessage, ServerResponse } from "node:http";
import { readFileSync } from "node:fs";
import { randomBytes } from "node:crypto";

export type Item = Record<string, unknown> & { id: string };

// An API error, sent as {success: false, errors: [...]}. Codes match the devices.
export class ApiError extends Error {
  readonly status: number;
  readonly code: number;
  readonly source: string;
  constructor(status: number, code: number, message: string, source = "Request") {
    super(message);
    this.status = status;
    this.code = code;
    this.source = source;
  }
}
const notFound = () => new ApiError(404, 122, "Endpoint not implemented");

type Handler = (data: unknown, id: string | undefined) => unknown;
interface Route {
  method: string;
  path: string;
  // True when the route ends in "/:id".
  withId: boolean;
  handler: Handler;
}

export function env(name: string, fallback: string): string {
  return process.env[name] || fallback;
}

export class MockDevice {
  readonly name: string;
  private readonly routes: Route[] = [];
  private readonly tokens = new Map<string, number>();
  private readonly username = env("MOCK_USERNAME", "admin");
  private readonly password = env("MOCK_PASSWORD", "admin01");
  private readonly tokenTtl = Number(env("MOCK_TOKEN_TTL", "299"));
  readonly juggler: EventJuggler;

  constructor(name: string) {
    this.name = name;
    this.juggler = new EventJuggler(this);
  }

  on(method: string, path: string, handler: Handler): void {
    const withId = path.endsWith("/:id");
    this.routes.push({ method, path: withId ? path.slice(0, -4) : path, withId, handler });
  }

  // A list of config sections, such as /ports_settings/config. GET reads the list or one
  // item; PUT /:id merges one item; PUT on the list merges an array of items by id.
  // onChange runs after each changed item with the values it had before.
  configList(path: string, items: Item[], onChange?: (item: Item, before: Item) => void): void {
    const merge = (id: string | undefined, values: unknown): Item => {
      const item = items.find((i) => i.id === id);
      if (!item) throw new ApiError(404, 122, `Section ${id} not found`);
      if (!values || typeof values !== "object") throw new ApiError(400, 101, "data must be an object");
      const before = { ...item };
      Object.assign(item, values, { id: item.id });
      onChange?.(item, before);
      return item;
    };
    this.on("GET", path, () => items);
    this.on("GET", `${path}/:id`, (_, id) => {
      const item = items.find((i) => i.id === id);
      if (!item) throw new ApiError(404, 122, `Section ${id} not found`);
      return item;
    });
    this.on("PUT", `${path}/:id`, (data, id) => merge(id, data));
    this.on("PUT", path, (data) => {
      if (!Array.isArray(data)) throw new ApiError(400, 101, "data must be an array");
      return data.map((d: { id?: string }) => merge(d.id, d));
    });
  }

  private dispatch(method: string, endpoint: string, data: unknown): unknown {
    const path = endpoint.replace(/^\/api/, "").replace(/\/$/, "");
    for (const r of this.routes) {
      if (r.method !== method) continue;
      if (!r.withId && path === r.path) return r.handler(data, undefined);
      if (r.withId && path.startsWith(`${r.path}/`) && !path.slice(r.path.length + 1).includes("/")) {
        return r.handler(data, decodeURIComponent(path.slice(r.path.length + 1)));
      }
    }
    throw notFound();
  }

  private checkToken(req: IncomingMessage): void {
    const header = req.headers.authorization ?? "";
    if (!header.startsWith("Bearer ")) throw new ApiError(401, 121, "Missing bearer token", "Authorization");
    const expires = this.tokens.get(header.slice(7));
    if (!expires || expires < Date.now()) throw new ApiError(401, 123, "Expired bearer token", "Authorization");
  }

  private login(data: unknown): unknown {
    const { username, password } = (data ?? {}) as { username?: string; password?: string };
    if (username !== this.username || password !== this.password) {
      throw new ApiError(401, 120, "Invalid username and/or password! Please try again.", "Authorization");
    }
    const token = randomBytes(16).toString("hex");
    this.tokens.set(token, Date.now() + this.tokenTtl * 1000);
    return { token, group: "root", expires: this.tokenTtl, username };
  }

  private handle(req: IncomingMessage, text: string): { status: number; body: unknown } {
    const path = new URL(req.url ?? "/", "https://x").pathname;
    const method = req.method ?? "GET";
    let body: { data?: unknown } & Record<string, unknown> = {};
    if (text) {
      try {
        body = JSON.parse(text);
      } catch {
        throw new ApiError(400, 100, "Response not implemented");
      }
    }
    // Login takes its fields at the top level, every other request inside "data".
    if (path === "/api/login" && method === "POST") return { status: 200, body: { success: true, data: this.login(body) } };
    this.checkToken(req);
    if (path === "/api/bulk" && method === "POST") {
      // Each sub-request has its own result; the bulk request itself succeeds with HTTP 207.
      const items = Array.isArray(body.data) ? (body.data as { endpoint?: string; method?: string; data?: unknown }[]) : [];
      const results = items.map((i) => {
        try {
          return { success: true, data: this.dispatch(i.method ?? "GET", i.endpoint ?? "", i.data) };
        } catch (e) {
          return failure(e);
        }
      });
      return { status: 207, body: { success: true, data: results } };
    }
    return { status: 200, body: { success: true, data: this.dispatch(method, path, body.data) } };
  }

  listen(): void {
    const tls = env("MOCK_TLS_DIR", "/app/tls");
    const server = createServer({ cert: readFileSync(`${tls}/cert.pem`), key: readFileSync(`${tls}/key.pem`) }, (req, res) => {
      let text = "";
      req.setEncoding("utf8");
      req.on("data", (c: string) => (text += c));
      req.on("end", () => {
        let reply: { status: number; body: unknown };
        try {
          reply = this.handle(req, text);
        } catch (e) {
          reply = { status: e instanceof ApiError ? e.status : 500, body: failure(e) };
        }
        send(res, reply.status, reply.body);
      });
    });
    const port = Number(env("MOCK_PORT", "443"));
    server.listen(port, () => console.log(`${this.name} mock on https://0.0.0.0:${port}`));
  }
}

function failure(e: unknown): unknown {
  const err = e instanceof ApiError ? e : new ApiError(500, 0, String((e as Error)?.message ?? e));
  return { success: false, errors: [{ code: err.code, error: err.message, source: err.source }] };
}

function send(res: ServerResponse, status: number, body: unknown): void {
  // The devices close every connection; the panel relies on that.
  res.writeHead(status, { "Content-Type": "application/json", Connection: "close" });
  res.end(JSON.stringify(body));
}

// Event Juggler: events that watch a log type and actions that run when one fires.
// Only the HTTP action is simulated: it POSTs http_text, with %et and %ex filled in.
export class EventJuggler {
  private readonly events: Item[] = [];
  private readonly operations: Item[] = [];
  private nextId = 1;

  constructor(device: MockDevice) {
    device.on("GET", "/event_juggler/events/config", () => this.events);
    device.on("POST", "/event_juggler/events/config", (data) => this.create(data as Record<string, unknown>));
    device.on("DELETE", "/event_juggler/events/config/:id", (_, id) => {
      const i = this.events.findIndex((e) => e.id === id);
      if (i < 0) throw new ApiError(404, 122, `Section ${id} not found`);
      const [event] = this.events.splice(i, 1);
      for (const a of event.actions as string[]) {
        const j = this.operations.findIndex((o) => o.id === a);
        if (j >= 0) this.operations.splice(j, 1);
      }
      return { id };
    });
    device.configList("/event_juggler/operations/config", this.operations);
  }

  // Creating an event also creates its first action, as on the devices.
  private create(data: Record<string, unknown>): Item {
    const id = String(this.nextId++);
    const actionId = String(this.nextId++);
    this.operations.push({ id: actionId, ".type": "operation", enabled: "1", plugin: "" });
    const event: Item = { enabled: "1", ...data, id, ".type": "event", actions: [actionId] };
    this.events.push(event);
    return event;
  }

  // Creates the panel_* rules that `npm run setup-hooks` would create, for a ready-to-use stack.
  seed(hookUrl: string, logEvents: Record<string, string>): void {
    for (const [name, logEvent] of Object.entries(logEvents)) {
      const event = this.create({ name, plugin: "log", log_event: logEvent, log_event_mark: "all" });
      const op = this.operations.find((o) => o.id === (event.actions as string[])[0])!;
      Object.assign(op, { plugin: "http", http_post: "1", http_url: hookUrl, http_text: "%et: %ex", http_timeout: "5" });
    }
  }

  // Runs the HTTP actions of every enabled event that watches this log type.
  fire(logEvent: string, text: string): void {
    for (const e of this.events.filter((e) => e.enabled === "1" && e.log_event === logEvent)) {
      for (const op of this.operations.filter((o) => (e.actions as string[]).includes(o.id))) {
        if (op.plugin !== "http" || typeof op.http_url !== "string") continue;
        const body = String(op.http_text ?? "").replaceAll("%et", logEvent).replaceAll("%ex", text);
        fetch(op.http_url, { method: op.http_post === "1" ? "POST" : "GET", body, signal: AbortSignal.timeout(5000) }).catch((err) =>
          console.log(`event juggler: ${op.http_url}: ${(err as Error).message}`),
        );
      }
    }
  }
}

// "lan1=AA:..,BB:..;wan=CC:.." -> {lan1: [...], wan: [...]}. Port names are case-insensitive.
export function parseNeighbors(spec: string): Record<string, string[]> {
  const out: Record<string, string[]> = {};
  for (const part of spec.split(";").filter(Boolean)) {
    const [port, macs = ""] = part.split("=");
    out[port.trim().toLowerCase()] = macs.split(",").map((m) => m.trim().toUpperCase()).filter(Boolean);
  }
  return out;
}

// Random walk for live-looking values, kept between min and max.
export function drift(value: number, step: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value + (Math.random() * 2 - 1) * step));
}
