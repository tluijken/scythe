// The monitor-mix (`mix.*`) and setlist (`setlist.*`) domains (docs/control-api.md): whether
// the server has them, their commands, and their answers in the shape the pages use.

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

export function normalizeMix(m) {
  return {
    id: m.id,
    name: m.musician || m.name,
    color: m.color ?? null,
    moreMeDb: m.more_me ?? 0,
    output: m.output?.label ?? null,
    db: m.volume_db,
    // The metronome's level in the mix in dB, null when the mix has no click.
    clickDb: m.click > 0 ? 20 * Math.log10(m.click) : null,
    sources: (m.sources || []).map((s) => ({
      id: s.track,
      name: s.name || "Unnamed track",
      color: s.color ?? null,
      db: s.volume_db,
      pan: s.pan,
      muted: !!s.muted,
      own: !!s.own,
    })),
  };
}

export function normalizeMixes(result) {
  return (result || []).map(normalizeMix);
}

// Finds the remembered mix again: by id while the session runs, by musician after a reopen
// (ids are not stable across opening the project again).
export function pickMix(mixes, remembered) {
  if (!remembered) return null;
  return mixes.find((m) => m.id === remembered.id && m.name === remembered.name) || mixes.find((m) => m.name === remembered.name) || null;
}

export const MORE_ME_RANGE = [-24, 24];

export const mixCommands = {
  list: () => ({ cmd: "mix.list" }),
  level: (mix, source, db) => ({ cmd: "mix.level", mix, source, db }),
  pan: (mix, source, pan) => ({ cmd: "mix.pan", mix, source, pan }),
  mute: (mix, source, muted) => ({ cmd: "mix.mute", mix, source, muted }),
  moreMe: (mix, db) => ({ cmd: "mix.more_me", mix, db }),
  click: (mix, db) => ({ cmd: "mix.click", mix, db }),
  clickOff: (mix) => ({ cmd: "mix.click", mix, volume: 0 }),
};

// Joins `setlist.list` with `setlist.songs`: each entry gets its song's start, end and cues.
export function normalizeSetlists(list, songs) {
  const lists = (list?.setlists || []).map((l) => ({
    index: l.index,
    name: l.name,
    countIn: !!l.count_in,
    songs: l.entries.map((e) => {
      const song = e.song != null ? songs[e.song] : null;
      return {
        name: e.name,
        number: song?.number ?? null,
        start: song?.start ?? null,
        end: song?.end ?? null,
        duration: e.duration ?? song?.duration ?? null,
        sections: song?.sections || [],
        missing: !song,
      };
    }),
  }));
  return { lists, active: list?.current ?? null, autoAdvance: !!list?.auto_advance };
}

export function normalizeStatus(s) {
  if (!s) return null;
  return {
    setlist: s.setlist ?? null,
    current: s.current?.entry ?? null,
    next: s.next?.entry ?? null,
    elapsed: s.elapsed ?? null,
    remaining: s.remaining ?? null,
    playing: !!s.playing,
    autoAdvance: !!s.auto_advance,
    countIn: !!s.count_in,
  };
}

// A running count-in from the transport state: "3 / 4", or null when none runs.
export function countInLabel(transport) {
  const c = transport?.count_in;
  if (!c || c.position == null || c.beat == null) return null;
  return `${c.beat} / ${c.beats}`;
}

// The health event as a status line: rate, buffer, CPU and xruns, and whether to worry.
export function healthLine(h) {
  if (!h) return null;
  const rate = h.sample_rate % 1000 === 0 ? `${h.sample_rate / 1000}k` : `${(h.sample_rate / 1000).toFixed(1)}k`;
  const parts = [rate, h.block_size ?? "auto", `CPU ${Math.round(h.cpu)}%`];
  if (h.xruns > 0) parts.push(`${h.xruns} xruns`);
  if (h.status !== "running") parts.push("no audio device");
  const warn = h.status !== "running" || h.cpu_peak >= 80;
  return { text: parts.join(" · "), warn, title: h.reason || `${h.device}, output latency ${h.output_latency_ms} ms, peak CPU ${h.cpu_peak}%` };
}

// Elapsed and remaining seconds of `song` at transport `position`, for a clock that runs
// smoothly between status updates.
export function songProgress(song, position) {
  if (!song || song.start == null) return { elapsed: null, remaining: null, fraction: 0 };
  const elapsed = Math.max(0, position - song.start);
  const duration = song.duration;
  if (duration == null || duration <= 0) return { elapsed, remaining: null, fraction: 0 };
  const clamped = Math.min(elapsed, duration);
  return { elapsed: clamped, remaining: duration - clamped, fraction: clamped / duration };
}

export const setlistCommands = {
  list: () => ({ cmd: "setlist.list" }),
  songs: () => ({ cmd: "setlist.songs" }),
  status: () => ({ cmd: "setlist.status" }),
  use: (setlist) => ({ cmd: "setlist.select", setlist }),
  cue: (entry) => ({ cmd: "setlist.cue", entry }),
  play: () => ({ cmd: "setlist.play" }),
  stop: () => ({ cmd: "setlist.stop" }),
  next: () => ({ cmd: "setlist.next" }),
  previous: () => ({ cmd: "setlist.previous" }),
  autoAdvance: (enabled) => ({ cmd: "setlist.auto_advance", enabled }),
  countIn: (enabled) => ({ cmd: "setlist.count_in", enabled }),
};

// A musician's or the stage's link only reaches its own page, whatever the address says.
export function pageForAccess(path, access) {
  if (access?.role === "musician") return "/monitor";
  if (access?.role === "stage") return "/setlist";
  return path;
}

// The address a phone opens for `link` (as `access.list` gives it): the server's network
// address when this page was opened on the machine itself, which a phone cannot reach.
export function linkUrl(link, origins, here = globalThis.location?.origin) {
  const local = /^https?:\/\/(localhost|127\.[\d.]+|\[::1\])(:|\/|$)/.test(here || "");
  const base = local && origins?.length ? origins[0] : here;
  return new URL(link.path, base).toString();
}

export const accessCommands = {
  list: () => ({ cmd: "access.list" }),
  createMusician: (musician, name) => ({ cmd: "access.create", role: "musician", musician, ...(name ? { name } : {}) }),
  createStage: (name) => ({ cmd: "access.create", role: "stage", ...(name ? { name } : {}) }),
  rename: (id, name) => ({ cmd: "access.update", id, name }),
  renew: (id) => ({ cmd: "access.renew", id }),
  remove: (id) => ({ cmd: "access.delete", id }),
};

// What a setlist entry is sent back as in `setlist.set`: its start marker's GUID when it has
// one, else its name.
export function songSelector(entry) {
  return entry.guid || entry.name;
}

// The entries of a setlist with the one at `from` moved to `to`, as `setlist.move` does.
export function moveEntry(list, from, to) {
  const next = [...list];
  const [moved] = next.splice(from, 1);
  next.splice(to, 0, moved);
  return next;
}

// Where an entry dragged by `dy` pixels lands, rows being `rowHeight` apart.
export function dropIndex(from, dy, rowHeight, count) {
  return Math.max(0, Math.min(count - 1, from + Math.round(dy / rowHeight)));
}
