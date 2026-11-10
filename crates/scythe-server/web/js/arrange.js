// The arrange view: track headers, items on a timeline, the ruler and the marker lane. One
// native scroll area, so touch scrolling and momentum work as on any page; headers and the
// ruler stay in view with `position: sticky`.

import { html, useEffect, useLayoutEffect, useRef, useState } from "../vendor/preact-htm.js";
import { folderParents, moveCommand, rowShows, trackDrop, withChildren } from "./domains.js";
import { barIndexAt, barStep, clamp, formatDb, formatTime, SWATCHES } from "./format.js";
import { useStoreValue } from "./store.js";
import { TempoDialog, TempoLane, TempoMapDialog } from "./tempo.js";
import { EditableText, Icon, IconButton, Modal, dragPointer, useDoubleTap } from "./ui.js";
import { PeakCache, columnsFor, drawPeaks } from "./waveform.js";

const HEADER_W = 236;
const RULER_H = 30;
const MARKER_H = 26;
const TEMPO_H = 24;
const TOP_H = RULER_H + MARKER_H + TEMPO_H;
const ROW_HEIGHTS = [44, 64, 92];
const SMALL_H = 26;
const MIN_PPS = 0.5;
const MAX_PPS = 800;

// The nearest beat: bars split evenly by the time signature's numerator.
function snapTime(t, bars, beats) {
  const i = barIndexAt(bars, t);
  if (i < 0) return 0;
  const start = bars[i];
  const end = bars[i + 1] ?? start + (start - (bars[i - 1] ?? 0) || 2);
  const step = (end - start) / beats;
  const k = Math.round((t - start) / step);
  return start + k * step;
}

function Playhead({ store, pps }) {
  const t = useStoreValue(store, "transport");
  if (!t) return null;
  return html`<div class=${`playhead ${t.playing ? "moving" : ""}`} style=${{ transform: `translateX(${HEADER_W + t.position * pps}px)` }}></div>
    ${t.playing && html`<div class="edit-cursor" style=${{ transform: `translateX(${HEADER_W + t.cursor * pps}px)` }}></div>`}`;
}

function LoopRange({ store, pps }) {
  const t = useStoreValue(store, "transport");
  if (!t?.loop_range) return null;
  const [a, b] = t.loop_range;
  return html`<div class=${`loop-range ${t.loop_enabled ? "on" : ""}`} style=${{ left: `${HEADER_W + a * pps}px`, width: `${(b - a) * pps}px` }}></div>`;
}

function ColorPicker({ store, track, onClose }) {
  return html`<${Modal} title=${`Colour of ${track.name}`} onClose=${onClose}>
    <div class="swatches">
      ${SWATCHES.map((c) => html`<button class=${`swatch ${track.color === c ? "on" : ""}`} style=${{ background: c }} title=${c}
        onClick=${() => {
          store.fire({ cmd: "track.color", track: track.id, color: c });
          onClose();
        }}></button>`)}
      <button class="swatch none" title="No colour" onClick=${() => {
        store.fire({ cmd: "track.color", track: track.id, color: null });
        onClose();
      }}><${Icon} name="close" size=${14} /></button>
    </div>
  </${Modal}>`;
}

export function FlagButtons({ store, track, compact = false }) {
  const flag = (cmd, field, value) => {
    store.patchTrack({ id: track.id, [field]: value });
    store.fire({ cmd, track: track.id, [field]: value });
  };
  return html`<div class=${`flags ${compact ? "compact" : ""}`}>
    <button class=${`flag mute ${track.muted ? "on" : ""}`} title="Mute" onClick=${() => flag("track.mute", "muted", !track.muted)}>M</button>
    <button class=${`flag solo ${track.solo ? "on" : ""}`} title="Solo" onClick=${() => flag("track.solo", "solo", !track.solo)}>S</button>
    <button class=${`flag arm ${track.armed ? "on" : ""}`} title="Arm for recording" onClick=${() => flag("track.arm", "armed", !track.armed)}><span></span></button>
    <button class=${`flag monitor ${track.monitor ? "on" : ""}`} title="Input monitoring" onClick=${() => flag("track.monitor", "monitor", !track.monitor)}>
      <${Icon} name="headphones" size=${13} /></button>
  </div>`;
}

