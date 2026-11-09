// Access links for the editor: a musician's link reaches only their monitor mix, the stage's
// only the setlist. Each shows as a QR code for a phone to scan; the server holds the links and
// enforces what they allow.

import { html, useEffect, useState } from "../vendor/preact-htm.js";
import { accessCommands, linkUrl } from "./domains.js";
import { qrMatrix, qrPath } from "./qr.js";
import { EditableText, Icon, IconButton, Modal } from "./ui.js";

const BASE_KEY = "scythe.linkBase";

export function QrCode({ text, size = 220 }) {
  let matrix;
  try {
    matrix = qrMatrix(text);
  } catch {
    return html`<p class="dim">Too long for a QR code.</p>`;
  }
  const n = matrix.length + 8;
  return html`<svg class="qr" viewBox=${`0 0 ${n} ${n}`} width=${size} height=${size} role="img" aria-label=${`QR code for ${text}`}
    shape-rendering="crispEdges"><rect width=${n} height=${n} fill="#fff" /><path d=${qrPath(matrix)} fill="#000" /></svg>`;
}

function savedBase() {
  try {
    return localStorage.getItem(BASE_KEY) || "";
  } catch {
    return "";
  }
}

function saveBase(base) {
  try {
    if (base) localStorage.setItem(BASE_KEY, base);
    else localStorage.removeItem(BASE_KEY);
  } catch {
    // Not remembered in private browsing.
  }
}

async function copy(store, text) {
  try {
    await navigator.clipboard.writeText(text);
    store.notify("Link copied");
  } catch {
    prompt("Copy the link", text);
  }
}

function LinkRow({ store, link, url, open, onToggle, onChange }) {
  const run = (command, message) =>
    store.run(command).then((answer) => {
      onChange();
      if (message) store.notify(message);
      return answer;
    }, () => {});
  const who = link.role === "musician" ? `${link.musician}'s monitor mix` : "Setlist and play/stop";
  return html`<div class=${`link ${open ? "open" : ""}`}>
    <div class="link-head">
      <${Icon} name=${link.role === "musician" ? "headphones" : "list"} size=${18} />
      <div class="link-text">
        <${EditableText} cls="link-name" value=${link.name} onCommit=${(name) => run(accessCommands.rename(link.id, name))} />
        <span class="dim small">${who}</span>
      </div>
      <${IconButton} icon="qr" title="Show the QR code" active=${open} onClick=${onToggle} />
      <${IconButton} icon="copy" title="Copy the link" onClick=${() => copy(store, url)} />
      <${IconButton} icon="refresh" title="New token: the old link stops working"
        onClick=${() => confirm(`Give "${link.name}" a new token? Phones with the old link lose access.`) && run(accessCommands.renew(link.id), "New token made")} />
      <${IconButton} icon="trash" title="Delete the link"
        onClick=${() => confirm(`Delete "${link.name}"? Phones with it lose access at once.`) && run(accessCommands.remove(link.id), "Link deleted")} />
    </div>
    ${open && html`<div class="link-qr"><${QrCode} text=${url} /><a class="mono small link-url" href=${url} target="_blank" rel="noreferrer">${url}</a></div>`}
  </div>`;
}

// `focus` names a musician whose link should be shown (made first when there is none).
export function AccessDialog({ store, snap, onClose, focus = null }) {
  const [data, setData] = useState(null);
  const [open, setOpen] = useState(null);
  const [musician, setMusician] = useState(focus || snap.mixes?.[0]?.musician || "");
  const [base, setBase] = useState(savedBase);
  const load = () => store.run(accessCommands.list()).then(setData, () => {});
  useEffect(() => {
    load().then(async () => {
      if (!focus) return;
      const list = await store.run(accessCommands.list());
      let link = list.links.find((l) => l.role === "musician" && l.musician.toLowerCase() === focus.toLowerCase());
      if (!link) link = await store.run(accessCommands.createMusician(focus));
      setOpen(link.id);
      load();
    }).catch(() => {});
  }, []);
  const url = (link) => {
    try {
      if (base) return new URL(link.path, base).toString();
    } catch {
      // Not an address yet while it is being typed.
    }
    return linkUrl(link, data?.origins);
  };
  const create = (command) =>
    store.run(command).then((link) => {
      setOpen(link.id);
      load();
    }, () => {});
  const mixes = snap.mixes || [];
  return html`<${Modal} title="Access links" wide=${true} onClose=${onClose}>
    ${data && !data.admin_token && html`<p class="note warn">This Scythe runs without an admin token, so anyone on the network has full control
      anyway. Start it with <code>--token</code> for the links to mean something.</p>`}
    <p class="dim small">Scan a code with a phone or tablet. A musician's link reaches only their own monitor mix; the stage link plays the
      setlist without editing it. Scythe checks every command, whatever the page does.</p>
    <div class="link-create">
      <select value=${musician} onChange=${(e) => setMusician(e.target.value)} disabled=${!mixes.length}>
        ${mixes.length ? mixes.map((m) => html`<option value=${m.musician}>${m.musician}</option>`) : html`<option>No monitor mixes yet</option>`}
      </select>
      <button class="btn" disabled=${!musician || !mixes.length} onClick=${() => create(accessCommands.createMusician(musician))}>
        <${Icon} name="headphones" size=${16} />Musician link</button>
      <button class="btn" onClick=${() => create(accessCommands.createStage())}><${Icon} name="list" size=${16} />Stage link</button>
    </div>
    <div class="links">
      ${data?.links.map((link) => html`<${LinkRow} key=${link.id} store=${store} link=${link} url=${url(link)} open=${open === link.id}
        onToggle=${() => setOpen(open === link.id ? null : link.id)} onChange=${load} />`)}
      ${data && !data.links.length && html`<p class="dim">No links yet.</p>`}
    </div>
    <label class="field"><span>Address phones use${data?.origins?.length ? "" : " (this page's address when empty)"}</span>
      <input class="mono" value=${base} placeholder=${data?.origins?.[0] || location.origin} spellcheck="false"
        onChange=${(e) => {
          const next = e.target.value.trim();
          setBase(next);
          saveBase(next);
        }} /></label>
    ${!data?.stored && data && html`<p class="dim small">This server keeps links in memory only: they end when it stops.</p>`}
  </${Modal}>`;
}
