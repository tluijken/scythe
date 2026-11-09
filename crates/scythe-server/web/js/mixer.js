// The mixer dock: one strip per track and the master, as in the desktop app's mixer.

import { html } from "../vendor/preact-htm.js";
import { formatDb, formatPan } from "./format.js";
import { Fader, Meter, Pan } from "./ui.js";

const trackRef = (id) => (id === 0 ? "master" : id);

// Sends a continuous control (fader, pan) while it moves, only the newest value in flight. The
// control shows its own value while dragged; the server's answer is patched in so it does not
// jump back before the next snapshot.
export function liveControl(store, track, field, cmd, value) {
  const command = field === "pan" ? { cmd, track: trackRef(track.id), pan: value } : { cmd, track: trackRef(track.id), volume: value };
  store.run(command, { latest: `${cmd}:${track.id}` }).then((info) => store.patchTrack(info), () => {});
}

function Strip({ store, track, master = false, selected, onSelect }) {
  const read = master ? () => store.meters.master : () => store.meters.tracks.get(track.id);
  const color = master ? "var(--text)" : track.color || "var(--text-dim)";
  const flag = (cmd, field) => {
    const value = !track[field];
    store.patchTrack({ id: track.id, [field]: value });
    store.fire({ cmd, track: trackRef(track.id), [field]: value });
  };
  return html`<div class=${`strip ${selected ? "selected" : ""} ${master ? "master" : ""}`}>
    <div class="strip-head" onClick=${onSelect}>
      <span class="dot" style=${{ background: color }}></span><span class="strip-name">${master ? "Master" : track.name || `Track ${track.index + 1}`}</span>
    </div>
    <${Pan} pan=${track.pan} width=${80} onInput=${(p) => liveControl(store, track, "pan", "track.pan", p)} />
    <div class="strip-pan mono">${formatPan(track.pan)}</div>
    <div class="strip-fader">
      <${Fader} gain=${track.volume} color=${color} length=${104}
        onInput=${(g) => liveControl(store, track, "volume", "track.volume", g)} />
      <${Meter} read=${read} />
    </div>
    <div class="strip-db mono">${formatDb(track.volume)} dB</div>
    <div class="strip-flags">
      <button class=${`flag mute ${track.muted ? "on" : ""}`} title="Mute" onClick=${() => flag("track.mute", "muted")}>M</button>
      ${!master && html`<button class=${`flag solo ${track.solo ? "on" : ""}`} title="Solo" onClick=${() => flag("track.solo", "solo")}>S</button>`}
    </div>
  </div>`;
}

export function Mixer({ store, snap, sel, setSel }) {
  return html`<section class="mixer panel">
    <div class="panel-tools"><span class="panel-title">Mixer</span><span class="grow"></span><span class="dim small mono">Post-fader · peak</span></div>
    <div class="strips">
      ${snap.tracks.map(
        (t) => html`<${Strip} key=${t.id} store=${store} track=${t} selected=${sel.track === t.id} onSelect=${() => setSel({ track: t.id, item: null })} />`,
      )}
      <${Strip} store=${store} track=${snap.master} master=${true} selected=${sel.track === 0} onSelect=${() => setSel({ track: 0, item: null })} />
    </div>
  </section>`;
}
