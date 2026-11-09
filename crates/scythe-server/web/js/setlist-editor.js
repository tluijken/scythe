// Setlists in the editor: make, rename, duplicate and delete them, add songs, drag them into
// order, import and export text, and choose which one the stage plays.

import { html, useEffect, useState } from "../vendor/preact-htm.js";
import { dropIndex, moveEntry, songSelector } from "./domains.js";
import { formatClock } from "./format.js";
import { EditableText, Icon, IconButton, Modal, Switch, dragPointer } from "./ui.js";

const ROW_H = 44;

function useSetlistData(store) {
  const [data, setData] = useState(null);
  const load = () =>
    Promise.all([store.run({ cmd: "setlist.list" }), store.run({ cmd: "setlist.songs" })]).then(
      ([list, songs]) => setData({ list, songs }),
      () => {},
    );
  useEffect(() => {
    load();
    let timer = null;
    const off = store.client.on("event:project_changed", () => {
      clearTimeout(timer);
      timer = setTimeout(load, 150);
    });
    return () => {
      clearTimeout(timer);
      off();
    };
  }, []);
  return { data, setData, load };
}

function TextDialog({ title, text, onClose, onImport }) {
  const [value, setValue] = useState(text || "");
  const download = () => {
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([value], { type: "text/plain" }));
    a.download = `${(value.split("\n")[0] || "setlist").trim() || "setlist"}.txt`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 1000);
  };
  return html`<${Modal} title=${title} onClose=${onClose}>
    <p class="dim small">A title line, then one song per line; numbering and bullets are fine. Songs are matched to the markers by name.</p>
    <textarea class="mono setlist-text" value=${value} onInput=${(e) => setValue(e.target.value)} rows="12" spellcheck="false"
      placeholder=${"Saturday set\n1. Opener\n2. Ballad\n3. Closer"}></textarea>
    <div class="button-row">
      ${onImport
        ? html`<span class="grow"></span><button class="btn primary" disabled=${!value.trim()} onClick=${() => onImport(value)}>Import</button>`
        : html`<button class="btn" onClick=${() => navigator.clipboard?.writeText(value)}><${Icon} name="copy" size=${16} />Copy</button>
            <button class="btn" onClick=${download}><${Icon} name="download" size=${16} />Download</button>`}
    </div>
  </${Modal}>`;
}

function Entry({ entry, index, count, onMove, onRemove }) {
  const [dy, setDy] = useState(null);
  const down = (e) => {
    if (e.button > 0) return;
    e.preventDefault();
    let last = 0;
    dragPointer(
      e,
      ({ dy: d }) => {
        last = d;
        setDy(d);
      },
      ({ cancelled }) => {
        setDy(null);
        const to = dropIndex(index, last, ROW_H, count);
        if (!cancelled && to !== index) onMove(index, to);
      },
    );
  };
  return html`<div class=${`entry ${dy !== null ? "dragging" : ""} ${entry.song == null ? "missing" : ""}`}
      style=${dy !== null ? { transform: `translateY(${dy}px)` } : {}}>
    <button class="grip" onPointerDown=${down} title="Drag to reorder" aria-label="Drag to reorder"><${Icon} name="grip" size=${16} /></button>
    <span class="mono dim entry-num">${index + 1}</span>
    <span class="ellipsis grow" title=${entry.song == null ? "No marker of this name in the project" : ""}>${entry.name}</span>
    <span class="mono dim small">${entry.duration != null ? formatClock(entry.duration) : "--:--"}</span>
    <${IconButton} icon="close" title="Take it off the setlist" onClick=${() => onRemove(index)} />
  </div>`;
}

