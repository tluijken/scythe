// The monitor-mix (`mix.*`) and setlist (`setlist.*`) domains: whether the server has them,
// and their answers in one shape for the pages. The command and field names live only here.

import { gainToDb } from "./format.js";

// Whether a probe's error means the server does not know the domain (an older Scythe).
export function missingDomain(error) {
  return error?.kind === "invalid" || error?.kind === "not_found";
}

// Asks `probe` once and caches whether the domain is there; network trouble is not cached.
export function detector(client, probe) {
  let known = null;
  return async () => {
    if (known !== null) return known;
    try {
      await client.send(probe);
      known = true;
    } catch (e) {
      if (missingDomain(e)) known = false;
      else return null;
    }
    return known;
  };
}

const first = (...values) => values.find((v) => v !== undefined && v !== null);

const listOf = (result, key) => (Array.isArray(result) ? result : first(result?.[key], []));

function levelDb(s) {
  const db = first(s.volume_db, s.db, s.level_db);
  if (db !== undefined) return db;
  return gainToDb(first(s.volume, s.level, 1));
}

export function normalizeSource(s) {
  return {
    id: first(s.track, s.source, s.id),
    name: first(s.name, s.track_name, ""),
    color: first(s.color, null),
    db: levelDb(s),
    pan: first(s.pan, 0),
    muted: !!s.muted,
    own: !!s.own,
  };
}

export function normalizeMix(m) {
  const sources = (m.sources || []).map(normalizeSource);
  const ownIds = new Set(m.own || []);
  for (const s of sources) s.own = s.own || ownIds.has(s.id);
  return {
    id: first(m.mix, m.id, m.track),
    name: first(m.name, m.musician, "Mix"),
    color: first(m.color, null),
    moreMeDb: first(m.more_me_db, m.more_me, 0),
    output: first(m.output_label, m.output, null),
    db: levelDb(m),
    sources,
  };
}

export function normalizeMixes(result) {
  return listOf(result, "mixes").map(normalizeMix);
}

// Finds the remembered mix again: by id while the session runs, by name after a reopen
// (ids are not stable across opening the project again).
export function pickMix(mixes, remembered) {
  if (!remembered) return null;
  return mixes.find((m) => m.id === remembered.id && m.name === remembered.name) || mixes.find((m) => m.name === remembered.name) || null;
}

export const MORE_ME_RANGE = [-12, 12];

export const mixCommands = {
  list: () => ({ cmd: "mix.list" }),
  level: (mix, source, db) => ({ cmd: "mix.level", mix, source, db }),
  pan: (mix, source, pan) => ({ cmd: "mix.pan", mix, source, pan }),
  mute: (mix, source, muted) => ({ cmd: "mix.mute", mix, source, muted }),
  moreMe: (mix, db) => ({ cmd: "mix.more_me", mix, db }),
};

// The mix bus's peaks: a `mix_meters` event when the server sends one, else the bus's track
// meter (a monitor mix is a track).
export function mixPeak(store, mixMeters, id) {
  return mixMeters.get(id) || store.meters.tracks.get(id) || [0, 0];
}

export function normalizeMixMeters(event) {
  const map = new Map();
  for (const m of first(event.mixes, event.meters, [])) map.set(first(m.mix, m.id), first(m.peak, [0, 0]));
  return map;
}

export function normalizeSong(s, index) {
  const start = first(s.start, s.position, 0);
  const end = first(s.end, null);
  return {
    index,
    number: first(s.number, index + 1),
    name: first(s.name, `Song ${index + 1}`),
    start,
    end,
    duration: first(s.duration, end != null ? end - start : null),
    sections: (s.sections || []).map((x) => (Array.isArray(x) ? { name: x[0], position: x[1] } : { name: x.name, position: first(x.position, x.start) })),
    missing: !!s.missing,
  };
}

export function normalizeSetlists(result) {
  const lists = listOf(result, "setlists").map((l, i) => ({
    index: first(l.index, i),
    name: first(l.name, `Setlist ${i + 1}`),
    songs: (l.songs || l.entries || []).map(normalizeSong),
  }));
  return {
    lists,
    active: first(result?.active, result?.current, 0),
    autoAdvance: !!first(result?.auto_advance, false),
  };
}

const entryIndex = (v) => (v && typeof v === "object" ? first(v.entry, v.index) : v);

export function normalizeStatus(s) {
  if (!s) return null;
  return {
    setlist: first(s.setlist, s.active, 0),
    current: first(entryIndex(s.current), null),
    next: first(entryIndex(s.next), null),
    elapsed: first(s.elapsed, null),
    remaining: first(s.remaining, null),
    playing: !!s.playing,
    autoAdvance: s.auto_advance,
  };
}

// Elapsed and remaining seconds of `song` at transport `position`, for a smooth clock between
// status updates.
export function songProgress(song, position) {
  if (!song) return { elapsed: null, remaining: null, fraction: 0 };
  const elapsed = Math.max(0, position - song.start);
  const duration = song.duration ?? null;
  if (duration == null || duration <= 0) return { elapsed, remaining: null, fraction: 0 };
  const clamped = Math.min(elapsed, duration);
  return { elapsed: clamped, remaining: duration - clamped, fraction: clamped / duration };
}

export const setlistCommands = {
  list: () => ({ cmd: "setlist.list" }),
  status: () => ({ cmd: "setlist.status" }),
  use: (setlist) => ({ cmd: "setlist.select", setlist }),
  cue: (entry) => ({ cmd: "setlist.cue", entry }),
  play: () => ({ cmd: "setlist.play" }),
  stop: () => ({ cmd: "setlist.stop" }),
  next: () => ({ cmd: "setlist.next" }),
  previous: () => ({ cmd: "setlist.previous" }),
  autoAdvance: (enabled) => ({ cmd: "setlist.auto_advance", enabled }),
};

export const isSetlistEvent = (name) => typeof name === "string" && name.startsWith("setlist");
export const isMixEvent = (name) => typeof name === "string" && name.startsWith("mix") && name !== "mix_meters";
