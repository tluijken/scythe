// Monitor mixes in the editor: the list with output, limiter, sources and links, and the wizard
// that makes one, as in the desktop app: musician, sources, their own sources, output.

import { html, useState } from "../vendor/preact-htm.js";
import { AccessDialog } from "./access.js";
import { EditableText, Icon, IconButton, Modal, Switch } from "./ui.js";

const OUTPUTS = 32;

// Device outputs to offer: stereo pairs and single channels, `taken` (first outputs of other
// mixes) marked.
export function outputChoices(taken = []) {
  const choices = [];
  for (let n = 0; n < OUTPUTS; n += 2) choices.push({ value: `s${n}`, output: n, mono: false, label: `Out ${n + 1}/${n + 2}`, taken: taken.includes(n) });
  for (let n = 0; n < OUTPUTS; n++) choices.push({ value: `m${n}`, output: n, mono: true, label: `Out ${n + 1} (mono)`, taken: taken.includes(n) });
  return choices;
}

export function outputValue(output) {
  return output ? `${output.mono ? "m" : "s"}${output.output}` : "";
}

// The tracks a mix can take: not monitor mixes, not folder parents (their children are in).
export function mixCandidates(snap) {
  const mixes = new Set((snap.mixes || []).map((m) => m.id));
  return snap.tracks.filter((t) => !mixes.has(t.id) && t.folder_depth <= 0);
}

export function createCommand(form) {
  const cmd = { cmd: "mix.create", musician: form.musician.trim(), sources: form.sources, own: form.own.filter((id) => form.sources.includes(id)) };
  if (form.output) {
    cmd.output = form.output.output;
    cmd.mono = form.output.mono;
  }
  cmd.limiter = form.limiter;
  if (form.levelsFromTracks) cmd.levels_from_tracks = true;
  if (form.pansFromTracks) cmd.pans_from_tracks = true;
  return cmd;
}

function OutputSelect({ value, taken, onChange, auto = false }) {
  const choices = outputChoices(taken);
  return html`<select value=${value} onChange=${(e) => onChange(choices.find((c) => c.value === e.target.value) || null)}>
    <option value="">${auto ? "The next free output" : "None"}</option>
    ${choices.map((c) => html`<option value=${c.value}>${c.label}${c.taken ? " · in use" : ""}</option>`)}
  </select>`;
}

export const toggled = (list, id) => (list.includes(id) ? list.filter((x) => x !== id) : [...list, id]);

function TrackChecks({ tracks, chosen, onToggle }) {
  return html`<div class="checks">
    ${tracks.map((t) => html`<label class="check" key=${t.id}><input type="checkbox" checked=${chosen.includes(t.id)} onChange=${() => onToggle(t.id)} />
      <span class="dot" style=${{ background: t.color || "var(--text-dim)" }}></span><span class="ellipsis">${t.name || `Track ${t.index + 1}`}</span></label>`)}
  </div>`;
}

const STEPS = ["Musician", "Sources", "Own sources", "Output"];

