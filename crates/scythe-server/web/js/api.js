// The control API client: commands over the WebSocket (HTTP while it is down), events to
// listeners, and reconnecting with backoff. See docs/control-api.md.

const TOKEN_KEY = "scythe.token";

export class ApiError extends Error {
  constructor(kind, message) {
    super(message);
    this.kind = kind;
  }
}

// Delay before reconnect attempt `attempt` (0-based): doubling from 250 ms to 8 s, with up to
// 25% jitter so a room full of phones does not reconnect in step after a restart.
export function backoffDelay(attempt, random = Math.random) {
  const base = Math.min(8000, 250 * Math.pow(2, attempt));
  return Math.round(base * (1 + 0.25 * random()));
}

export function storedToken(storage = globalThis.localStorage) {
  try {
    return storage?.getItem(TOKEN_KEY) || null;
  } catch {
    return null;
  }
}

export function storeToken(token, storage = globalThis.localStorage) {
  try {
    if (token) storage?.setItem(TOKEN_KEY, token);
    else storage?.removeItem(TOKEN_KEY);
  } catch {
    // Private browsing: the token then lasts until the page is closed.
  }
}

// A token passed in the address (`?token=…`, handy for a QR code on stage) is remembered and
// taken out of the address bar.
export function adoptTokenFromUrl(loc = globalThis.location, hist = globalThis.history, storage) {
  const url = new URL(loc.href);
  const token = url.searchParams.get("token");
  if (!token) return null;
  storeToken(token, storage);
  url.searchParams.delete("token");
  hist?.replaceState(null, "", url.pathname + url.search + url.hash);
  return token;
}

export function wsUrl(base, token) {
  const url = new URL("/api/ws", base);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  if (token) url.searchParams.set("token", token);
  return url.toString();
}

export class Client {
  constructor({ base = globalThis.location?.origin, token = null, WebSocketImpl, fetchImpl, setTimer, clearTimer } = {}) {
    this.base = base;
    this.token = token;
    this.WebSocket = WebSocketImpl || globalThis.WebSocket;
    this.fetch = fetchImpl || globalThis.fetch?.bind(globalThis);
    this.setTimer = setTimer || ((f, ms) => setTimeout(f, ms));
    this.clearTimer = clearTimer || ((t) => clearTimeout(t));
    this.status = "idle";
    this.listeners = new Map();
    this.pending = new Map();
    this.nextId = 1;
    this.attempt = 0;
    this.ws = null;
    this.timer = null;
    this.stopped = true;
    this.coalesced = new Map();
    // What the token may do (`{"role": "admin" | "musician" | "stage", "musician"?}`), once known.
    this.access = null;
  }

  on(type, fn) {
    if (!this.listeners.has(type)) this.listeners.set(type, new Set());
    this.listeners.get(type).add(fn);
    return () => this.listeners.get(type)?.delete(fn);
  }

  emit(type, value) {
    for (const fn of this.listeners.get(type) || []) fn(value);
  }

  setStatus(status) {
    if (status === this.status) return;
    this.status = status;
    this.emit("status", status);
  }

  headers() {
    const h = { "Content-Type": "application/json" };
    if (this.token) h.Authorization = `Bearer ${this.token}`;
    return h;
  }

  async http(method, path, body) {
    let response;
    try {
      response = await this.fetch(new URL(path, this.base).toString(), {
        method,
        headers: this.headers(),
        body: body === undefined ? undefined : JSON.stringify(body),
        cache: "no-store",
      });
    } catch (e) {
      throw new ApiError("offline", "Scythe cannot be reached");
    }
    let json = null;
    try {
      json = await response.json();
    } catch {
      // A proxy error page or an empty body: the status says enough.
    }
    if (response.status === 401) {
      this.setStatus("unauthorized");
      throw new ApiError("unauthorized", json?.error?.message || "a valid token is required");
    }
    if (!json) throw new ApiError("failed", `HTTP ${response.status}`);
    if (!json.ok) throw new ApiError(json.error?.kind || "failed", json.error?.message || "failed");
    return json.result;
  }

