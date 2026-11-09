// Waveforms of arrange items: peaks from `GET /api/peaks` (the engine's peak cache), kept per
// item and resolution so scrolling and re-rendering never ask twice.

const MAX_COLUMNS = 4096;
const MIN_COLUMNS = 64;
const MAX_ENTRIES = 400;
const RETRY_MS = 800;
const MAX_TRIES = 15;

// Columns to ask for an item drawn `px` wide: a power of two, so zooming a little reuses what
// is there and zooming a lot asks again.
export function columnsFor(px) {
  let n = MIN_COLUMNS;
  while (n < px && n < MAX_COLUMNS) n *= 2;
  return n;
}

// What the peaks depend on, as far as a snapshot tells: a split, trim, gain or take change
// gives the item a new key.
export function peaksKey(item, columns) {
  return [item.id, item.length.toFixed(6), item.volume, item.active_take, item.file || "", columns].join("|");
}

export class PeakCache {
  constructor(fetchPeaks, { setTimer = (f, ms) => setTimeout(f, ms), max = MAX_ENTRIES } = {}) {
    this.fetchPeaks = fetchPeaks;
    this.setTimer = setTimer;
    this.max = max;
    this.entries = new Map();
  }

  // Resolves with `{state, min, max}` once known; a file still loading is asked for again a
  // little later, a few times.
  get(item, columns) {
    const key = peaksKey(item, columns);
    const hit = this.entries.get(key);
    if (hit) {
      this.entries.delete(key);
      this.entries.set(key, hit);
      return hit;
    }
    const attempt = (tries) =>
      this.fetchPeaks(item.id, columns).then((p) => {
        if (p.state === "loading" && tries < MAX_TRIES) {
          return new Promise((resolve) => this.setTimer(resolve, RETRY_MS)).then(() => attempt(tries + 1));
        }
        return p;
      });
    const promise = attempt(1).then(
      (p) => {
        if (p.state === "loading") this.entries.delete(key);
        return p;
      },
      (e) => {
        this.entries.delete(key);
        throw e;
      },
    );
    this.entries.set(key, promise);
    while (this.entries.size > this.max) this.entries.delete(this.entries.keys().next().value);
    return promise;
  }
}

// Draws `peaks` into `canvas` (one column per pixel of its width), filled with `color`.
export function drawPeaks(canvas, peaks, color) {
  const ctx = canvas.getContext("2d");
  const { width, height } = canvas;
  ctx.clearRect(0, 0, width, height);
  if (!peaks?.min?.length) return;
  ctx.fillStyle = color;
  const mid = height / 2;
  const n = peaks.min.length;
  for (let x = 0; x < width; x++) {
    const i = Math.min(n - 1, Math.floor((x / width) * n));
    const top = mid - peaks.max[i] * mid;
    const bottom = mid - peaks.min[i] * mid;
    ctx.fillRect(x, top, 1, Math.max(1, bottom - top));
  }
}
