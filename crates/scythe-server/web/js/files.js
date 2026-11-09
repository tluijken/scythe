// Opening and saving projects: a browser over the folders the server offers (`GET /api/files`,
// which never lists anything outside them), plus new and save.

import { html, useEffect, useState } from "../vendor/preact-htm.js";
import { Icon, Modal } from "./ui.js";

// `name` as a project file in `dir`: `.RPP` added unless it ends in it already.
export function savePath(dir, name) {
  const clean = name.trim().replace(/[\\/]/g, "-");
  const file = /\.rpp$/i.test(clean) ? clean : `${clean}.RPP`;
  return `${dir.replace(/\/+$/, "")}/${file}`;
}

// The folders from a root down to `path`, for the breadcrumbs.
export function crumbs(roots, path) {
  if (!path) return [];
  const root = roots.find((r) => path === r.path || path.startsWith(r.path.replace(/\/+$/, "") + "/"));
  if (!root) return [{ name: path, path }];
  const rest = path.slice(root.path.replace(/\/+$/, "").length).split("/").filter(Boolean);
  const out = [{ name: root.name, path: root.path }];
  let at = root.path.replace(/\/+$/, "");
  for (const part of rest) {
    at = `${at}/${part}`;
    out.push({ name: part, path: at });
  }
  return out;
}

function formatDate(seconds) {
  if (!seconds) return "";
  return new Date(seconds * 1000).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

export function FileDialog({ store, snap, onClose }) {
  const startDir = snap.path ? snap.path.replace(/\/[^/]*$/, "") : null;
  const [listing, setListing] = useState(null);
  const [selected, setSelected] = useState(null);
  const [name, setName] = useState(snap.name || "Untitled");
  const [busy, setBusy] = useState(false);
  const browse = (path) =>
    store.client
      .http("GET", path ? `/api/files?path=${encodeURIComponent(path)}` : "/api/files")
      .then((l) => {
        setListing(l);
        setSelected(null);
      })
      .catch((e) => {
        if (path) browse(null);
        else store.notify(e.message, "error");
      });
  useEffect(() => {
    browse(startDir);
  }, []);
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
  const open = (path) => {
    if (snap.dirty && !confirm("Drop the unsaved changes and open another project?")) return;
    act({ cmd: "project.open", path }, "Opened");
  };
  const dir = listing?.path;
  return html`<${Modal} title="Project" wide=${true} onClose=${onClose}>
    <div class="button-row">
      <button class="btn" disabled=${busy || !snap.path} onClick=${() => act({ cmd: "project.save" }, "Saved")}><${Icon} name="save" size=${16} />Save</button>
      <span class="grow"></span>
      <button class="btn danger" disabled=${busy} onClick=${() => {
        if (!snap.dirty || confirm("Drop the unsaved changes and start an empty project?")) act({ cmd: "project.new" }, "New project");
      }}>New</button>
    </div>
    <div class="crumbs">
      <button class="crumb" onClick=${() => browse(null)}>Folders</button>
      ${listing && crumbs(listing.roots, dir).map((c) => html`<span class="dim">/</span><button class="crumb" onClick=${() => browse(c.path)}>${c.name}</button>`)}
    </div>
    <div class="files">
      ${listing?.parent && html`<button class="file-row" onClick=${() => browse(listing.parent)}><${Icon} name="up" size=${16} /><span>..</span></button>`}
      ${listing?.entries.map((e) => html`<button key=${e.path} class=${`file-row ${selected === e.path ? "on" : ""} ${e.path === snap.path ? "current" : ""}`}
          onClick=${() => (e.dir ? browse(e.path) : setSelected(e.path))} onDblClick=${() => !e.dir && open(e.path)}>
        <${Icon} name=${e.dir ? "folder" : "file"} size=${16} />
        <span class="ellipsis grow">${e.name}</span>
        <span class="dim small">${e.dir ? "" : formatDate(e.modified)}</span>
      </button>`)}
      ${listing && !listing.entries.length && html`<p class="dim small">No folders or projects here.</p>`}
      ${!listing && html`<p class="dim small">Loading…</p>`}
    </div>
    <div class="button-row">
      <button class="btn primary" disabled=${busy || !selected} onClick=${() => open(selected)}><${Icon} name="open" size=${16} />Open</button>
    </div>
    ${dir && html`<div class="save-as">
      <label class="field"><span>Save a copy here as</span><input value=${name} onInput=${(e) => setName(e.target.value)} spellcheck="false" /></label>
      <button class="btn" disabled=${busy || !name.trim()} onClick=${() => {
        const path = savePath(dir, name);
        const exists = listing.entries.some((e) => e.path === path);
        if (!exists || confirm(`${path.split("/").pop()} exists. Overwrite it?`)) act({ cmd: "project.save", path }, "Saved");
      }}><${Icon} name="save" size=${16} />Save as</button>
    </div>`}
    <p class="dim small">Folders are the ones Scythe was started with (<code>--folder</code>), on the machine running it.</p>
  </${Modal}>`;
}
