// The tempo map in the editor: a lane of tempo and time signature markers under the marker lane
// (drag to move, double-tap to edit, double-tap the lane to add) and a dialog listing them all.

import { html, useState } from "../vendor/preact-htm.js";
import { formatBpm, formatTime } from "./format.js";
import { Icon, Modal, Switch, dragPointer, useDoubleTap } from "./ui.js";

const DENOMS = [1, 2, 4, 8, 16, 32, 64];

export function tempoLabel(marker) {
  const sig = marker.time_signature ? ` ${marker.time_signature[0]}/${marker.time_signature[1]}` : "";
  return `${formatBpm(marker.bpm)}${sig}`;
}

// The command a marker editor's fields come to: only what changed is sent.
export function tempoEdit(marker, fields) {
  const cmd = { cmd: "tempo.edit", index: marker.index };
  if (fields.time !== undefined && Math.abs(fields.time - marker.time) > 1e-9) cmd.time = fields.time;
  if (fields.bpm !== undefined && fields.bpm !== marker.bpm) cmd.bpm = fields.bpm;
  if (fields.linear !== undefined && fields.linear !== marker.linear) cmd.linear = fields.linear;
  if (fields.timeSignature !== undefined && JSON.stringify(fields.timeSignature) !== JSON.stringify(marker.time_signature ?? null)) {
    cmd.time_signature = fields.timeSignature;
  }
  return cmd;
}

export function TempoDialog({ store, marker, onClose }) {
  const [time, setTime] = useState(marker.time.toFixed(3));
  const [bpm, setBpm] = useState(String(marker.bpm));
  const [hasSig, setHasSig] = useState(!!marker.time_signature);
  const [num, setNum] = useState(marker.time_signature?.[0] ?? 4);
  const [denom, setDenom] = useState(marker.time_signature?.[1] ?? 4);
  const [linear, setLinear] = useState(marker.linear);
  const save = () => {
    const cmd = tempoEdit(marker, {
      time: parseFloat(time),
      bpm: parseFloat(bpm),
      linear,
      timeSignature: hasSig ? [parseInt(num, 10), parseInt(denom, 10)] : null,
    });
    if (Object.keys(cmd).length > 2) store.fire(cmd);
    onClose();
  };
  return html`<${Modal} title=${`Tempo marker ${marker.index + 1} · ${marker.bar_beat}`} onClose=${onClose}>
    <div class="field-row">
      <label class="field"><span>Position (s)</span><input class="mono" type="number" step="0.001" min="0" value=${time}
        onInput=${(e) => setTime(e.target.value)} /></label>
      <label class="field"><span>Tempo (bpm)</span><input class="mono" type="number" step="0.01" min="20" max="400" value=${bpm}
        onInput=${(e) => setBpm(e.target.value)} onKeyDown=${(e) => e.key === "Enter" && save()} /></label>
    </div>
    <label class="switch-row"><${Switch} on=${hasSig} onChange=${setHasSig} title="Starts a new time signature" /><span>New time signature</span></label>
    ${hasSig && html`<div class="field-row">
      <label class="field"><span>Beats per bar</span><input class="mono" type="number" min="1" max="64" value=${num} onInput=${(e) => setNum(e.target.value)} /></label>
      <label class="field"><span>Beat</span><select value=${denom} onChange=${(e) => setDenom(e.target.value)}>
        ${DENOMS.map((d) => html`<option value=${d}>${d}</option>`)}</select></label>
    </div>`}
    <label class="switch-row"><${Switch} on=${linear} onChange=${setLinear} title="Ramp to the next marker's tempo" /><span>Ramp to the next marker</span></label>
    <div class="button-row">
      <button class="btn danger" onClick=${() => {
        store.fire({ cmd: "tempo.delete", index: marker.index });
        onClose();
      }}><${Icon} name="trash" size=${16} />Delete</button>
      <span class="grow"></span>
      <button class="btn primary" onClick=${save}>Save</button>
    </div>
  </${Modal}>`;
}

