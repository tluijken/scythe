// The session editor: transport, arrange, mixer and inspector, laid out like the desktop app.

import { html, useEffect, useState } from "../vendor/preact-htm.js";
import { Arrange } from "./arrange.js";
import { detector } from "./domains.js";
import { formatBpm, formatTime } from "./format.js";
import { Inspector } from "./inspector.js";
import { Mixer } from "./mixer.js";
import { useStoreValue } from "./store.js";
import { Icon, IconButton, Logo, Modal, OfflineVeil, StatusPill } from "./ui.js";

export const trackRef = (id) => (id === 0 ? "master" : id);

function useDomains(client, status) {
  const [domains, setDomains] = useState({ mix: false, setlist: false });
  useEffect(() => {
    if (status !== "open") return;
    const mix = detector(client, { cmd: "mix.list" });
    const setlist = detector(client, { cmd: "setlist.list" });
    Promise.all([mix(), setlist()]).then(([m, s]) => setDomains({ mix: !!m, setlist: !!s }));
  }, [client, status]);
  return domains;
}

function Transport({ store, snap }) {
  const t = useStoreValue(store, "transport") || snap.transport;
  const [editingBpm, setEditingBpm] = useState(false);
  const run = (cmd) => store.fire({ cmd });
  const [num, denom] = snap.time_signature;
  const bpmInput = (e) => {
    if (e.key === "Enter" || e.type === "blur") {
      const bpm = parseFloat(e.target.value);
      setEditingBpm(false);
      if (isFinite(bpm) && bpm !== snap.bpm) store.fire({ cmd: "project.tempo", bpm, at: 0 });
    } else if (e.key === "Escape") setEditingBpm(false);
  };
  return html`<div class="transport">
    <${IconButton} icon="start" title="Go to start (Home)" onClick=${() => store.fire({ cmd: "transport.seek", position: 0 })} />
    <${IconButton} icon="stop" title="Stop (Esc)" size=${15} onClick=${() => run("transport.stop")} />
    <${IconButton} icon=${t.playing ? "pause" : "play"} title=${t.playing ? "Pause (Space)" : "Play (Space)"} cls="play" size=${18}
      onClick=${() => run(t.playing ? "transport.pause" : "transport.play")} />
    <${IconButton} icon="record" title="Record armed tracks" cls=${`record ${t.recording ? "on" : ""}`} size=${14}
      onClick=${() => run(t.recording ? "transport.stop" : "transport.record")} />
    <${IconButton} icon="loop" title="Toggle repeat (R)" active=${t.loop_enabled}
      onClick=${() => store.fire({ cmd: "transport.loop", enabled: !t.loop_enabled })} />
    <div class="sep"></div>
    <div class="clock">
      <span class="bars">${t.bar_beat ?? ""}</span>
      <span class="time">${formatTime(t.position)}</span>
    </div>
    ${editingBpm
      ? html`<input class="pill-input mono" type="number" min="20" max="400" step="0.1" value=${formatBpm(snap.bpm)} autofocus
          onKeyDown=${bpmInput} onBlur=${bpmInput} />`
      : html`<button class="pill mono" title="Tempo at the project start: click to change" onClick=${() => setEditingBpm(true)}>
          ${formatBpm(snap.bpm)} bpm</button>`}
    <span class="pill mono wide-only">${num}/${denom}</span>
    <${IconButton} icon="metronome" title="Metronome" cls="raised wide-only" size=${16} active=${t.metronome}
      onClick=${() => store.fire({ cmd: "transport.metronome", enabled: !t.metronome })} />
  </div>`;
}

function FileDialog({ store, snap, onClose }) {
  const [path, setPath] = useState(snap.path || "");
  const [busy, setBusy] = useState(false);
  const act = async (command, message) => {
    setBusy(true);
    try {
      await store.run(command);
      store.notify(message);
      onClose();
    } catch {
      setBusy(false);
    }
  };
  return html`<${Modal} title="Project" onClose=${onClose}>
    <p class="dim small">Paths are on the machine running Scythe.</p>
    <label class="field"><span>File</span>
      <input class="mono" value=${path} onInput=${(e) => setPath(e.target.value)} placeholder="/home/band/live/set.RPP" spellcheck="false" /></label>
    <div class="button-row">
      <button class="btn" disabled=${busy || !path} onClick=${() => act({ cmd: "project.open", path }, "Opened")}><${Icon} name="open" size=${16} />Open</button>
      <button class="btn" disabled=${busy || !snap.path} onClick=${() => act({ cmd: "project.save" }, "Saved")}><${Icon} name="save" size=${16} />Save</button>
      <button class="btn" disabled=${busy || !path || path === snap.path} onClick=${() => act({ cmd: "project.save", path }, "Saved")}>Save as</button>
      <span class="grow"></span>
      <button class="btn danger" disabled=${busy} onClick=${() => {
        if (!snap.dirty || confirm("Drop the unsaved changes and start an empty project?")) act({ cmd: "project.new" }, "New project");
      }}>New</button>
    </div>
  </${Modal}>`;
}

