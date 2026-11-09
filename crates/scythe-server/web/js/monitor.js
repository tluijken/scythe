// "More me": a musician's own monitor mix on their phone. Only the chosen mix is reachable
// from this page; `?mix=Name&lock=1` pins it so the switcher is gone too.

import { html, useCallback, useEffect, useRef, useState } from "../vendor/preact-htm.js";
import {
  MORE_ME_RANGE, detector, isMixEvent, mixCommands, mixPeak, normalizeMix, normalizeMixMeters, normalizeMixes, pickMix,
} from "./domains.js";
import { dbToGain, formatDbValue, formatPan, gainToDb } from "./format.js";
import { useStoreValue } from "./store.js";
import { Fader, Icon, IconButton, Knob, Logo, Meter, OfflineVeil, Pan, StatusPill } from "./ui.js";

const MIX_KEY = "scythe.monitor.mix";

function remembered() {
  try {
    return JSON.parse(localStorage.getItem(MIX_KEY));
  } catch {
    return null;
  }
}

function remember(mix) {
  try {
    if (mix) localStorage.setItem(MIX_KEY, JSON.stringify({ id: mix.id, name: mix.name }));
    else localStorage.removeItem(MIX_KEY);
  } catch {
    // Not remembered in private browsing; the musician picks again next time.
  }
}

export function Unavailable({ what }) {
  return html`<div class="unavailable"><${Logo} size=${56} /><h1>${what} is not available</h1>
    <p class="dim">This Scythe does not offer ${what.toLowerCase()} yet. Update Scythe on the machine running the session.</p></div>`;
}

// Mixes from the server, kept fresh: fetched on connecting, after project changes and mix
// events, with local edits shown at once.
function useMixes(client, store, status) {
  const [mixes, setMixes] = useState(null);
  const [supported, setSupported] = useState(null);
  const meters = useRef(new Map());
  const detect = useRef(detector(client, mixCommands.list()));
  const load = useCallback(() => {
    client.send(mixCommands.list()).then((r) => setMixes(normalizeMixes(r)), () => {});
  }, [client]);
  useEffect(() => {
    if (status !== "open") return;
    detect.current().then((ok) => {
      if (ok === null) return;
      setSupported(ok);
      if (ok) load();
    });
  }, [status]);
  useEffect(() => {
    let timer = null;
    const soon = () => {
      clearTimeout(timer);
      timer = setTimeout(load, 120);
    };
    const offChanged = client.on("event:project_changed", soon);
    const offEvent = client.on("event", (e) => isMixEvent(e.event) && soon());
    const offMeters = client.on("event:mix_meters", (e) => (meters.current = normalizeMixMeters(e)));
    return () => {
      clearTimeout(timer);
      offChanged();
      offEvent();
      offMeters();
    };
  }, [load]);
  return { mixes, setMixes, supported, meters, load };
}

