// Units and formatting, kept in step with scythe-core (units.rs, fader.rs, tempo.rs) so the
// web UI reads the same as the desktop app.

export const MINUS_INF_DB = -150;

export const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

export function dbToGain(db) {
  return db <= MINUS_INF_DB ? 0 : Math.pow(10, db / 20);
}

export function gainToDb(gain) {
  return gain <= 0 ? MINUS_INF_DB : Math.max(MINUS_INF_DB, 20 * Math.log10(gain));
}

export function formatDb(gain) {
  const db = gainToDb(gain);
  if (db <= -100) return "-inf";
  if (Math.abs(db) < 0.05) return "0.0";
  return (db > 0 ? "+" : "") + db.toFixed(1);
}

export function formatDbValue(db) {
  return formatDb(dbToGain(db));
}

export function formatPan(pan) {
  const p = Math.round(pan * 100);
  if (p === 0) return "C";
  return p < 0 ? `L${-p}` : `R${p}`;
}

// REAPER's fader law, measured with reaper.DB2SLIDER: slider 0..1000 with 0 dB at 716.
const SLIDER_TABLE = [
  [-150, 2.513872979395647],
  [-120, 7.9495630678164],
  [-90, 25.138596787751663],
  [-60, 79.48234285438186],
  [-40, 170.9837428707412],
  [-30, 250.09007842261482],
  [-20, 363.27144175838055],
  [-12, 483.9496089500646],
  [-6, 592.8483359945452],
  [-3, 652.8388355955244],
  [0, 716],
  [3, 782.758923678708],
  [6, 852.2869803032572],
  [12, 1000],
  [24, 1332.4540140734498],
];

function partition(pred) {
  let i = 0;
  while (i < SLIDER_TABLE.length && pred(SLIDER_TABLE[i])) i++;
  return Math.min(i, SLIDER_TABLE.length - 1);
}

export function dbToSlider(db) {
  const [loDb, loS] = SLIDER_TABLE[0];
  if (db <= loDb) return loS * Math.pow(10, (db - loDb) / 60);
  const i = partition(([d]) => d <= db);
  const [d0, s0] = SLIDER_TABLE[i - 1];
  const [d1, s1] = SLIDER_TABLE[i];
  return s0 + ((s1 - s0) * (db - d0)) / (d1 - d0);
}

export function sliderToDb(slider) {
  const [loDb, loS] = SLIDER_TABLE[0];
  if (slider <= 0) return -1000;
  if (slider <= loS) return loDb + 60 * Math.log10(slider / loS);
  const i = partition(([, s]) => s <= slider);
  const [d0, s0] = SLIDER_TABLE[i - 1];
  const [d1, s1] = SLIDER_TABLE[i];
  return d0 + ((d1 - d0) * (slider - s0)) / (s1 - s0);
}

// Fader travel 0..1 for a gain, silence to +12 dB, as the desktop mixer draws it.
export function faderPosition(gain) {
  if (gain <= 0) return 0;
  return clamp(dbToSlider(gainToDb(gain)) / 1000, 0, 1);
}

export function faderGain(pos) {
  return pos <= 0 ? 0 : dbToGain(sliderToDb(Math.min(pos, 1) * 1000));
}

// Meter height 0..1: -60 dB..0 dB, linear in dB.
export function meterFraction(peak) {
  if (!(peak > 0)) return 0;
  return clamp((20 * Math.log10(peak) + 60) / 60, 0, 1);
}

const pad = (n, w) => String(n).padStart(w, "0");

// `m:ss.mmm`, or `h:mm:ss.mmm` past an hour.
export function formatTime(seconds) {
  const neg = seconds < 0;
  const total = Math.round(Math.abs(seconds) * 1000);
  const h = Math.floor(total / 3600000);
  const m = Math.floor((total % 3600000) / 60000);
  const s = Math.floor((total % 60000) / 1000);
  const ms = total % 1000;
  const sign = neg ? "-" : "";
  return h > 0 ? `${sign}${h}:${pad(m, 2)}:${pad(s, 2)}.${pad(ms, 3)}` : `${sign}${m}:${pad(s, 2)}.${pad(ms, 3)}`;
}

// `m:ss` for song lengths and countdowns on stage.
export function formatClock(seconds) {
  if (seconds == null || !isFinite(seconds)) return "–:––";
  const neg = seconds < 0;
  const total = Math.floor(Math.abs(seconds));
  return `${neg ? "-" : ""}${Math.floor(total / 60)}:${pad(total % 60, 2)}`;
}

export function formatBpm(bpm) {
  const r = Math.round(bpm * 100) / 100;
  return Number.isInteger(r) ? String(r) : r.toFixed(2);
}

// REAPER's I_RECINPUT numbering, inputs counted from 0 but shown from 1.
export function inputLabel(input) {
  if (input == null || input < 0) return "None";
  if (input >= 4096) {
    const device = Math.floor((input - 4096) / 32);
    const channel = (input - 4096) % 32;
    const dev = device === 63 ? "all devices" : `device ${device + 1}`;
    return `MIDI ${dev}, ${channel === 0 ? "all channels" : `ch ${channel}`}`;
  }
  if (input >= 1024) {
    const n = input - 1024;
    return `In ${n + 1}/${n + 2}`;
  }
  return `In ${input + 1}`;
}

// The audio inputs a picker offers: none, mono 1..n and stereo pairs.
export function inputChoices(count = 16) {
  const out = [{ value: -1, label: "None" }];
  for (let i = 0; i < count; i++) out.push({ value: i, label: inputLabel(i) });
  for (let i = 0; i < count - 1; i++) out.push({ value: 1024 + i, label: inputLabel(1024 + i) });
  out.push({ value: 4096 + 63 * 32, label: inputLabel(4096 + 63 * 32) });
  return out;
}

// Index of the bar holding `t` in `bars` (bar starts, ascending), or -1 before the first.
export function barIndexAt(bars, t) {
  let lo = 0;
  let hi = bars.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (bars[mid] <= t) lo = mid + 1;
    else hi = mid;
  }
  return lo - 1;
}

// A ruler step in bars that keeps labels at least `minPx` apart.
export function barStep(barPx, minPx = 56) {
  for (const step of [1, 2, 4, 8, 16, 32, 64, 128]) {
    if (barPx * step >= minPx) return step;
  }
  return 256;
}

export const SWATCHES = [
  "#66d9ef", "#a6e22e", "#e6db74", "#fd971f", "#f92672", "#ae81ff",
  "#4f9fb0", "#7fa83a", "#b8a955", "#c4762a", "#b8325f", "#8565c4",
];