function TopBar({ store, snap, status, domains, onFile }) {
  const sub = `${snap.dirty ? "Unsaved changes" : snap.path ? "Saved" : "Not saved yet"}${snap.path ? " · " + snap.path.split("/").pop() : ""}`;
  return html`<header class="topbar">
    <div class="title">
      <${Logo} />
      <div class="title-text"><div class="name">${snap.name || "Untitled"}</div><div class="sub">${sub}</div></div>
    </div>
    <${Transport} store=${store} snap=${snap} />
    <div class="topbar-right">
      <${IconButton} icon="undo" title=${snap.undo ? `Undo ${snap.undo} (Ctrl+Z)` : "Nothing to undo"} disabled=${!snap.undo}
        onClick=${() => store.fire({ cmd: "project.undo" })} />
      <${IconButton} icon="redo" title=${snap.redo ? `Redo ${snap.redo} (Ctrl+Shift+Z)` : "Nothing to redo"} disabled=${!snap.redo}
        onClick=${() => store.fire({ cmd: "project.redo" })} />
      <${IconButton} icon="save" title="Open, save, new (Ctrl+S saves)" onClick=${onFile} />
      ${domains.mix && html`<a class="icon-btn" href="/monitor" title="Monitor mixes"><${Icon} name="headphones" /></a>`}
      ${domains.setlist && html`<a class="icon-btn" href="/setlist" title="Setlist"><${Icon} name="list" /></a>`}
      <${StatusPill} status=${status} />
    </div>
  </header>`;
}

function Rail({ view, setView }) {
  const toggle = (key) => setView({ ...view, [key]: !view[key] });
  return html`<nav class="rail">
    <${IconButton} icon="arrange" title="Arrange" cls="rail-btn" active=${true} size=${20} />
    <${IconButton} icon="mixer" title="Mixer (M)" cls="rail-btn" active=${view.mixer} size=${20} onClick=${() => toggle("mixer")} />
    <${IconButton} icon="fx" title="Inspector (I)" cls="rail-btn" active=${view.inspector} size=${20} onClick=${() => toggle("inspector")} />
  </nav>`;
}

function useShortcuts(store, snap, sel, setSel, setFile) {
  useEffect(() => {
    const key = (e) => {
      if (e.target.closest("input, select, textarea") || document.querySelector(".modal")) return;
      const mod = e.ctrlKey || e.metaKey;
      const t = store.transport || {};
      if (e.key === " ") {
        e.preventDefault();
        store.fire({ cmd: t.playing ? "transport.pause" : "transport.play" });
      } else if (e.key === "Escape") store.fire({ cmd: "transport.stop" });
      else if (e.key === "Home") store.fire({ cmd: "transport.seek", position: 0 });
      else if (mod && e.key.toLowerCase() === "z") {
        e.preventDefault();
        store.fire({ cmd: e.shiftKey ? "project.redo" : "project.undo" }, { quiet: true });
      } else if (mod && e.key.toLowerCase() === "s") {
        e.preventDefault();
        if (snap?.path) store.run({ cmd: "project.save" }).then(() => store.notify("Saved"), () => {});
        else setFile(true);
      } else if (!mod && e.key.toLowerCase() === "r") store.fire({ cmd: "transport.loop", enabled: !t.loop_enabled });
      else if (sel.item != null && (e.key === "Delete" || e.key === "Backspace")) {
        store.fire({ cmd: "item.delete", item: sel.item });
        setSel({ ...sel, item: null });
      } else if (sel.item != null && !mod && e.key.toLowerCase() === "s") {
        store.fire({ cmd: "item.split", item: sel.item, at: t.cursor ?? 0 });
      }
    };
    addEventListener("keydown", key);
    return () => removeEventListener("keydown", key);
  }, [store, snap, sel]);
}

export function Editor({ client, store }) {
  const snap = useStoreValue(store, "snapshot");
  const status = useStoreValue(store, "status");
  const domains = useDomains(client, status);
  const wide = matchMedia("(min-width: 1100px)").matches;
  const [view, setView] = useState({ mixer: true, inspector: wide });
  const [sel, setSel] = useState({ track: null, item: null });
  const [file, setFile] = useState(false);
  useShortcuts(store, snap, sel, setSel, setFile);
  useEffect(() => {
    document.title = snap ? `${snap.dirty ? "* " : ""}${snap.name || "Untitled"} – Scythe` : "Scythe";
  }, [snap?.name, snap?.dirty]);
  if (!snap) return html`<div class="boot"><${Logo} size=${56} /><div>Connecting to Scythe…</div><${OfflineVeil} status=${status} /></div>`;
  const select = (next) => {
    setSel(next);
    if (next.track != null && !view.inspector && wide) setView({ ...view, inspector: true });
  };
  return html`<div class=${`editor ${view.inspector ? "with-inspector" : ""} ${view.mixer ? "with-mixer" : ""}`}>
    <${TopBar} store=${store} snap=${snap} status=${status} domains=${domains} onFile=${() => setFile(true)} />
    <${Rail} view=${view} setView=${setView} />
    <main class="main">
      <${Arrange} store=${store} snap=${snap} sel=${sel} setSel=${select} />
      ${view.mixer && html`<${Mixer} store=${store} snap=${snap} sel=${sel} setSel=${select} />`}
    </main>
    ${view.inspector && html`<${Inspector} store=${store} snap=${snap} sel=${sel} setSel=${setSel}
      onClose=${() => setView({ ...view, inspector: false })} />`}
    ${file && html`<${FileDialog} store=${store} snap=${snap} onClose=${() => setFile(false)} />`}
    <${OfflineVeil} status=${status} />
  </div>`;
}
