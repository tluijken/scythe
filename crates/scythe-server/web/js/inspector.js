// The inspector: the selected item, and the selected track's settings, sends and effects.

import { html, useEffect, useMemo, useState } from "../vendor/preact-htm.js";
import { FlagButtons } from "./arrange.js";
import { formatDb, formatPan, formatTime, inputChoices, SWATCHES } from "./format.js";
import { liveControl } from "./mixer.js";
import { Fader, Icon, IconButton, Modal, Pan, Switch } from "./ui.js";

const trackRef = (id) => (id === 0 ? "master" : id);

function Section({ title, children, right }) {
  return html`<div class="section"><div class="section-head"><h3>${title}</h3>${right}</div>${children}</div>`;
}

function ItemSection({ store, item, cursor, onDeleted }) {
  return html`<${Section} title="Item">
    <div class="kv"><span>Name</span><span class="ellipsis">${item.name}</span></div>
    <div class="kv"><span>Position</span><span class="mono">${formatTime(item.position)}</span></div>
    <div class="kv"><span>Length</span><span class="mono">${formatTime(item.length)}</span></div>
    ${item.takes > 1 && html`<div class="kv"><span>Take</span><span class="mono">${item.active_take + 1} / ${item.takes}</span></div>`}
    <div class="kv"><span>Volume</span><span class="mono">${formatDb(item.volume)} dB</span></div>
    <div class="control-row">
      <${Fader} horizontal=${true} gain=${item.volume} length=${220} label="Item volume"
        onCommit=${(g) => store.run({ cmd: "item.volume", item: item.id, volume: g }).then((i) => store.patchItem(i), () => {})} />
    </div>
    <div class="button-row">
      <button class=${`btn small toggle ${item.muted ? "on warn" : ""}`} onClick=${() => store.fire({ cmd: "item.mute", item: item.id, muted: !item.muted })}>Mute</button>
      <button class="btn small" disabled=${!(cursor > item.position && cursor < item.position + item.length)}
        title="Split at the edit cursor" onClick=${() => store.fire({ cmd: "item.split", item: item.id, at: cursor })}><${Icon} name="split" size=${14} />Split</button>
      <button class="btn small danger" onClick=${() => {
        store.fire({ cmd: "item.delete", item: item.id });
        onDeleted();
      }}><${Icon} name="trash" size=${14} />Delete</button>
    </div>
  </${Section}>`;
}

const FOLDER_CHOICES = [
  { value: 1, label: "Starts a folder" },
  { value: 0, label: "Plain track" },
  { value: -1, label: "Closes 1 folder" },
  { value: -2, label: "Closes 2 folders" },
  { value: -3, label: "Closes 3 folders" },
];

function TrackSection({ store, snap, track, onDeleted }) {
  const master = track.id === 0;
  const choices = useMemo(() => {
    const list = inputChoices(16);
    return list.some((c) => c.value === track.input) ? list : [...list, { value: track.input, label: `Input ${track.input}` }];
  }, [track.input]);
  const folders = FOLDER_CHOICES.some((c) => c.value === track.folder_depth)
    ? FOLDER_CHOICES
    : [...FOLDER_CHOICES, { value: track.folder_depth, label: `Depth ${track.folder_depth}` }];
  return html`<${Section} title=${master ? "Master" : "Track"}>
    ${!master && html`<label class="field"><span>Name</span>
      <input value=${track.name} key=${track.id} onChange=${(e) => store.fire({ cmd: "track.rename", track: track.id, name: e.target.value })} /></label>`}
    <div class="kv"><span>Volume</span><span class="mono">${formatDb(track.volume)} dB</span></div>
    <div class="control-row"><${Fader} horizontal=${true} gain=${track.volume} length=${220} color=${track.color || "var(--text-dim)"}
      onInput=${(g) => liveControl(store, track, "volume", "track.volume", g)} /></div>
    <div class="kv"><span>Pan</span><span class="mono">${formatPan(track.pan)}</span></div>
    <div class="control-row"><${Pan} pan=${track.pan} width=${220} onInput=${(p) => liveControl(store, track, "pan", "track.pan", p)} /></div>
    ${master
      ? html`<div class="button-row"><button class=${`flag mute ${track.muted ? "on" : ""}`} onClick=${() => store.fire({ cmd: "track.mute", track: "master" })}>M</button></div>`
      : html`
        <${FlagButtons} store=${store} track=${track} />
        <label class="field"><span>Input</span>
          <select value=${track.input} onChange=${(e) => store.fire({ cmd: "track.input", track: track.id, input: parseInt(e.target.value, 10) })}>
            ${choices.map((c) => html`<option value=${c.value}>${c.label}</option>`)}
          </select></label>
        <label class="field"><span>Folder</span>
          <select value=${track.folder_depth} onChange=${(e) => store.fire({ cmd: "track.folder", track: track.id, depth: parseInt(e.target.value, 10) })}>
            ${folders.map((c) => html`<option value=${c.value}>${c.label}</option>`)}
          </select></label>
        <div class="swatches small">
          ${SWATCHES.map((c) => html`<button class=${`swatch ${track.color === c ? "on" : ""}`} style=${{ background: c }} title=${c}
            onClick=${() => store.fire({ cmd: "track.color", track: track.id, color: c })}></button>`)}
        </div>
        <div class="button-row">
          <button class="btn small" disabled=${track.index === 0} onClick=${() => store.fire({ cmd: "track.move", track: track.id, index: track.index - 1 })}>
            <${Icon} name="up" size=${14} />Up</button>
          <button class="btn small" disabled=${track.index === snap.tracks.length - 1}
            onClick=${() => store.fire({ cmd: "track.move", track: track.id, index: track.index + 1 })}><${Icon} name="down" size=${14} />Down</button>
          <span class="grow"></span>
          <button class="btn small danger" onClick=${() => {
            if (confirm(`Delete the track "${track.name}"? Undo brings it back.`)) {
              store.fire({ cmd: "track.delete", track: track.id });
              onDeleted();
            }
          }}><${Icon} name="trash" size=${14} />Delete</button>
        </div>`}
  </${Section}>`;
}