function TrackHeader({ store, track, selected, onSelect, rowH, small, folders, onDrag }) {
  const [picking, setPicking] = useState(false);
  const indent = track.level * 12;
  const folder = track.folder_depth > 0;
  const fold = ["Folder open: tap to show its tracks small", "Folder tracks small: tap to collapse", "Folder collapsed: tap to open"];
  return html`<div class=${`track-head ${selected ? "selected" : ""} ${small ? "small" : ""}`} style=${{ height: `${rowH}px` }}
      onPointerDown=${(e) => {
        onSelect();
        onDrag(e);
      }}>
    ${folders.map((f) => html`<span class="fold-line" style=${{ left: `${7 + f.level * 12}px`, background: f.color || "var(--text-dim)" }}></span>`)}
    <div class="head-line" style=${{ paddingLeft: `${12 + indent}px` }}>
      <span class="grip" title="Drag to move the track, into a folder over the middle of a track"><${Icon} name="grip" size=${12} /></span>
      ${folder && html`<button class=${`fold-toggle c${track.folder_compact}`} title=${fold[track.folder_compact] || fold[0]}
        onClick=${() => store.fire({ cmd: "track.collapse", track: track.id })}><${Icon} name=${track.folder_compact === 2 ? "next" : "down"} size=${13} /></button>`}
      <button class=${`color-dot ${folder ? "folder" : ""}`} style=${{ background: track.color || "var(--text-dim)" }}
        title="Track colour" onClick=${() => setPicking(true)}></button>
      <${EditableText} cls="track-name" value=${track.name} placeholder=${`Track ${track.index + 1}`}
        onCommit=${(name) => store.fire({ cmd: "track.rename", track: track.id, name })} />
      <span class="db mono">${formatDb(track.volume)}</span>
    </div>
    ${rowH >= 64 && html`<div class="head-line second" style=${{ paddingLeft: `${12 + indent}px` }}>
      <${FlagButtons} store=${store} track=${track} />
      <span class="fx-count mono">${track.fx.length} fx</span>
    </div>`}
    ${picking && html`<${ColorPicker} store=${store} track=${track} onClose=${() => setPicking(false)} />`}
  </div>`;
}

// One cache per connection, kept across re-renders and remounts of the arrange view.
const caches = new WeakMap();

function peakCache(client) {
  if (!caches.has(client)) {
    caches.set(client, new PeakCache((item, width) => client.http("GET", `/api/peaks?item=${item}&width=${width}`)));
  }
  return caches.get(client);
}

