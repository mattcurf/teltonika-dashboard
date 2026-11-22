// Minimal client for the Teltonika Networks Web API (RutOS / TSWOS).
// https://developers.teltonika-networks.com/
import { request } from "node:https";

interface ApiEnvelope<T> {
  success: boolean;
  data: T;
  errors?: { code: number; error: string; source: string }[];
}

/** Results of one poll, keyed by endpoint path. */
export class PollResults {
  private readonly items: Map<string, ApiEnvelope<unknown>>;

  constructor(items: Map<string, ApiEnvelope<unknown>>) {
    this.items = items;
  }

  /** Returns the endpoint's data, or throws its API error. */
  get<T>(path: string): T {
    const item = this.items.get(path);
    if (!item?.success) throw new Error(`${path}: ${item ? apiError(item) : "not requested"}`);
    return item.data as T;
  }

  /** Returns the endpoint's data, or null when the endpoint failed, e.g. an optional package is missing. */
  optional<T>(path: string): T | null {
    const item = this.items.get(path);
    return item?.success ? (item.data as T) : null;
  }
}

export class TeltonikaClient {
  readonly host: string;
  private readonly username: string;
  private readonly password: string;
  private token: string | null = null;
  private pendingLogin: Promise<string> | null = null;
  // Last successful response per path, for endpoints polled with a max age.
  private readonly cache = new Map<string, { at: number; item: ApiEnvelope<unknown> }>();

  constructor(host: string, username: string, password: string) {
    this.host = host;
    this.username = username;
    this.password = password;
  }

  /**
   * Fetches endpoints in one POST /api/bulk request. `paths` maps each endpoint to the
   * maximum age in ms of a cached result; 0 always fetches. The device runs bulk
   * sub-requests one after another, so a poll costs it one connection and one auth check.
   */
  async poll(paths: Record<string, number>): Promise<PollResults> {
    const now = Date.now();
    const results = new Map<string, ApiEnvelope<unknown>>();
    const due: string[] = [];
    for (const [path, maxAge] of Object.entries(paths)) {
      const hit = this.cache.get(path);
      if (hit && now - hit.at < maxAge) results.set(path, hit.item);
      else due.push(path);
    }
    if (due.length) {
      const items = await this.bulk(due);
      due.forEach((path, i) => {
        const item = items[i] ?? { success: false, data: null, errors: [{ code: 0, error: "missing in bulk response", source: "Request" }] };
        results.set(path, item);
        if (item.success && paths[path] > 0) this.cache.set(path, { at: now, item });
      });
    }
    return new PollResults(results);
  }

  /**
   * Sends one API request and returns its data, or throws the API error.
   * Requests other than GET change device state, so they also drop cached results.
   */
  async call<T>(method: "GET" | "POST" | "PUT" | "DELETE", path: string, data?: unknown): Promise<T> {
    const res = await this.authed<T>(method, path, data === undefined ? undefined : { data });
    if (method !== "GET") this.cache.clear();
    if (!res.body.success) throw new Error(`${method} ${path}: ${apiError(res.body)}`);
    return res.body.data;
  }

  private async bulk(paths: string[]): Promise<ApiEnvelope<unknown>[]> {
    const body = { data: paths.map((p) => ({ endpoint: `/api${p}`, method: "GET" })) };
    const res = await this.authed<ApiEnvelope<unknown>[]>("POST", "/bulk", body);
    if (!res.body.success || !Array.isArray(res.body.data)) throw new Error(`bulk: ${apiError(res.body)}`);
    return res.body.data;
  }

  // Sends a request with the session token, logging in again once when the device rejects it.
  private async authed<T>(method: string, path: string, body: unknown): Promise<{ status: number; body: ApiEnvelope<T> }> {
    for (let attempt = 0; ; attempt++) {
      const token = await this.session();
      const res = await this.send<T>(method, path, body, token);
      // A rejected token still returns HTTP 207; each item then fails with an Authorization
      // error (121 "Missing bearer token" or 123 "Expired bearer token").
      const expired =
        res.status === 401 ||
        authFailed(res.body) ||
        (Array.isArray(res.body.data) && (res.body.data as ApiEnvelope<unknown>[]).some(authFailed));
      if (expired && attempt === 0) {
        if (this.token === token) this.token = null;
        continue;
      }
      return res;
    }
  }

  // Callers that need a token at the same time share one login request.
  private session(): Promise<string> {
    if (this.token) return Promise.resolve(this.token);
    this.pendingLogin ??= this.login().finally(() => (this.pendingLogin = null));
    return this.pendingLogin;
  }

  private async login(): Promise<string> {
    const res = await this.send<{ token: string }>("POST", "/login", { username: this.username, password: this.password }, null);
    if (!res.body.success) throw new Error(`login failed: ${apiError(res.body)}`);
    this.token = res.body.data.token;
    return this.token;
  }

  private send<T>(method: string, path: string, body: unknown, token: string | null): Promise<{ status: number; body: ApiEnvelope<T> }> {
    return new Promise((resolve, reject) => {
      const req = request(
        {
          host: this.host,
          path: `/api${path}`,
          method,
          // The devices ship self-signed certificates.
          rejectUnauthorized: false,
          // Idle-socket timeout; the deadline below caps the whole request.
          timeout: 10_000,
          headers: {
            "Content-Type": "application/json",
            ...(token ? { Authorization: `Bearer ${token}` } : {}),
          },
        },
        (res) => {
          let text = "";
          res.setEncoding("utf8");
          res.on("data", (chunk: string) => (text += chunk));
          res.on("end", () => {
            try {
              resolve({ status: res.statusCode ?? 0, body: JSON.parse(text) });
            } catch {
              reject(new Error(`${path}: HTTP ${res.statusCode}, non-JSON response`));
            }
          });
        },
      );
      const deadline = setTimeout(() => req.destroy(new Error(`${path}: timed out`)), 20_000);
      req.on("close", () => clearTimeout(deadline));
      req.on("timeout", () => req.destroy(new Error(`${path}: timed out`)));
      req.on("error", reject);
      req.end(body === undefined ? undefined : JSON.stringify(body));
    });
  }
}

function authFailed(body: ApiEnvelope<unknown> | undefined): boolean {
  return body?.errors?.some((e) => e.source === "Authorization") ?? false;
}

function apiError(body: ApiEnvelope<unknown>): string {
  return body.errors?.map((e) => e.error).join("; ") || "unknown error";
}