export function TempoMapDialog({ store, snap, onClose }) {
  const [editing, setEditing] = useState(null);
  const cursor = store.transport?.cursor ?? 0;
  return html`<${Modal} title="Tempo map" wide=${true} onClose=${onClose}>
    <p class="dim small">Starts at ${formatBpm(snap.bpm)} bpm in ${snap.time_signature[0]}/${snap.time_signature[1]}. A time signature
      always starts a bar, so such a marker moves to the next bar line.</p>
    <div class="table">
      ${(snap.tempo || []).map((m) => html`<div class="table-row" key=${m.index}>
        <span class="mono">${m.bar_beat}</span>
        <span class="mono dim">${formatTime(m.time)}</span>
        <span class="mono">${formatBpm(m.bpm)} bpm${m.linear ? " ↗" : ""}</span>
        <span class="mono">${m.time_signature ? `${m.time_signature[0]}/${m.time_signature[1]}` : ""}</span>
        <span class="grow"></span>
        <button class="btn small" onClick=${() => setEditing(m)}>Edit</button>
        <button class="icon-btn" title="Delete" onClick=${() => store.fire({ cmd: "tempo.delete", index: m.index })}><${Icon} name="trash" size=${15} /></button>
      </div>`)}
      ${!snap.tempo?.length && html`<p class="dim">No tempo changes: the whole project runs at one tempo.</p>`}
    </div>
    <div class="button-row">
      <button class="btn" onClick=${() => store.fire({ cmd: "tempo.add", time: cursor })}><${Icon} name="plus" size=${16} />Tempo at the cursor</button>
      <button class="btn" onClick=${() => store.fire({ cmd: "tempo.add", time: cursor, time_signature: snap.time_signature })}>
        <${Icon} name="plus" size=${16} />Time signature at the cursor</button>
    </div>
    ${editing && html`<${TempoDialog} store=${store} marker=${editing} onClose=${() => setEditing(null)} />`}
  </${Modal}>`;
}

function TempoFlag({ store, marker, pps, snapTo, onEdit }) {
  const [drag, setDrag] = useState(null);
  const tap = useDoubleTap(() => onEdit(marker));
  const down = (e) => {
    if (e.button > 0) return;
    e.stopPropagation();
    e.preventDefault();
    if (tap()) return;
    let moved = null;
    dragPointer(
      e,
      ({ dx }) => {
        if (Math.abs(dx) < 3 && moved === null) return;
        moved = snapTo(Math.max(0, marker.time + dx / pps));
        setDrag(moved);
      },
      ({ cancelled }) => {
        setDrag(null);
        // One command on release, so the whole drag is one undo step.
        if (!cancelled && moved !== null && Math.abs(moved - marker.time) > 1e-6) store.fire({ cmd: "tempo.edit", index: marker.index, time: moved });
      },
    );
  };
  const left = (drag ?? marker.time) * pps;
  return html`<div class=${`tempo-flag ${marker.time_signature ? "sig" : ""} ${drag !== null ? "dragging" : ""}`} style=${{ left: `${left}px` }}
      onPointerDown=${down} title=${`${marker.bar_beat}: ${tempoLabel(marker)}\nDrag to move, double-tap to edit`}>
    <span>${tempoLabel(marker)}</span></div>`;
}

export function TempoLane({ store, snap, pps, width, snapTo, onEdit }) {
  const tap = useDoubleTap(() => {});
  const down = (e) => {
    if (e.button > 0 || e.target !== e.currentTarget) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const at = snapTo(Math.max(0, (e.clientX - rect.left) / pps));
    if (tap()) store.fire({ cmd: "tempo.add", time: at });
  };
  return html`<div class="tempo-lane" style=${{ width: `${width}px` }} onPointerDown=${down} title="Double-tap to add a tempo marker">
    ${(snap.tempo || []).map((m) => html`<${TempoFlag} key=${m.index} store=${store} marker=${m} pps=${pps} snapTo=${snapTo} onEdit=${onEdit} />`)}
  </div>`;
}