  health() {
    return this.fetch(new URL("/api/health", this.base).toString(), { cache: "no-store" }).then((r) => r.json());
  }

  // Starts connecting; the snapshot is fetched first so a wrong token shows as
  // `unauthorized` (a refused WebSocket upgrade looks the same as no server at all).
  start() {
    this.stopped = false;
    this.connect();
  }

  stop() {
    this.stopped = true;
    if (this.timer) this.clearTimer(this.timer);
    this.timer = null;
    const ws = this.ws;
    this.ws = null;
    if (ws) ws.close();
    this.failPending("offline", "disconnected");
    this.setStatus("idle");
  }

  setToken(token) {
    this.token = token;
  }

  async connect() {
    if (this.stopped) return;
    this.setStatus(this.attempt === 0 ? "connecting" : "reconnecting");
    try {
      const snapshot = await this.http("GET", "/api/snapshot");
      if (this.stopped) return;
      if (snapshot.access) this.access = snapshot.access;
      this.emit("snapshot", snapshot);
    } catch (e) {
      if (e.kind === "unauthorized") return;
      return this.retry();
    }
    let ws;
    try {
      ws = new this.WebSocket(wsUrl(this.base, this.token));
    } catch {
      return this.retry();
    }
    this.ws = ws;
    ws.onmessage = (m) => this.message(m.data);
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.failPending("offline", "the connection to Scythe was lost");
      this.retry();
    };
    ws.onerror = () => {};
  }

  retry() {
    if (this.stopped) return;
    this.setStatus("reconnecting");
    const delay = backoffDelay(this.attempt);
    this.attempt += 1;
    if (this.timer) this.clearTimer(this.timer);
    this.timer = this.setTimer(() => {
      this.timer = null;
      this.connect();
    }, delay);
  }

  message(text) {
    let msg;
    try {
      msg = JSON.parse(text);
    } catch {
      return;
    }
    if (msg.type === "hello") {
      this.attempt = 0;
      if (msg.access) this.access = msg.access;
      this.setStatus("open");
      this.emit("hello", msg);
    } else if (msg.type === "reply") {
      const waiting = this.pending.get(msg.id);
      if (!waiting) return;
      this.pending.delete(msg.id);
      if (msg.ok) waiting.resolve(msg.result);
      else waiting.reject(new ApiError(msg.error?.kind || "failed", msg.error?.message || "failed"));
    } else if (msg.type === "event") {
      this.emit("event", msg);
      this.emit(`event:${msg.event}`, msg);
    }
  }

  failPending(kind, message) {
    for (const waiting of this.pending.values()) waiting.reject(new ApiError(kind, message));
    this.pending.clear();
  }

  // Runs one command; resolves with its result or rejects with an ApiError.
  send(command) {
    if (this.ws && this.status === "open") {
      const id = this.nextId++;
      return new Promise((resolve, reject) => {
        this.pending.set(id, { resolve, reject });
        try {
          this.ws.send(JSON.stringify({ ...command, id }));
        } catch {
          this.pending.delete(id);
          reject(new ApiError("offline", "the connection to Scythe was lost"));
        }
      });
    }
    return this.http("POST", "/api/command", command);
  }

  // For continuous controls: while a command for `key` is on its way, later ones replace each
  // other and only the last is sent once it is answered, so a fader dragged on a phone over
  // wifi never builds a queue.
  sendLatest(key, command) {
    const busy = this.coalesced.get(key);
    if (busy) {
      busy.next = command;
      return busy.done;
    }
    const slot = { next: null, done: null };
    const loop = async (cmd) => {
      try {
        for (;;) {
          const result = await this.send(cmd);
          if (!slot.next) return result;
          cmd = slot.next;
          slot.next = null;
        }
      } finally {
        this.coalesced.delete(key);
      }
    };
    this.coalesced.set(key, slot);
    slot.done = loop(command);
    return slot.done;
  }
}