function MixPicker({ mixes, onPick, current }) {
  return html`<div class="mix-picker">
    <h1>Whose mix?</h1>
    <p class="dim">Pick yours; this device remembers it.</p>
    <div class="mix-list">
      ${mixes.map((m) => html`<button class=${`mix-choice ${current?.id === m.id ? "on" : ""}`} onClick=${() => onPick(m)}>
        <span class="dot" style=${{ background: m.color || "var(--orange)" }}></span>
        <span class="mix-choice-name">${m.name}</span>
        ${m.output && html`<span class="dim small mono">${m.output}</span>`}
      </button>`)}
      ${mixes.length === 0 && html`<p class="dim">No monitor mixes in this session yet. They are made in Scythe's monitor page.</p>`}
    </div>
  </div>`;
}

function SourceRow({ source, onLevel, onPan, onMute }) {
  const [open, setOpen] = useState(false);
  const [drag, setDrag] = useState(null);
  const db = drag ?? source.db;
  return html`<div class=${`source ${source.own ? "own" : ""} ${source.muted ? "muted" : ""}`}>
    <div class="source-head">
      <button class="source-name" onClick=${() => setOpen(!open)} aria-expanded=${open}>
        <span class="dot" style=${{ background: source.color || "var(--text-dim)" }}></span>
        <span class="ellipsis">${source.name}</span>
        ${source.own && html`<span class="pill small accent">me</span>`}
      </button>
      <span class="source-db mono">${formatDbValue(db)} dB</span>
      <button class=${`flag mute big ${source.muted ? "on" : ""}`} onClick=${() => onMute(!source.muted)} aria-pressed=${source.muted}>M</button>
    </div>
    <${Fader} horizontal=${true} gain=${dbToGain(db)} length=${280} color=${source.color || "var(--text-dim)"} label=${`${source.name} level`}
      onInput=${(g) => {
        setDrag(gainToDb(g));
        onLevel(gainToDb(g));
      }} onCommit=${() => setDrag(null)} />
    ${open && html`<div class="source-pan"><span class="dim small">Pan ${formatPan(source.pan)}</span><${Pan} pan=${source.pan} width=${240} onInput=${onPan} /></div>`}
  </div>`;
}

export function MonitorPage({ client, store }) {
  const status = useStoreValue(store, "status");
  const { mixes, setMixes, supported, meters, load } = useMixes(client, store, status);
  const params = new URLSearchParams(location.search);
  const locked = params.get("lock") === "1" && params.has("mix");
  const [chosen, setChosen] = useState(() => (params.has("mix") ? { name: params.get("mix") } : remembered()));
  const [picking, setPicking] = useState(false);
  const mix = mixes ? pickMix(mixes, chosen) : null;

  useEffect(() => {
    document.title = mix ? `${mix.name} – Scythe` : "Monitor – Scythe";
  }, [mix?.name]);

  if (supported === false) return html`<${Unavailable} what="Monitor mixing" />`;
  if (!mixes) return html`<div class="boot"><${Logo} size=${56} /><div>Connecting…</div><${OfflineVeil} status=${status} /></div>`;

  const pick = (m) => {
    remember(m);
    setChosen({ id: m.id, name: m.name });
    setPicking(false);
  };
  if (!mix || picking) {
    if (locked && !picking) return html`<${Unavailable} what=${`The mix "${params.get("mix")}"`} />`;
    return html`<div class="monitor-page"><${MixPicker} mixes=${mixes} current=${mix} onPick=${pick} /><${OfflineVeil} status=${status} /></div>`;
  }

  const patch = (change) => setMixes((list) => list.map((m) => (m.id === mix.id ? change(m) : m)));
  const patchSource = (id, fields) => patch((m) => ({ ...m, sources: m.sources.map((s) => (s.id === id ? { ...s, ...fields } : s)) }));
  const answer = (result) => {
    if (result && (result.sources || result.mix != null)) {
      const fresh = normalizeMix(result);
      if (fresh.id === mix.id && fresh.sources.length) setMixes((list) => list.map((m) => (m.id === mix.id ? fresh : m)));
    }
  };
  const send = (command, key) =>
    store.run(command, { latest: key }).then(answer, () => load());
  const sources = [...mix.sources].sort((a, b) => b.own - a.own);
  return html`<div class="monitor-page">
    <header class="monitor-head">
      ${locked
        ? html`<div class="mix-title"><span class="dot" style=${{ background: mix.color || "var(--orange)" }}></span>${mix.name}</div>`
        : html`<button class="mix-title" onClick=${() => setPicking(true)} title="Choose another mix">
            <span class="dot" style=${{ background: mix.color || "var(--orange)" }}></span>${mix.name}<${Icon} name="down" size=${18} /></button>`}
      <${StatusPill} status=${status} compact=${true} />
    </header>
    <${Meter} axis="x" cls="mix-meter" read=${() => mixPeak(store, meters.current, mix.id)} />
    <div class="more-me">
      <${IconButton} icon="minus" title="Less me" cls="round big"
        onClick=${() => {
          const db = Math.max(MORE_ME_RANGE[0], mix.moreMeDb - 1);
          patch((m) => ({ ...m, moreMeDb: db }));
          send(mixCommands.moreMe(mix.id, db), "more-me");
        }} />
      <${Knob} value=${mix.moreMeDb} range=${MORE_ME_RANGE} size=${Math.min(220, innerWidth * 0.48)} label="More me"
        text=${`${mix.moreMeDb > 0 ? "+" : ""}${mix.moreMeDb.toFixed(1)} dB`}
        onInput=${(db) => {
          patch((m) => ({ ...m, moreMeDb: db }));
          send(mixCommands.moreMe(mix.id, db), "more-me");
        }} />
      <${IconButton} icon="plus" title="More me" cls="round big"
        onClick=${() => {
          const db = Math.min(MORE_ME_RANGE[1], mix.moreMeDb + 1);
          patch((m) => ({ ...m, moreMeDb: db }));
          send(mixCommands.moreMe(mix.id, db), "more-me");
        }} />
    </div>
    <div class="sources">
      ${sources.map(
        (s) => html`<${SourceRow} key=${s.id} source=${s}
          onLevel=${(db) => {
            patchSource(s.id, { db });
            send(mixCommands.level(mix.id, s.id, db), `level:${s.id}`);
          }}
          onPan=${(pan) => {
            patchSource(s.id, { pan });
            send(mixCommands.pan(mix.id, s.id, pan), `pan:${s.id}`);
          }}
          onMute=${(muted) => {
            patchSource(s.id, { muted });
            send(mixCommands.mute(mix.id, s.id, muted), `mute:${s.id}`);
          }} />`,
      )}
    </div>
    <${OfflineVeil} status=${status} />
  </div>`;
}