export function MixWizard({ store, snap, onClose, onCreated }) {
  const candidates = mixCandidates(snap);
  const taken = (snap.mixes || []).map((m) => m.output?.output).filter((n) => n != null);
  const [step, setStep] = useState(0);
  const [busy, setBusy] = useState(false);
  const [form, setForm] = useState({
    musician: "",
    sources: candidates.map((t) => t.id),
    own: [],
    output: null,
    limiter: true,
    levelsFromTracks: false,
    pansFromTracks: false,
  });
  const set = (fields) => setForm((f) => ({ ...f, ...fields }));
  const exists = (snap.mixes || []).some((m) => m.musician.toLowerCase() === form.musician.trim().toLowerCase());
  const ready = [form.musician.trim() && !exists, form.sources.length > 0, true, true];
  const create = async () => {
    setBusy(true);
    try {
      const mix = await store.run(createCommand(form));
      store.notify(`Monitor mix for ${mix.musician} made`);
      onCreated?.(mix);
      onClose();
    } catch {
      setBusy(false);
    }
  };
  const body = [
    html`<label class="field"><span>Whose mix is it?</span><input value=${form.musician} placeholder="Drummer" autofocus
      onInput=${(e) => set({ musician: e.target.value })} onKeyDown=${(e) => e.key === "Enter" && ready[0] && setStep(1)} /></label>
      ${exists && html`<p class="note warn">There is a mix for ${form.musician.trim()} already.</p>`}
      <p class="dim small">The mix is a track named "MON ${form.musician.trim() || "…"}" that hears its sources before their faders.</p>`,
    html`<p class="dim small">What ${form.musician.trim()} hears.</p>
      <div class="button-row"><button class="btn small" onClick=${() => set({ sources: candidates.map((t) => t.id) })}>All</button>
        <button class="btn small" onClick=${() => set({ sources: [] })}>None</button></div>
      <${TrackChecks} tracks=${candidates} chosen=${form.sources} onToggle=${(id) => setForm((f) => ({ ...f, sources: toggled(f.sources, id) }))} />`,
    html`<p class="dim small">${form.musician.trim()}'s own sources, which "more me" raises.</p>
      <${TrackChecks} tracks=${candidates.filter((t) => form.sources.includes(t.id))} chosen=${form.own} onToggle=${(id) => setForm((f) => ({ ...f, own: toggled(f.own, id) }))} />`,
    html`<label class="field"><span>Device output</span>
        <${OutputSelect} value=${outputValue(form.output)} taken=${taken} auto=${true} onChange=${(output) => set({ output })} /></label>
      <label class="switch-row"><${Switch} on=${form.limiter} onChange=${(limiter) => set({ limiter })} title="Safety limiter" />
        <span>Safety limiter on the mix (protects ears from a feedback squeal)</span></label>
      <label class="switch-row"><${Switch} on=${form.levelsFromTracks} onChange=${(levelsFromTracks) => set({ levelsFromTracks })} title="Levels from tracks" />
        <span>Start from the tracks' fader levels instead of 0 dB</span></label>
      <label class="switch-row"><${Switch} on=${form.pansFromTracks} onChange=${(pansFromTracks) => set({ pansFromTracks })} title="Pans from tracks" />
        <span>Start from the tracks' pans instead of centre</span></label>`,
  ];
  return html`<${Modal} title="New monitor mix" wide=${true} onClose=${onClose}>
    <ol class="steps">${STEPS.map((s, i) => html`<li class=${i === step ? "on" : i < step ? "done" : ""}>${s}</li>`)}</ol>
    ${body[step]}
    <div class="button-row">
      ${step > 0 && html`<button class="btn" onClick=${() => setStep(step - 1)}>Back</button>`}
      <span class="grow"></span>
      ${step < STEPS.length - 1
        ? html`<button class="btn primary" disabled=${!ready[step]} onClick=${() => setStep(step + 1)}>Next</button>`
        : html`<button class="btn primary" disabled=${busy || !ready[0] || !ready[1]} onClick=${create}>Make the mix</button>`}
    </div>
  </${Modal}>`;
}

function MixSources({ store, snap, mix }) {
  const inMix = new Set(mix.sources.map((s) => s.track));
  const addable = mixCandidates(snap).filter((t) => !inMix.has(t.id));
  return html`<div class="mix-sources">
    ${mix.sources.map((s) => html`<div class="mix-source" key=${s.track}>
      <span class="dot" style=${{ background: s.color || "var(--text-dim)" }}></span>
      <span class="ellipsis grow">${s.name || `Track ${(snap.tracks.find((t) => t.id === s.track)?.index ?? 0) + 1}`}</span>
      <span class="mono dim small">${s.volume_db.toFixed(1)} dB</span>
      <label class="check small"><input type="checkbox" checked=${s.own} onChange=${() => store.fire({ cmd: "mix.own", mix: mix.id, source: s.track, own: !s.own })} />own</label>
      <${IconButton} icon="close" title="Take it out of the mix" onClick=${() => store.fire({ cmd: "mix.remove_source", mix: mix.id, source: s.track })} />
    </div>`)}
    ${addable.length > 0 && html`<select value="" onChange=${(e) => e.target.value && store.fire({ cmd: "mix.add_source", mix: mix.id, source: parseInt(e.target.value, 10) })}>
      <option value="">Add a source…</option>
      ${addable.map((t) => html`<option value=${t.id}>${t.name || `Track ${t.index + 1}`}</option>`)}
    </select>`}
  </div>`;
}

