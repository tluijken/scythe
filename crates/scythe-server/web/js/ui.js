// Controls shared by the pages, drawn like the desktop app's widgets (ui/widgets.rs).

import { html, useEffect, useRef, useState } from "../vendor/preact-htm.js";
import { clamp, faderGain, faderPosition, formatDb, formatPan } from "./format.js";
import { useMeter } from "./store.js";

const ICONS = {
  start: `<path d="M6 5v14"/><path d="M19 5l-9 7 9 7z"/>`,
  stop: `<rect x="4" y="4" width="16" height="16" rx="3" fill="currentColor" stroke="none"/>`,
  play: `<path d="M7 4.5L20 12 7 19.5z" fill="currentColor" stroke="none"/>`,
  pause: `<rect x="6" y="5" width="4" height="14" rx="1.5" fill="currentColor" stroke="none"/><rect x="14" y="5" width="4" height="14" rx="1.5" fill="currentColor" stroke="none"/>`,
  record: `<circle cx="12" cy="12" r="9" fill="currentColor" stroke="none"/>`,
  loop: `<path d="M17 2l4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="M7 22l-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/>`,
  metronome: `<path d="M9 3h6l4 18H5z"/><path d="M12 15l6-9"/>`,
  search: `<circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5"/>`,
  arrange: `<path d="M3 6h10M3 12h16M3 18h7"/>`,
  mixer: `<path d="M6 3v18M12 3v18M18 3v18"/><rect x="4" y="13" width="4" height="3" rx="1" fill="currentColor"/><rect x="10" y="7" width="4" height="3" rx="1" fill="currentColor"/><rect x="16" y="15" width="4" height="3" rx="1" fill="currentColor"/>`,
  folder: `<path d="M3 6l2-2h4l2 2h8l2 2v10l-2 2H5l-2-2z"/>`,
  fx: `<path d="M9 2v6M15 2v6"/><path d="M6 8h12v4a6 6 0 0 1-12 0z"/><path d="M12 18v4"/>`,
  routing: `<circle cx="6" cy="6" r="2.5"/><circle cx="6" cy="18" r="2.5"/><circle cx="18" cy="12" r="2.5"/><path d="M8.5 6h2l3 3 2 2.8M8.5 18h2l3-3 2-2.8"/>`,
  settings: `<circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9L7 7M17 17l2.1 2.1M4.9 19.1L7 17M17 7l2.1-2.1"/>`,
  plus: `<path d="M12 5v14M5 12h14"/>`,
  minus: `<path d="M5 12h14"/>`,
  previous: `<path d="M15 5l-7 7 7 7"/>`,
  next: `<path d="M9 5l7 7-7 7"/>`,
  undo: `<path d="M9 14L4 9l5-5"/><path d="M4 9h11a5 5 0 0 1 0 10h-3"/>`,
  redo: `<path d="M15 14l5-5-5-5"/><path d="M20 9H9a5 5 0 0 0 0 10h3"/>`,
  save: `<path d="M5 3h11l3 3v15H5z"/><path d="M8 3v5h7V3M8 21v-7h8v7"/>`,
  open: `<path d="M3 6l2-2h4l2 2h8l2 2v2"/><path d="M3 6v12l2 2h13l3-9H7l-2 7"/>`,
  close: `<path d="M6 6l12 12M18 6L6 18"/>`,
  trash: `<path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/>`,
  split: `<path d="M12 3v18"/><path d="M5 8l-3 4 3 4M19 8l3 4-3 4"/>`,
  marker: `<path d="M6 21V4h11l-3 4 3 4H6"/>`,
  power: `<path d="M12 3v9"/><path d="M6.3 7a8 8 0 1 0 11.4 0"/>`,
  zoomIn: `<circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5M8 11h6M11 8v6"/>`,
  zoomOut: `<circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5M8 11h6"/>`,
  menu: `<path d="M4 6h16M4 12h16M4 18h16"/>`,
  up: `<path d="M6 15l6-6 6 6"/>`,
  down: `<path d="M6 9l6 6 6-6"/>`,
  headphones: `<path d="M4 15v-3a8 8 0 0 1 16 0v3"/><rect x="3" y="14" width="5" height="7" rx="2"/><rect x="16" y="14" width="5" height="7" rx="2"/>`,
  list: `<path d="M9 6h11M9 12h11M9 18h11"/><circle cx="4.5" cy="6" r="1" fill="currentColor"/><circle cx="4.5" cy="12" r="1" fill="currentColor"/><circle cx="4.5" cy="18" r="1" fill="currentColor"/>`,
  key: `<circle cx="8" cy="15" r="4"/><path d="M11 12l9-9M17 6l3 3"/>`,
  qr: `<rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><path d="M14 14h3v3h-3zM20 14v.01M14 20h.01M17 20h4v-3"/>`,
  copy: `<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5a1 1 0 0 0-1-1H5a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h3"/>`,
  grip: `<circle cx="9" cy="6" r="1" fill="currentColor"/><circle cx="15" cy="6" r="1" fill="currentColor"/><circle cx="9" cy="12" r="1" fill="currentColor"/><circle cx="15" cy="12" r="1" fill="currentColor"/><circle cx="9" cy="18" r="1" fill="currentColor"/><circle cx="15" cy="18" r="1" fill="currentColor"/>`,
  refresh: `<path d="M20 11a8 8 0 1 0-2.3 5.7"/><path d="M20 4v7h-7"/>`,
  file: `<path d="M6 3h8l4 4v14H6z"/><path d="M14 3v4h4"/>`,
  download: `<path d="M12 4v11M7 10l5 5 5-5M5 20h14"/>`,
};

