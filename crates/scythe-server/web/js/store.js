// Session state shared by the pages: the latest snapshot, transport and meters from the
// server, with hooks that re-render only what listens to each.

import { useEffect, useRef, useState } from "../vendor/preact-htm.js";
import { meterFraction } from "./format.js";

export class Store {
  constructor(client) {
    this.client = client;
    this.snapshot = null;
    this.transport = null;
    this.status = client.status;
    this.meters = { master: [0, 0], tracks: new Map(), mixes: new Map() };
    this.access = client.access;
    this.listeners = { snapshot: new Set(), transport: new Set(), status: new Set(), notice: new Set(), access: new Set() };
    this.fetching = false;
    this.wanted = 0;
    client.on("status", (s) => this.set("status", s));
    client.on("hello", (h) => h.access && this.set("access", h.access));
    // A snapshot fetched on (re)connecting is the truth even when its revision is lower: the
    // server may have restarted.
    client.on("snapshot", (snap) => this.applySnapshot(snap, true));
    client.on("event:transport", (e) => this.set("transport", e));
    client.on("event:meters", (e) => {
      this.meters.master = e.master || [0, 0];
      const tracks = new Map();
      for (const t of e.tracks || []) tracks.set(t.id, t.peak);
      this.meters.tracks = tracks;
      const mixes = new Map();
      for (const m of e.mixes || []) mixes.set(m.id, m.peak);
      this.meters.mixes = mixes;
    });
    client.on("event:project_changed", (e) => {
      if (!this.snapshot || e.revision > this.snapshot.revision) this.refresh(e.revision);
    });
    client.on("event:notice", (e) => this.notify(e.message));
  }

  set(key, value) {
    this[key] = value;
    for (const fn of this.listeners[key]) fn(value);
  }

  on(key, fn) {
    this.listeners[key].add(fn);
    return () => this.listeners[key].delete(fn);
  }

  notify(message, kind = "info") {
    for (const fn of this.listeners.notice) fn({ message, kind, at: Date.now() });
  }

  applySnapshot(snap, force = false) {
    if (!force && this.snapshot && snap.revision < this.snapshot.revision) return;
    if (snap.access) this.set("access", snap.access);
    this.set("snapshot", snap);
    this.set("transport", snap.transport);
  }

  // Fetches a new snapshot, one at a time: changes arriving meanwhile (a fader being dragged)
  // fold into a single fetch after the current one.
  async refresh(revision = Infinity) {
    this.wanted = Math.max(this.wanted, revision);
    if (this.fetching) return;
    this.fetching = true;
    try {
      while (!this.snapshot || this.wanted > this.snapshot.revision) {
        const target = this.wanted;
        const snap = await this.client.send({ cmd: "snapshot" });
        this.applySnapshot(snap);
        if (this.wanted === target) this.wanted = snap.revision;
      }
    } catch {
      // Offline: the reconnect fetches a snapshot anyway.
    } finally {
      this.fetching = false;
    }
  }

  // Runs a command, reporting a failure as a toast and fetching the server's truth so an
  // optimistic change on screen is put right.
  async run(command, { latest = null, quiet = false } = {}) {
    try {
      return await (latest ? this.client.sendLatest(latest, command) : this.client.send(command));
    } catch (e) {
      if (!quiet && e.kind !== "offline") this.notify(e.message, "error");
      this.refresh();
      throw e;
    }
  }

  // Like `run` for fire-and-forget controls: the failure is already reported.
  fire(command, options) {
    this.run(command, options).catch(() => {});
  }

  track(id) {
    if (!this.snapshot) return null;
    if (id === 0) return this.snapshot.master;
    return this.snapshot.tracks.find((t) => t.id === id) || null;
  }

  // Puts `info` (a track as a command answers it) into the snapshot at once, so the change
  // shows before the next snapshot arrives.
  patchTrack(info) {
    if (!this.snapshot || !info || info.id == null) return;
    const snap = this.snapshot;
    const next =
      info.id === 0
        ? { ...snap, master: { ...snap.master, ...info } }
        : { ...snap, tracks: snap.tracks.map((t) => (t.id === info.id ? { ...t, ...info } : t)) };
    this.set("snapshot", next);
  }

  // Puts an item (as `item.*` commands answer it) where it now is; `from` is the track it was
  // on before a move.
  patchItem(info, from = info?.track) {
    if (!this.snapshot || !info || info.id == null) return;
    const tracks = this.snapshot.tracks.map((t) => {
      let items = t.items;
      if (t.id === from || t.id === info.track) items = items.filter((it) => it.id !== info.id);
      if (t.id === info.track) items = [...items, info].sort((a, b) => a.position - b.position);
      return items === t.items ? t : { ...t, items };
    });
    this.set("snapshot", { ...this.snapshot, tracks });
  }
}

export function useStoreValue(store, key) {
  const [value, setValue] = useState(store[key]);
  useEffect(() => {
    setValue(store[key]);
    return store.on(key, setValue);
  }, [store, key]);
  return value;
}

// Drives meter bars outside of rendering: one animation frame loop writes the heights of every
// mounted meter, so 20 updates a second never re-render a mixer.
class MeterBus {
  constructor() {
    this.meters = new Set();
    this.running = false;
  }

  add(entry) {
    this.meters.add(entry);
    if (!this.running) {
      this.running = true;
      requestAnimationFrame(() => this.frame());
    }
    return () => this.meters.delete(entry);
  }

  frame() {
    if (this.meters.size === 0) {
      this.running = false;
      return;
    }
    for (const { el, read } of this.meters) {
      const peaks = read() || [0, 0];
      const axis = el.dataset.axis === "x" ? "scaleX" : "scaleY";
      const bars = el.children;
      for (let i = 0; i < bars.length; i++) {
        const peak = peaks[Math.min(i, peaks.length - 1)] || 0;
        const fill = bars[i].firstElementChild;
        if (!fill) continue;
        fill.style.transform = `${axis}(${meterFraction(peak)})`;
        fill.classList.toggle("hot", peak >= 0.89);
      }
    }
    requestAnimationFrame(() => this.frame());
  }
}

const bus = new MeterBus();

// A ref for a `.meter` element whose bars follow `read()` (`[left, right]` peaks).
export function useMeter(read) {
  const ref = useRef(null);
  const readRef = useRef(read);
  readRef.current = read;
  useEffect(() => {
    if (!ref.current) return;
    return bus.add({ el: ref.current, read: () => readRef.current() });
  }, []);
  return ref;
}