function SendsSection({ store, snap, track }) {
  const [dest, setDest] = useState("");
  const name = (id) => snap.tracks.find((t) => t.id === id)?.name || `Track ${id}`;
  const sendControl = (send, field, value) => {
    const cmd = field === "pan" ? { cmd: "send.pan", track: track.id, send: send.index, pan: value } : { cmd: "send.volume", track: track.id, send: send.index, volume: value };
    store.fire(cmd, { latest: `${cmd.cmd}:${track.id}:${send.index}` });
  };
  const targets = snap.tracks.filter((t) => t.id !== track.id);
  return html`<${Section} title="Sends">
    ${track.sends.length === 0 && html`<p class="dim small">No sends.</p>`}
    ${track.sends.map(
      (s) => html`<div class="send" key=${s.index}>
        <div class="send-head"><span class="ellipsis">→ ${name(s.dest)}</span><span class="mono dim">${formatDb(s.volume)} dB · ${formatPan(s.pan)}</span></div>
        <${Fader} horizontal=${true} gain=${s.volume} length=${200} label=${`Send to ${name(s.dest)}`} onInput=${(g) => sendControl(s, "volume", g)} />
        <div class="send-row">
          <${Pan} pan=${s.pan} width=${140} onInput=${(p) => sendControl(s, "pan", p)} />
          <button class=${`flag mute ${s.muted ? "on" : ""}`} title="Mute send" onClick=${() => store.fire({ cmd: "send.mute", track: track.id, send: s.index, muted: !s.muted })}>M</button>
          <${IconButton} icon="trash" title="Remove send" onClick=${() => store.fire({ cmd: "send.remove", track: track.id, send: s.index })} />
        </div>
      </div>`,
    )}
    <div class="add-row">
      <select value=${dest} onChange=${(e) => setDest(e.target.value)}>
        <option value="">Send to…</option>
        ${targets.map((t) => html`<option value=${t.id}>${t.name || `Track ${t.index + 1}`}</option>`)}
      </select>
      <button class="btn small" disabled=${!dest} onClick=${() => {
        store.fire({ cmd: "send.add", track: track.id, dest: parseInt(dest, 10) });
        setDest("");
      }}><${Icon} name="plus" size=${14} />Add</button>
    </div>
  </${Section}>`;
}

function FxParams({ store, track, index }) {
  const [params, setParams] = useState(null);
  const [error, setError] = useState(null);
  useEffect(() => {
    let live = true;
    store.client
      .send({ cmd: "fx.params", track: trackRef(track.id), index })
      .then((p) => live && setParams(p), (e) => live && setError(e.message));
    return () => (live = false);
  }, [track.id, index]);
  if (error) return html`<p class="dim small">${error}</p>`;
  if (!params) return html`<p class="dim small">Loading parameters…</p>`;
  if (params.length === 0) return html`<p class="dim small">No parameters.</p>`;
  const change = (p, value) => {
    setParams((list) => list.map((x) => (x.id === p.id ? { ...x, value } : x)));
    store
      .run({ cmd: "fx.param", track: trackRef(track.id), index, param: p.id, value }, { latest: `fx.param:${track.id}:${index}:${p.id}` })
      .then((answer) => answer && setParams((list) => list.map((x) => (x.id === p.id ? { ...x, ...answer } : x))), () => {});
  };
  return html`<div class="params">
    ${params.map(
      (p) => html`<label class="param" key=${p.id}>
        <span class="param-name ellipsis" title=${p.name}>${p.name}</span>
        <input type="range" min=${p.min} max=${p.max} step=${(p.max - p.min) / 1000 || 0.001} value=${p.value}
          onInput=${(e) => change(p, parseFloat(e.target.value))} onDblClick=${() => change(p, p.default)} />
        <span class="param-text mono">${p.text || p.value.toFixed(2)}</span>
      </label>`,
    )}
  </div>`;
}