function ItemWave({ store, item, width, color }) {
  const canvas = useRef(null);
  const [state, setState] = useState(null);
  const [retry, setRetry] = useState(0);
  const columns = columnsFor(width);
  useEffect(() => {
    if (!item.file) return;
    let live = true;
    let timer = null;
    peakCache(store.client)
      .get(item, columns)
      .then((peaks) => {
        if (!live) return;
        setState(peaks.state);
        if (peaks.state === "ready" && canvas.current) drawPeaks(canvas.current, peaks, color);
        // A long file can take a while to read; keep asking while the item is on screen.
        if (peaks.state === "loading") timer = setTimeout(() => setRetry((r) => r + 1), 2000);
      }, () => {});
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [item.id, item.length, item.volume, item.active_take, item.file, columns, color, retry]);
  if (!item.file) return null;
  if (state === "offline") return html`<span class="item-note">media offline</span>`;
  return html`<canvas class="item-wave" ref=${canvas} width=${columns} height="64"></canvas>`;
}

function Item({ store, item, track, pps, rowH, selected, onSelect, snap, snapOn, rowIndex, rows }) {
  const [preview, setPreview] = useState(null);
  const down = (e) => {
    if (e.button > 0) return;
    e.stopPropagation();
    const touch = e.pointerType !== "mouse";
    onSelect();
    if (touch && !selected) return;
    e.preventDefault();
    let result = null;
    dragPointer(
      e,
      ({ dx, dy }) => {
        if (Math.abs(dx) < 3 && Math.abs(dy) < 3 && !result) return;
        let position = Math.max(0, item.position + dx / pps);
        if (snapOn) position = snapTime(position, snap.bars, snap.time_signature[0]);
        const target = clamp(rowIndex + Math.round(dy / (rowH + 4)), 0, rows.length - 1);
        result = { position, row: target };
        setPreview(result);
      },
      ({ cancelled }) => {
        setPreview(null);
        if (!result || cancelled) return;
        const dest = rows[result.row];
        if (Math.abs(result.position - item.position) < 1e-6 && dest.id === track.id) return;
        store.patchItem({ ...item, position: result.position, track: dest.id }, track.id);
        const cmd = { cmd: "item.move", item: item.id, position: result.position };
        if (dest.id !== track.id) cmd.track = dest.id;
        store.run(cmd).then((info) => store.patchItem(info, dest.id), () => {});
      },
    );
  };
  const left = (preview?.position ?? item.position) * pps;
  const shift = preview ? (preview.row - rowIndex) * (rowH + 4) : 0;
  const width = Math.max(2, item.length * pps);
  const color = track.color || "#a6a28c";
  return html`<div class=${`item ${selected ? "selected" : ""} ${item.muted ? "muted" : ""} ${preview ? "dragging" : ""}`}
      style=${{ left: `${left}px`, width: `${width}px`, transform: shift ? `translateY(${shift}px)` : "", "--item-color": color }}
      title=${`${item.name}\n${formatTime(item.position)} – ${formatTime(item.position + item.length)}`}
      onPointerDown=${down} onClick=${(e) => e.stopPropagation()}>
    <${ItemWave} store=${store} item=${item} width=${width} color=${color} />
    ${width > 24 && html`<span class="item-name">${item.name}</span>`}
  </div>`;
}

function MarkerDialog({ store, marker, onClose }) {
  const [name, setName] = useState(marker.name);
  const [position, setPosition] = useState(marker.position.toFixed(3));
  const [end, setEnd] = useState(marker.end != null ? marker.end.toFixed(3) : "");
  const save = () => {
    const cmd = { cmd: "marker.edit", marker: marker.number, region: marker.region, name, position: parseFloat(position) };
    if (marker.region) cmd.end = parseFloat(end);
    store.fire(cmd);
    onClose();
  };
  return html`<${Modal} title=${`${marker.region ? "Region" : "Marker"} ${marker.number}`} onClose=${onClose}>
    <label class="field"><span>Name</span><input value=${name} onInput=${(e) => setName(e.target.value)} onKeyDown=${(e) => e.key === "Enter" && save()} /></label>
    <div class="field-row">
      <label class="field"><span>Position (s)</span><input class="mono" type="number" step="0.001" min="0" value=${position} onInput=${(e) => setPosition(e.target.value)} /></label>
      ${marker.region && html`<label class="field"><span>End (s)</span><input class="mono" type="number" step="0.001" min="0" value=${end} onInput=${(e) => setEnd(e.target.value)} /></label>`}
    </div>
    <div class="swatches small">
      ${SWATCHES.map((c) => html`<button class=${`swatch ${marker.color === c ? "on" : ""}`} style=${{ background: c }}
        onClick=${() => store.fire({ cmd: "marker.edit", marker: marker.number, region: marker.region, color: c })}></button>`)}
    </div>
    <div class="button-row">
      <button class="btn" onClick=${() => {
        store.fire({ cmd: "marker.goto", marker: marker.number, region: marker.region });
        onClose();
      }}>Go to</button>
      <button class="btn danger" onClick=${() => {
        store.fire({ cmd: "marker.delete", marker: marker.number, region: marker.region });
        onClose();
      }}><${Icon} name="trash" size=${16} />Delete</button>
      <span class="grow"></span>
      <button class="btn primary" onClick=${save}>Save</button>
    </div>
  </${Modal}>`;
}

function MarkerFlag({ store, marker, pps, room, onEdit }) {
  const tap = useDoubleTap(() => onEdit(marker));
  const left = marker.position * pps;
  const click = (e) => {
    e.stopPropagation();
    if (!tap()) store.fire({ cmd: "marker.goto", marker: marker.number, region: marker.region });
  };
  const style = { left: `${left}px`, "--marker-color": marker.color || (marker.region ? "var(--blue)" : "var(--orange)") };
  if (marker.region) {
    style.width = `${Math.max(4, (marker.end - marker.position) * pps)}px`;
    return html`<div class="region" style=${style} onClick=${click} title=${`Region ${marker.number}: ${marker.name}\nTap to go, double-tap to edit`}>
      <span>${marker.name || marker.number}</span></div>`;
  }
  style.maxWidth = `${clamp(room * pps - 2, 14, 220)}px`;
  return html`<div class="marker" style=${style} onClick=${click} title=${`Marker ${marker.number}: ${marker.name}\nTap to go, double-tap to edit`}>
    <span class="marker-num">${marker.number}</span>${marker.name && html`<span class="marker-name">${marker.name}</span>`}</div>`;
}

function Ruler({ store, snap, pps, range, snapOn, width }) {
  const step = barStep(barPx(snap, pps));
  const [i0, i1] = range;
  const labels = [];
  for (let i = Math.max(0, i0 - (i0 % step)); i <= i1 && i < snap.bars.length; i += step) {
    labels.push(html`<div class="ruler-bar" style=${{ left: `${snap.bars[i] * pps}px` }}>${i + 1}</div>`);
  }
  const down = (e) => {
    if (e.button > 0) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const at = (x) => Math.max(0, (x - rect.left) / pps);
    const t0 = at(e.clientX);
    let selecting = false;
    dragPointer(
      e,
      ({ dx, event }) => {
        if (Math.abs(dx) > 6) selecting = true;
        if (!selecting) return;
        let a = t0;
        let b = at(event.clientX);
        if (snapOn) [a, b] = [snapTime(a, snap.bars, snap.time_signature[0]), snapTime(b, snap.bars, snap.time_signature[0])];
        if (a > b) [a, b] = [b, a];
        if (b - a > 0.01) store.fire({ cmd: "transport.loop", start: a, end: b }, { latest: "loop-range" });
      },
      ({ cancelled }) => {
        if (selecting || cancelled) return;
        store.fire({ cmd: "transport.seek", position: snapOn ? snapTime(t0, snap.bars, snap.time_signature[0]) : t0 });
      },
    );
  };
  return html`<div class="ruler" style=${{ width: `${width}px` }} onPointerDown=${down}
      title="Tap to move the cursor, drag to set the loop range">${labels}</div>`;
}

// Seconds to the next plain marker, so a crowded marker lane cuts names instead of overlapping
// them.
function markerRoom(markers, m) {
  const next = markers.find((o) => !o.region && o.position > m.position);
  return next ? next.position - m.position : Infinity;
}

const barPx = (snap, pps) => (snap.bars.length > 1 ? (snap.bars[1] - snap.bars[0]) * pps : 2 * pps);

function Grid({ snap, pps, range, from, height }) {
  const step = barStep(barPx(snap, pps), 14);
  const lines = [];
  const [i0, i1] = range;
  for (let i = Math.max(0, i0 - (i0 % step)); i <= i1 && i < snap.bars.length; i += step) {
    if (snap.bars[i] < from) continue;
    lines.push(html`<div class=${`grid-line ${i % (step * 4) === 0 ? "strong" : ""}`} style=${{ left: `${HEADER_W + snap.bars[i] * pps}px` }}></div>`);
  }
  return html`<div class="grid" style=${{ height: `${height}px` }}>${lines}</div>`;
}

export function Arrange({ store, snap, sel, setSel }) {
  const scroller = useRef(null);
  const [pps, setPps] = useState(() => {
    const saved = parseFloat(localStorage.getItem("scythe.pps"));
    return isFinite(saved) ? saved : 12;
  });
  const [rowSize, setRowSize] = useState(1);
  const [snapOn, setSnapOn] = useState(true);
  const [view, setView] = useState({ left: 0, width: 1200 });
  const [editing, setEditing] = useState(null);
  const [tempoMarker, setTempoMarker] = useState(null);
  const [tempoMap, setTempoMap] = useState(false);
  const anchor = useRef(null);
  const rowH = ROW_HEIGHTS[rowSize];
  const tracks = snap.tracks;
  const end = Math.max(snap.length, ...snap.markers.map((m) => m.end ?? m.position), 60) + 30;
  const contentW = end * pps;
  const shows = rowShows(tracks);
  const parents = folderParents(tracks);
  const layout = [];
  let rowsH = 0;
  tracks.forEach((_, i) => {
    if (shows[i] === "hidden") return;
    const h = shows[i] === "small" ? SMALL_H : rowH;
    layout.push({ i, top: rowsH, h });
    rowsH += h + 4;
  });
  const shownTracks = layout.map(({ i }) => tracks[i]);
  const rowsEl = useRef(null);
  const [drag, setDrag] = useState(null);
  // Where a dragged track lands with the pointer `y` from the top of the rows.
  const dropAt = (y, moving) => {
    const last = layout[layout.length - 1];
    if (!last) return null;
    if (y > last.top + last.h + 2) return { before: tracks.length, level: 0 };
    const row = layout.find((r) => y < r.top + r.h + 2) || last;
    return trackDrop(tracks, moving, row.i, clamp((y - row.top) / row.h, 0, 1));
  };
  const dropTop = (drop) => {
    const row = layout.find((r) => r.i >= drop.before);
    return (row ? row.top : rowsH) - 3;
  };
  // Mouse drags move a track from anywhere on its header; touch drags from the grip, so
  // scrolling still works.
  const startDrag = (i) => (e) => {
    if (e.button > 0 || e.target.closest("button, input, .editable")) return;
    if (e.pointerType !== "mouse" && !e.target.closest(".grip")) return;
    const moving = withChildren(tracks, i);
    let drop = null;
    dragPointer(
      e,
      ({ dy, event }) => {
        if (!drop && Math.abs(dy) < 5) return;
        drop = dropAt(event.clientY - rowsEl.current.getBoundingClientRect().top, moving);
        setDrag({ moving, drop });
      },
      ({ cancelled }) => {
        setDrag(null);
        if (!cancelled && drop) store.fire(moveCommand(tracks, moving, drop));
      },
    );
  };

  useEffect(() => {
    try {
      localStorage.setItem("scythe.pps", String(pps));
    } catch {
      // Not remembered in private browsing.
    }
  }, [pps]);

  const readView = () => {
    const el = scroller.current;
    if (el) setView({ left: el.scrollLeft, width: el.clientWidth });
  };
  useEffect(() => {
    readView();
    addEventListener("resize", readView);
    return () => removeEventListener("resize", readView);
  }, []);
  const frame = useRef(0);
  const onScroll = () => {
    if (frame.current) return;
    frame.current = requestAnimationFrame(() => {
      frame.current = 0;
      readView();
    });
  };

  // Zooming keeps the time under `anchor` (the pointer, or the middle) where it was.
  const zoomTo = (next, clientX) => {
    const el = scroller.current;
    const rect = el.getBoundingClientRect();
    const x = clientX != null ? clientX - rect.left - HEADER_W : (el.clientWidth - HEADER_W) / 2;
    const t = (el.scrollLeft + x) / pps;
    const clamped = clamp(next, MIN_PPS, MAX_PPS);
    anchor.current = { t, x };
    setPps(clamped);
  };
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el || !anchor.current) return;
    el.scrollLeft = Math.max(0, anchor.current.t * pps - anchor.current.x);
    anchor.current = null;
    readView();
  }, [pps]);
  const wheel = (e) => {
    if (!(e.ctrlKey || e.metaKey)) return;
    e.preventDefault();
    zoomTo(pps * Math.exp(-e.deltaY * 0.01), e.clientX);
  };
  useEffect(() => {
    const el = scroller.current;
    el.addEventListener("wheel", wheel, { passive: false });
    // iPad Safari pinches with gesture events rather than ctrl+wheel.
    let base = pps;
    const start = () => (base = pps);
    const change = (e) => {
      e.preventDefault();
      zoomTo(base * e.scale);
    };
    el.addEventListener("gesturestart", start);
    el.addEventListener("gesturechange", change);
    return () => {
      el.removeEventListener("wheel", wheel);
      el.removeEventListener("gesturestart", start);
      el.removeEventListener("gesturechange", change);
    };
  });

  // Keeps the playhead in view while playing, and shows the edit cursor on opening.
  const shownCursor = useRef(false);
  useEffect(() => {
    const follow = (tr) => {
      const el = scroller.current;
      if (!el || !tr) return;
      const x = tr.position * pps;
      const lanesW = el.clientWidth - HEADER_W;
      const outside = x < el.scrollLeft || x > el.scrollLeft + lanesW - 20;
      if ((tr.playing || !shownCursor.current) && outside) el.scrollLeft = Math.max(0, x - lanesW * 0.1);
      shownCursor.current = true;
    };
    follow(store.transport);
    return store.on("transport", follow);
  }, [store, pps]);

  const fit = () => {
    const el = scroller.current;
    const len = Math.max(snap.length, 10);
    zoomTo((el.clientWidth - HEADER_W - 24) / len);
    anchor.current = { t: 0, x: 0 };
  };

  const visible0 = view.left / pps;
  const visible1 = (view.left + view.width - HEADER_W) / pps;
  const range = [Math.max(0, barIndexAt(snap.bars, visible0) - 1), barIndexAt(snap.bars, visible1) + 1];
  const t = store.transport || snap.transport;
  const selectedItem = sel.item != null ? tracks.flatMap((tr) => tr.items).find((it) => it.id === sel.item) : null;
  const laneClick = (track) => (e) => {
    const rect = e.currentTarget.getBoundingClientRect();
    let at = Math.max(0, (e.clientX - rect.left) / pps);
    if (snapOn) at = snapTime(at, snap.bars, snap.time_signature[0]);
    setSel({ track: track.id, item: null });
    store.fire({ cmd: "transport.seek", position: at });
  };
  const addRegion = () => {
    const [start, stop] = t.loop_range || [];
    if (start == null) return store.notify("Set a loop range on the ruler first", "error");
    store.fire({ cmd: "marker.add", position: start, end: stop, name: "" });
  };

  return html`<section class="arrange panel">
    <div class="panel-tools">
      <span class="panel-title">Arrange</span>
      <span class="dim small mono">${tracks.length} tracks · ${formatTime(snap.length)}</span>
      <span class="grow"></span>
      <button class="btn small" onClick=${() => store.fire({ cmd: "track.add" })}><${Icon} name="plus" size=${14} />Track</button>
      <button class="btn small" onClick=${() => store.fire({ cmd: "marker.add" })} title="Add a marker at the edit cursor"><${Icon} name="marker" size=${14} />Marker</button>
      <button class="btn small" onClick=${addRegion} title="Add a region over the loop range">Region</button>
      <button class="btn small" onClick=${() => setTempoMap(true)} title="Tempo and time signature markers">Tempo</button>
      <span class="tool-sep"></span>
      <${IconButton} icon="split" title="Split the selected item at the edit cursor (S)" disabled=${!selectedItem}
        onClick=${() => store.fire({ cmd: "item.split", item: sel.item, at: t.cursor })} />
      <${IconButton} icon="trash" title="Delete the selected item (Delete)" disabled=${!selectedItem} onClick=${() => {
        store.fire({ cmd: "item.delete", item: sel.item });
        setSel({ ...sel, item: null });
      }} />
      <span class="tool-sep"></span>
      <button class=${`btn small toggle ${snapOn ? "on" : ""}`} onClick=${() => setSnapOn(!snapOn)} title="Snap to beats">Snap</button>
      <${IconButton} icon="zoomOut" title="Zoom out" onClick=${() => zoomTo(pps / 1.5)} />
      <${IconButton} icon="zoomIn" title="Zoom in" onClick=${() => zoomTo(pps * 1.5)} />
      <button class="btn small" onClick=${fit} title="Show the whole project">Fit</button>
      <button class="btn small" onClick=${() => setRowSize((rowSize + 1) % ROW_HEIGHTS.length)} title="Track height">
        <${Icon} name="menu" size=${14} /></button>
    </div>
    <div class="arr-scroll" ref=${scroller} onScroll=${onScroll}>
      <div class="arr-canvas" style=${{ width: `${HEADER_W + contentW}px`, height: `${TOP_H + rowsH + 40}px` }}>
        <${Grid} snap=${snap} pps=${pps} range=${range} from=${visible0} height=${TOP_H + rowsH + 40} />
        <${LoopRange} store=${store} pps=${pps} />
        <div class="arr-top">
          <div class="arr-corner"><span class="dim small">Bars</span><span class="dim small">Markers</span>
            <button class="corner-link dim small" onClick=${() => setTempoMap(true)}>Tempo</button></div>
          <div class="arr-top-lanes">
            <${Ruler} store=${store} snap=${snap} pps=${pps} range=${range} snapOn=${snapOn} width=${contentW} />
            <div class="marker-lane" style=${{ width: `${contentW}px` }}>
              ${snap.markers.map((m) => html`<${MarkerFlag} key=${`${m.region}-${m.number}`} store=${store} marker=${m} room=${markerRoom(snap.markers, m)}
                pps=${pps} onEdit=${setEditing} />`)}
            </div>
            <${TempoLane} store=${store} snap=${snap} pps=${pps} width=${contentW} onEdit=${setTempoMarker}
              snapTo=${(t) => (snapOn ? snapTime(t, snap.bars, snap.time_signature[0]) : t)} />
          </div>
        </div>
        <div class="arr-rows" ref=${rowsEl}>
          ${layout.map(
            ({ i, h }, k) => {
              const track = tracks[i];
              const ancestors = [];
              for (let p = parents[i]; p != null; p = parents[p]) ancestors.push({ level: tracks[p].level, color: tracks[p].color });
              return html`<div class=${`arr-row ${drag?.moving.includes(i) ? "dragged" : ""}`} key=${track.id} style=${{ height: `${h}px` }}>
              <${TrackHeader} store=${store} track=${track} rowH=${h} small=${shows[i] === "small"} folders=${ancestors}
                selected=${sel.track === track.id} onDrag=${startDrag(i)}
                onSelect=${() => sel.track !== track.id && setSel({ track: track.id, item: null })} />
              <div class=${`lane ${sel.track === track.id ? "selected" : ""}`} style=${{ width: `${contentW}px` }} onClick=${laneClick(track)}>
                ${track.items
                  .filter((it) => it.position <= visible1 + 5 && it.position + it.length >= visible0 - 5)
                  .map(
                    (it) => html`<${Item} key=${it.id} store=${store} item=${it} track=${track} pps=${pps} rowH=${h}
                      selected=${sel.item === it.id} onSelect=${() => setSel({ track: track.id, item: it.id })}
                      snap=${snap} snapOn=${snapOn} rowIndex=${k} rows=${shownTracks} />`,
                  )}
              </div>
            </div>`;
            },
          )}
          ${drag?.drop && html`<div class="drop-line" style=${{ top: `${dropTop(drag.drop)}px`, left: `${6 + drag.drop.level * 12}px` }}></div>`}
          ${tracks.length === 0 && html`<div class="empty">No tracks yet. <button class="btn small" onClick=${() => store.fire({ cmd: "track.add" })}>Add a track</button></div>`}
        </div>
        <${Playhead} store=${store} pps=${pps} />
      </div>
    </div>
    ${editing && html`<${MarkerDialog} store=${store} marker=${editing} onClose=${() => setEditing(null)} />`}
    ${tempoMarker && html`<${TempoDialog} store=${store} marker=${tempoMarker} onClose=${() => setTempoMarker(null)} />`}
    ${tempoMap && html`<${TempoMapDialog} store=${store} snap=${snap} onClose=${() => setTempoMap(false)} />`}
  </section>`;
}