export function SetlistManager({ store, onClose }) {
  const { data, setData, load } = useSetlistData(store);
  const [chosen, setChosen] = useState(null);
  const [text, setText] = useState(null);
  if (!data) return html`<${Modal} title="Setlists" wide=${true} cls="setlists-modal" onClose=${onClose}><p class="dim">Loading…</p></${Modal}>`;
  const lists = data.list.setlists || [];
  const index = chosen != null && chosen < lists.length ? chosen : (data.list.current ?? 0);
  const list = lists[index] || null;
  const run = (command, message) =>
    store.run(command).then((answer) => {
      if (message) store.notify(message);
      load();
      return answer;
    });
  const quiet = (command, message) => run(command, message).catch(() => {});
  const setSongs = (entries) => quiet({ cmd: "setlist.set", setlist: index, songs: entries.map(songSelector) });
  const move = (from, to) => {
    const next = { ...list, entries: moveEntry(list.entries, from, to) };
    setData({ ...data, list: { ...data.list, setlists: lists.map((l, i) => (i === index ? next : l)) } });
    quiet({ cmd: "setlist.move", setlist: index, from, to });
  };
  const create = () => {
    const name = prompt("Name of the new setlist", "New setlist");
    if (name?.trim()) run({ cmd: "setlist.create", name: name.trim() }).then((l) => setChosen(l.index), () => {});
  };
  return html`<${Modal} title="Setlists" wide=${true} cls="setlists-modal" onClose=${onClose}>
    <div class="button-row">
      <button class="btn primary" onClick=${create}><${Icon} name="plus" size=${16} />New</button>
      <button class="btn" onClick=${() => setText({ title: "Import a setlist", text: "", import: true })}>Import</button>
      <span class="grow"></span>
      <label class="switch-row"><${Switch} on=${!!data.list.auto_advance} title="Cue the next song when one ends"
        onChange=${(enabled) => quiet({ cmd: "setlist.auto_advance", enabled })} /><span>Auto-advance</span></label>
      <a class="btn" href="/setlist" target="_blank" rel="noreferrer"><${Icon} name="list" size=${16} />Stage page</a>
    </div>
    <div class="setlist-editor">
      <div class="setlist-lists">
        ${lists.map((l, i) => html`<button class=${`setlist-choice ${i === index ? "on" : ""}`} onClick=${() => setChosen(i)}>
          <span class="ellipsis grow">${l.name}</span>
          ${i === data.list.current && html`<span class="pill small accent">played</span>`}
          <span class="dim small mono">${l.entries.length}</span></button>`)}
        ${!lists.length && html`<p class="dim small">No setlists yet.</p>`}
      </div>
      ${list
        ? html`<div class="setlist-detail">
            <div class="setlist-detail-head">
              <${EditableText} cls="setlist-title" value=${list.name} onCommit=${(name) => quiet({ cmd: "setlist.rename", setlist: index, name })} />
              <span class="dim small mono">${formatClock(list.duration)}</span>
              <span class="grow"></span>
              ${index !== data.list.current && html`<button class="btn small" onClick=${() => quiet({ cmd: "setlist.select", setlist: index }, `${list.name} is played now`)}>Play this one</button>`}
              <${IconButton} icon="copy" title="Duplicate" onClick=${() => run({ cmd: "setlist.duplicate", setlist: index }).then((l) => setChosen(l.index), () => {})} />
              <${IconButton} icon="download" title="Export as text" onClick=${() =>
                store.run({ cmd: "setlist.export", setlist: index }).then((r) => setText({ title: `Export ${list.name}`, text: r.text }), () => {})} />
              <${IconButton} icon="trash" title="Delete the setlist" onClick=${() => {
                if (confirm(`Delete the setlist "${list.name}"?`)) quiet({ cmd: "setlist.delete", setlist: index }).then(() => setChosen(null));
              }} />
            </div>
            <div class="entries">
              ${list.entries.map((e, i) => html`<${Entry} key=${`${i}-${e.name}`} entry=${e} index=${i} count=${list.entries.length} onMove=${move}
                onRemove=${(at) => setSongs(list.entries.filter((_, k) => k !== at))} />`)}
              ${!list.entries.length && html`<p class="dim small">Empty: add songs below.</p>`}
            </div>
            <div class="label">Songs in the project</div>
            <div class="song-pool">
              ${data.songs.map((s) => html`<button class="pool-song" onClick=${() => setSongs([...list.entries, { guid: s.guid, name: s.name }])}
                title="Add to the end of the setlist"><${Icon} name="plus" size=${14} /><span class="ellipsis">${s.name}</span>
                <span class="mono dim small">${formatClock(s.duration)}</span></button>`)}
              ${!data.songs.length && html`<p class="dim small">The project has no songs: a song starts at a marker (see the docs on stop markers).</p>`}
            </div>
          </div>`
        : html`<div class="setlist-detail dim">Make a setlist to start.</div>`}
    </div>
    ${text && html`<${TextDialog} key=${text.title} title=${text.title} text=${text.text} onClose=${() => setText(null)}
      onImport=${text.import ? (value) => run({ cmd: "setlist.import", text: value }, "Setlist imported").then((l) => {
        setChosen(l.index);
        setText(null);
      }, () => {}) : null} />`}
  </${Modal}>`;
}