export function Icon({ name, size = 18, cls = "" }) {
  return html`<svg class=${"icon " + cls} width=${size} height=${size} viewBox="0 0 24 24" fill="none" stroke="currentColor"
    stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
    dangerouslySetInnerHTML=${{ __html: ICONS[name] || "" }}></svg>`;
}

export function IconButton({ icon, title, onClick, cls = "", size = 18, active = false, disabled = false, children }) {
  return html`<button type="button" class=${`icon-btn ${cls} ${active ? "active" : ""}`} title=${title} aria-label=${title}
    disabled=${disabled} onClick=${onClick}><${Icon} name=${icon} size=${size} />${children}</button>`;
}

// Follows one pointer from `down` until it is released, with the pointer captured so the drag
// keeps working outside the control.
export function dragPointer(down, onMove, onEnd) {
  const el = down.currentTarget;
  const id = down.pointerId;
  try {
    el.setPointerCapture(id);
  } catch {
    // Some older Safari builds refuse capture for touch; the drag still works on the element.
  }
  const start = { x: down.clientX, y: down.clientY };
  const move = (e) => {
    if (e.pointerId !== id) return;
    e.preventDefault();
    onMove({ dx: e.clientX - start.x, dy: e.clientY - start.y, event: e });
  };
  const up = (e) => {
    if (e.pointerId !== id) return;
    el.removeEventListener("pointermove", move);
    el.removeEventListener("pointerup", up);
    el.removeEventListener("pointercancel", up);
    onEnd?.({ dx: e.clientX - start.x, dy: e.clientY - start.y, event: e, cancelled: e.type === "pointercancel" });
  };
  el.addEventListener("pointermove", move);
  el.addEventListener("pointerup", up);
  el.addEventListener("pointercancel", up);
}

// Double-tap on touch screens, which do not all send `dblclick`.
export function useDoubleTap(onDouble, ms = 320) {
  const last = useRef(0);
  return () => {
    const now = Date.now();
    if (now - last.current < ms) {
      last.current = 0;
      onDouble();
      return true;
    }
    last.current = now;
    return false;
  };
}