function MixCard({ store, snap, mix, onLink }) {
  const [open, setOpen] = useState(false);
  const taken = (snap.mixes || []).filter((m) => m.id !== mix.id).map((m) => m.output?.output).filter((n) => n != null);
  const output = (choice) => store.fire({ cmd: "mix.output", mix: mix.id, output: choice ? choice.output : null, mono: !!choice?.mono });
  return html`<div class="mix-card">
    <div class="mix-card-head">
      <span class="dot" style=${{ background: mix.color || "var(--orange)" }}></span>
      <${EditableText} cls="mix-card-name" value=${mix.musician} onCommit=${(musician) => store.fire({ cmd: "mix.rename", mix: mix.id, musician })} />
      <span class="dim small">${mix.sources.length} sources${mix.more_me ? ` · more me ${mix.more_me > 0 ? "+" : ""}${mix.more_me} dB` : ""}</span>
      <span class="grow"></span>
      <${IconButton} icon="qr" title=${`Link for ${mix.musician}'s phone`} onClick=${() => onLink(mix.musician)} />
      <${IconButton} icon="copy" title="Copy for another musician" onClick=${() => {
        const musician = prompt(`Copy ${mix.musician}'s mix for:`);
        if (musician?.trim()) store.fire({ cmd: "mix.copy", mix: mix.id, musician: musician.trim() });
      }} />
      <${IconButton} icon="trash" title="Delete the mix" onClick=${() => confirm(`Delete ${mix.musician}'s monitor mix?`) && store.fire({ cmd: "mix.delete", mix: mix.id })} />
    </div>
    <div class="mix-card-row">
      <label class="field"><span>Output</span><${OutputSelect} value=${outputValue(mix.output)} taken=${taken} onChange=${output} /></label>
      <label class="switch-row"><${Switch} on=${!!mix.limiter} onChange=${(on) => store.fire({ cmd: "mix.limiter", mix: mix.id, on })} title="Safety limiter" /><span>Limiter</span></label>
      <button class=${`btn small ${open ? "toggle on" : ""}`} onClick=${() => setOpen(!open)}>Sources</button>
    </div>
    ${open && html`<${MixSources} store=${store} snap=${snap} mix=${mix} />`}
  </div>`;
}

export function MixManager({ store, snap, onClose }) {
  const [wizard, setWizard] = useState(false);
  const [link, setLink] = useState(null);
  const mixes = snap.mixes || [];
  return html`<${Modal} title="Monitor mixes" wide=${true} onClose=${onClose}>
    <div class="button-row">
      <button class="btn primary" onClick=${() => setWizard(true)}><${Icon} name="plus" size=${16} />New mix</button>
      <span class="grow"></span>
      <a class="btn" href="/monitor" target="_blank" rel="noreferrer"><${Icon} name="headphones" size=${16} />Monitor page</a>
    </div>
    <div class="mix-cards">
      ${mixes.map((m) => html`<${MixCard} key=${m.id} store=${store} snap=${snap} mix=${m} onLink=${setLink} />`)}
      ${!mixes.length && html`<p class="dim">No monitor mixes yet. Each musician gets one: a track feeding their in-ear output.</p>`}
    </div>
    ${wizard && html`<${MixWizard} store=${store} snap=${snap} onClose=${() => setWizard(false)} />`}
    ${link && html`<${AccessDialog} store=${store} snap=${snap} focus=${link} onClose=${() => setLink(null)} />`}
  </${Modal}>`;
}