function FxCatalog({ store, track, onClose }) {
  const [catalog, setCatalog] = useState(null);
  const [query, setQuery] = useState("");
  useEffect(() => {
    store.client.send({ cmd: "fx.catalog" }).then(setCatalog, (e) => setCatalog({ error: e.message }));
  }, []);
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const hits = Array.isArray(catalog)
    ? catalog.filter((p) => words.every((w) => `${p.name} ${p.format} ${p.id}`.toLowerCase().includes(w))).slice(0, 200)
    : [];
  const add = (plugin) => {
    store.fire({ cmd: "fx.add", track: trackRef(track.id), plugin: plugin.id });
    onClose();
  };
  return html`<${Modal} title=${`Add effect to ${track.id === 0 ? "Master" : track.name || `Track ${track.index + 1}`}`} onClose=${onClose} wide=${true}>
    <div class="input-icon"><${Icon} name="search" size=${16} />
      <input placeholder="Search effects" value=${query} autofocus onInput=${(e) => setQuery(e.target.value)}
        onKeyDown=${(e) => e.key === "Enter" && hits[0] && add(hits[0])} /></div>
    <div class="catalog">
      ${catalog === null && html`<p class="dim">Scanning plugins… the first time can take a while.</p>`}
      ${catalog?.error && html`<p class="dim">${catalog.error}</p>`}
      ${hits.map((p) => html`<button class="catalog-row" key=${p.id} onClick=${() => add(p)}>
        <span class="ellipsis">${p.name}</span><span class="pill small">${p.format}</span></button>`)}
    </div>
  </${Modal}>`;
}

function FxSection({ store, track }) {
  const [open, setOpen] = useState(null);
  const [adding, setAdding] = useState(false);
  return html`<${Section} title="Effects" right=${html`<button class="btn small" onClick=${() => setAdding(true)}><${Icon} name="plus" size=${14} />Add</button>`}>
    ${track.fx.length === 0 && html`<p class="dim small">No effects.</p>`}
    ${track.fx.map(
      (fx) => html`<div class=${`fx ${open === fx.index ? "open" : ""}`} key=${`${fx.index}-${fx.name}`}>
        <div class="fx-row">
          <${Switch} on=${fx.enabled} title=${fx.enabled ? "Bypass" : "Enable"}
            onChange=${(on) => store.fire({ cmd: "fx.bypass", track: trackRef(track.id), index: fx.index, bypassed: !on })} />
          <button class="fx-name ellipsis" onClick=${() => setOpen(open === fx.index ? null : fx.index)} title=${fx.name}>
            ${fx.name}${!fx.running && html`<span class="pill small warn">not running</span>`}</button>
          <${IconButton} icon="trash" title="Remove" onClick=${() => {
            setOpen(null);
            store.fire({ cmd: "fx.remove", track: trackRef(track.id), index: fx.index });
          }} />
        </div>
        ${open === fx.index && html`<${FxParams} store=${store} track=${track} index=${fx.index} />`}
      </div>`,
    )}
    ${adding && html`<${FxCatalog} store=${store} track=${track} onClose=${() => setAdding(false)} />`}
  </${Section}>`;
}

export function Inspector({ store, snap, sel, setSel, onClose }) {
  const [tab, setTab] = useState("track");
  const track = sel.track === 0 ? snap.master : snap.tracks.find((t) => t.id === sel.track);
  const item = sel.item != null ? snap.tracks.flatMap((t) => t.items).find((i) => i.id === sel.item) : null;
  const cursor = (store.transport || snap.transport).cursor;
  const tabs = track && track.id !== 0 ? ["track", "sends", "fx"] : ["track", "fx"];
  return html`<aside class="inspector panel">
    <div class="panel-tools">
      <span class="panel-title ellipsis">${track ? (track.id === 0 ? "Master" : track.name || `Track ${track.index + 1}`) : "Inspector"}</span>
      <span class="grow"></span>
      <${IconButton} icon="close" title="Close the inspector" onClick=${onClose} />
    </div>
    ${track &&
    html`<div class="tabs">${tabs.map(
      (t) => html`<button class=${`tab ${tab === t ? "on" : ""}`} onClick=${() => setTab(t)}>${{ track: "Track", sends: "Sends", fx: `FX ${track.fx.length}` }[t]}</button>`,
    )}</div>`}
    <div class="inspector-body">
      ${item && html`<${ItemSection} store=${store} item=${item} cursor=${cursor} onDeleted=${() => setSel({ ...sel, item: null })} />`}
      ${!track && html`<p class="dim empty-note">Select a track or item to edit it here.</p>`}
      ${track && (tab === "track" || !tabs.includes(tab)) && html`<${TrackSection} store=${store} snap=${snap} track=${track} onDeleted=${() => setSel({ track: null, item: null })} />`}
      ${track && tab === "sends" && tabs.includes("sends") && html`<${SendsSection} store=${store} snap=${snap} track=${track} />`}
      ${track && tab === "fx" && html`<${FxSection} store=${store} track=${track} />`}
    </div>
  </aside>`;
}