// A fader with REAPER's fader law. Drags move it relative to where it was (a stray touch never
// jumps a level), double-tap sets 0 dB, Shift drags finely. `onInput` follows the drag,
// `onCommit` gets the final gain.
export function Fader({ gain, color = "var(--text-dim)", onInput, onCommit, horizontal = false, length = 116, label = "Volume" }) {
  const [drag, setDrag] = useState(null);
  const shown = drag ?? gain;
  const pos = faderPosition(shown);
  const tap = useDoubleTap(() => {
    onInput?.(1);
    onCommit?.(1);
  });
  const down = (e) => {
    if (e.button > 0) return;
    e.preventDefault();
    if (tap()) return;
    const startPos = faderPosition(gain);
    let latest = gain;
    dragPointer(
      e,
      ({ dx, dy, event }) => {
        const delta = horizontal ? dx : -dy;
        const fine = event.shiftKey ? 0.1 : 1;
        latest = faderGain(clamp(startPos + (delta / length) * fine, 0, 1));
        setDrag(latest);
        onInput?.(latest);
      },
      () => {
        setDrag(null);
        if (latest !== gain) onCommit?.(latest);
      },
    );
  };
  const key = (e) => {
    const step = { ArrowUp: 0.02, ArrowRight: 0.02, ArrowDown: -0.02, ArrowLeft: -0.02 }[e.key];
    if (step === undefined) return;
    e.preventDefault();
    const g = faderGain(clamp(faderPosition(gain) + step, 0, 1));
    onInput?.(g);
    onCommit?.(g);
  };
  const unity = faderPosition(1) * 100;
  const style = horizontal ? { width: "100%" } : { height: `${length}px` };
  return html`<div class=${`fader ${horizontal ? "h" : "v"} ${drag !== null ? "dragging" : ""}`} style=${style}
      role="slider" aria-label=${label} aria-valuetext=${`${formatDb(shown)} dB`} tabindex="0"
      onPointerDown=${down} onKeyDown=${key}>
    <div class="fader-fill" style=${horizontal ? { width: `${pos * 100}%`, background: color } : { height: `${pos * 100}%`, background: color }}></div>
    <div class="fader-unity" style=${horizontal ? { left: `${unity}%` } : { bottom: `${unity}%` }}></div>
    <div class="fader-cap" style=${horizontal ? { left: `${pos * 100}%` } : { bottom: `${pos * 100}%` }}></div>
  </div>`;
}

// A pan slider: relative drag, double-tap to centre.
export function Pan({ pan, onInput, onCommit, width = 88 }) {
  const [drag, setDrag] = useState(null);
  const shown = drag ?? pan;
  const tap = useDoubleTap(() => {
    onInput?.(0);
    onCommit?.(0);
  });
  const down = (e) => {
    if (e.button > 0) return;
    e.preventDefault();
    if (tap()) return;
    let latest = pan;
    dragPointer(
      e,
      ({ dx, event }) => {
        latest = Math.round(clamp(pan + ((dx / width) * 2 * (event.shiftKey ? 0.1 : 1)), -1, 1) * 100) / 100;
        setDrag(latest);
        onInput?.(latest);
      },
      () => {
        setDrag(null);
        if (latest !== pan) onCommit?.(latest);
      },
    );
  };
  return html`<div class="pan" role="slider" aria-label="Pan" aria-valuetext=${formatPan(shown)} title="Drag to pan, double-tap to centre"
      onPointerDown=${down}>
    <div class="pan-track"><div class="pan-centre"></div><div class="pan-knob" style=${{ left: `${((shown + 1) / 2) * 100}%` }}></div></div>
  </div>`;
}

// Stereo peak meter following `read()`; `axis="x"` lays it on its side.
export function Meter({ read, axis = "y", cls = "" }) {
  const ref = useMeter(read);
  return html`<div class=${`meter ${axis === "x" ? "mx" : "my"} ${cls}`} data-axis=${axis} ref=${ref}>
    <div class="meter-bar"><div class="meter-fill"></div></div><div class="meter-bar"><div class="meter-fill"></div></div>
  </div>`;
}

// A rotary knob over `range`, turned by dragging up or down.
export function Knob({ value, range, onInput, onCommit, size = 180, label, text, defaultValue = 0 }) {
  const [drag, setDrag] = useState(null);
  const shown = drag ?? value;
  const [lo, hi] = range;
  const frac = clamp((shown - lo) / (hi - lo), 0, 1);
  const zero = clamp((defaultValue - lo) / (hi - lo), 0, 1);
  const tap = useDoubleTap(() => {
    onInput?.(defaultValue);
    onCommit?.(defaultValue);
  });
  const down = (e) => {
    if (e.button > 0) return;
    e.preventDefault();
    if (tap()) return;
    let latest = value;
    dragPointer(
      e,
      ({ dy, dx }) => {
        const travel = -dy + dx * 0.5;
        latest = Math.round(clamp(value + (travel / (size * 1.6)) * (hi - lo), lo, hi) * 2) / 2;
        setDrag(latest);
        onInput?.(latest);
      },
      () => {
        setDrag(null);
        if (latest !== value) onCommit?.(latest);
      },
    );
  };
  const r = 42;
  const angle = (f) => Math.PI * 0.75 + Math.PI * 1.5 * f;
  const point = (f, rad = r) => [50 + Math.cos(angle(f)) * rad, 50 + Math.sin(angle(f)) * rad];
  const arc = (a, b) => {
    const [x0, y0] = point(a);
    const [x1, y1] = point(b);
    const large = (b - a) * 270 > 180 ? 1 : 0;
    return `M${x0} ${y0} A${r} ${r} 0 ${large} 1 ${x1} ${y1}`;
  };
  const [px0, py0] = point(frac, r - 18);
  const [px1, py1] = point(frac, r - 6);
  return html`<div class="knob" style=${{ width: `${size}px` }} role="slider" aria-label=${label} aria-valuetext=${text}
      onPointerDown=${down}>
    <svg viewBox="0 0 100 100" width=${size} height=${size}>
      <circle cx="50" cy="50" r=${r - 5} class="knob-body" />
      <path d=${arc(0, 1)} class="knob-track" />
      ${Math.abs(frac - zero) > 0.001 && html`<path d=${arc(Math.min(frac, zero), Math.max(frac, zero))} class="knob-value" />`}
      <line x1=${px0} y1=${py0} x2=${px1} y2=${py1} class="knob-pointer" />
    </svg>
    <div class="knob-text">${text}</div>
    <div class="knob-label">${label}</div>
  </div>`;
}

export function Switch({ on, onChange, title }) {
  return html`<button type="button" class=${`switch ${on ? "on" : ""}`} role="switch" aria-checked=${on} title=${title}
    onClick=${() => onChange(!on)}><span></span></button>`;
}

export function Modal({ title, onClose, children, wide = false, cls = "" }) {
  useEffect(() => {
    const key = (e) => e.key === "Escape" && onClose();
    addEventListener("keydown", key);
    return () => removeEventListener("keydown", key);
  }, [onClose]);
  return html`<div class="modal-backdrop" onPointerDown=${(e) => e.target === e.currentTarget && onClose()}>
    <div class=${`modal ${wide ? "wide" : ""} ${cls}`} role="dialog" aria-label=${title}>
      <div class="modal-head"><h2>${title}</h2><${IconButton} icon="close" title="Close" onClick=${onClose} /></div>
      <div class="modal-body">${children}</div>
    </div>
  </div>`;
}

export function Toasts({ store }) {
  const [toasts, setToasts] = useState([]);
  useEffect(
    () =>
      store.on("notice", (n) => {
        setToasts((t) => [...t.slice(-2), n]);
        setTimeout(() => setToasts((t) => t.filter((x) => x !== n)), n.kind === "error" ? 5000 : 2500);
      }),
    [store],
  );
  return html`<div class="toasts" aria-live="polite">${toasts.map((t) => html`<div class=${`toast ${t.kind}`} key=${t.at}>${t.message}</div>`)}</div>`;
}

const STATUS_TEXT = {
  open: "Live",
  connecting: "Connecting…",
  reconnecting: "Reconnecting…",
  unauthorized: "Token needed",
  idle: "Offline",
};

export function StatusPill({ status, compact = false }) {
  return html`<div class=${`status-pill ${status}`} title=${STATUS_TEXT[status] || status}>
    <span class="dot"></span>${!compact && html`<span>${STATUS_TEXT[status] || status}</span>`}
  </div>`;
}

// Shown over a page while the connection is down, so nobody trusts stale levels on stage.
export function OfflineVeil({ status }) {
  if (status === "open" || status === "idle") return null;
  return html`<div class="offline-veil"><div class="offline-card"><div class="spinner"></div>${STATUS_TEXT[status]}</div></div>`;
}

export function Logo({ size = 40 }) {
  return html`<div class="logo" style=${{ width: `${size}px`, height: `${size}px`, borderRadius: `${size * 0.3}px` }}><img src="/glyph.svg" alt="" /></div>`;
}

// An inline text field that commits on Enter or blur and gives up on Escape.
export function EditableText({ value, onCommit, cls = "", placeholder = "" }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(value);
  const input = useRef(null);
  useEffect(() => {
    if (editing) {
      input.current?.focus();
      input.current?.select();
    }
  }, [editing]);
  if (!editing) {
    return html`<span class=${`editable ${cls}`} title="Double-click to rename" onDblClick=${() => {
      setText(value);
      setEditing(true);
    }}>${value || html`<span class="dim">${placeholder}</span>`}</span>`;
  }
  const done = (commit) => {
    setEditing(false);
    if (commit && text !== value) onCommit(text);
  };
  return html`<input class=${`editable-input ${cls}`} ref=${input} value=${text} onInput=${(e) => setText(e.target.value)}
    onKeyDown=${(e) => {
      if (e.key === "Enter") done(true);
      if (e.key === "Escape") done(false);
      e.stopPropagation();
    }} onBlur=${() => done(true)} />`;
}
