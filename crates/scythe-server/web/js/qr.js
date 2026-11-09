// A QR code encoder (ISO/IEC 18004) for the access links: byte mode, error correction level M,
// any version that fits. Written out rather than fetched, so a stage without internet still
// shows codes. After Project Nayuki's QR Code generator (MIT).

const ECC_PER_BLOCK = [-1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28, 28, 28,
  28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28];
const BLOCKS = [-1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31,
  33, 35, 37, 38, 40, 43, 45, 47, 49];
// Level M's two format bits.
const ECL_BITS = 0;

const bit = (x, i) => ((x >>> i) & 1) !== 0;

function rawModules(ver) {
  let n = (16 * ver + 128) * ver + 64;
  if (ver >= 2) {
    const align = Math.floor(ver / 7) + 2;
    n -= (25 * align - 10) * align - 55;
    if (ver >= 7) n -= 36;
  }
  return n;
}

const dataCodewords = (ver) => Math.floor(rawModules(ver) / 8) - ECC_PER_BLOCK[ver] * BLOCKS[ver];

function gfMul(x, y) {
  let z = 0;
  for (let i = 7; i >= 0; i--) {
    z = (z << 1) ^ ((z >>> 7) * 0x11d);
    z ^= ((y >>> i) & 1) * x;
  }
  return z;
}

function rsDivisor(degree) {
  const result = new Array(degree).fill(0);
  result[degree - 1] = 1;
  let root = 1;
  for (let i = 0; i < degree; i++) {
    for (let j = 0; j < degree; j++) {
      result[j] = gfMul(result[j], root);
      if (j + 1 < degree) result[j] ^= result[j + 1];
    }
    root = gfMul(root, 0x02);
  }
  return result;
}

function rsRemainder(data, divisor) {
  const result = divisor.map(() => 0);
  for (const b of data) {
    const factor = b ^ result.shift();
    result.push(0);
    divisor.forEach((d, i) => (result[i] ^= gfMul(d, factor)));
  }
  return result;
}

export function alignmentPositions(ver) {
  if (ver === 1) return [];
  const count = Math.floor(ver / 7) + 2;
  const step = ver === 32 ? 26 : Math.ceil((ver * 4 + 4) / (count * 2 - 2)) * 2;
  const result = [6];
  for (let pos = ver * 4 + 10; result.length < count; pos -= step) result.splice(1, 0, pos);
  return result;
}

// The 15 format bits for level M and `mask`, with their BCH check and XOR mask.
export function formatBits(mask) {
  const data = (ECL_BITS << 3) | mask;
  let rem = data;
  for (let i = 0; i < 10; i++) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
  return ((data << 10) | rem) ^ 0x5412;
}

function versionBits(ver) {
  let rem = ver;
  for (let i = 0; i < 12; i++) rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25);
  return (ver << 12) | rem;
}

// The data codewords for `bytes`: mode, length, data, terminator and padding.
function encodeData(bytes, ver) {
  const bits = [];
  const put = (value, n) => {
    for (let i = n - 1; i >= 0; i--) bits.push((value >>> i) & 1);
  };
  put(0b0100, 4);
  put(bytes.length, ver <= 9 ? 8 : 16);
  bytes.forEach((b) => put(b, 8));
  const capacity = dataCodewords(ver) * 8;
  put(0, Math.min(4, capacity - bits.length));
  put(0, (8 - (bits.length % 8)) % 8);
  for (let pad = 0xec; bits.length < capacity; pad ^= 0xec ^ 0x11) put(pad, 8);
  const out = [];
  for (let i = 0; i < bits.length; i += 8) out.push(bits.slice(i, i + 8).reduce((a, b) => (a << 1) | b, 0));
  return out;
}

function interleave(data, ver) {
  const blocks = BLOCKS[ver];
  const eccLen = ECC_PER_BLOCK[ver];
  const raw = Math.floor(rawModules(ver) / 8);
  const short = blocks - (raw % blocks);
  const shortLen = Math.floor(raw / blocks);
  const divisor = rsDivisor(eccLen);
  const all = [];
  for (let i = 0, k = 0; i < blocks; i++) {
    const dat = data.slice(k, k + shortLen - eccLen + (i < short ? 0 : 1));
    k += dat.length;
    const ecc = rsRemainder(dat, divisor);
    if (i < short) dat.push(0);
    all.push(dat.concat(ecc));
  }
  const result = [];
  for (let i = 0; i < all[0].length; i++) {
    all.forEach((block, j) => {
      if (i !== shortLen - eccLen || j >= short) result.push(block[i]);
    });
  }
  return result;
}

class Grid {
  constructor(ver) {
    this.ver = ver;
    this.size = ver * 4 + 17;
    this.dark = Array.from({ length: this.size }, () => new Array(this.size).fill(false));
    this.fixed = Array.from({ length: this.size }, () => new Array(this.size).fill(false));
  }

  set(x, y, dark) {
    this.dark[y][x] = dark;
    this.fixed[y][x] = true;
  }

  finder(cx, cy) {
    for (let dy = -4; dy <= 4; dy++) {
      for (let dx = -4; dx <= 4; dx++) {
        const d = Math.max(Math.abs(dx), Math.abs(dy));
        const x = cx + dx;
        const y = cy + dy;
        if (x >= 0 && x < this.size && y >= 0 && y < this.size) this.set(x, y, d !== 2 && d !== 4);
      }
    }
  }

  alignment(cx, cy) {
    for (let dy = -2; dy <= 2; dy++) {
      for (let dx = -2; dx <= 2; dx++) this.set(cx + dx, cy + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
    }
  }

  format(mask) {
    const bits = formatBits(mask);
    const s = this.size;
    for (let i = 0; i <= 5; i++) this.set(8, i, bit(bits, i));
    this.set(8, 7, bit(bits, 6));
    this.set(8, 8, bit(bits, 7));
    this.set(7, 8, bit(bits, 8));
    for (let i = 9; i < 15; i++) this.set(14 - i, 8, bit(bits, i));
    for (let i = 0; i < 8; i++) this.set(s - 1 - i, 8, bit(bits, i));
    for (let i = 8; i < 15; i++) this.set(8, s - 15 + i, bit(bits, i));
    this.set(8, s - 8, true);
  }

  functionPatterns() {
    const s = this.size;
    for (let i = 0; i < s; i++) {
      this.set(6, i, i % 2 === 0);
      this.set(i, 6, i % 2 === 0);
    }
    this.finder(3, 3);
    this.finder(s - 4, 3);
    this.finder(3, s - 4);
    const pos = alignmentPositions(this.ver);
    const last = pos.length - 1;
    pos.forEach((x, i) =>
      pos.forEach((y, j) => {
        if (!((i === 0 && j === 0) || (i === 0 && j === last) || (i === last && j === 0))) this.alignment(x, y);
      }),
    );
    this.format(0);
    if (this.ver >= 7) {
      const bits = versionBits(this.ver);
      for (let i = 0; i < 18; i++) {
        const a = s - 11 + (i % 3);
        const b = Math.floor(i / 3);
        this.set(a, b, bit(bits, i));
        this.set(b, a, bit(bits, i));
      }
    }
  }

  codewords(data) {
    const s = this.size;
    let i = 0;
    for (let right = s - 1; right >= 1; right -= 2) {
      if (right === 6) right = 5;
      for (let vert = 0; vert < s; vert++) {
        for (let j = 0; j < 2; j++) {
          const x = right - j;
          const upward = ((right + 1) & 2) === 0;
          const y = upward ? s - 1 - vert : vert;
          if (!this.fixed[y][x] && i < data.length * 8) {
            this.dark[y][x] = bit(data[i >>> 3], 7 - (i & 7));
            i++;
          }
        }
      }
    }
  }

  mask(m) {
    const flips = [
      (x, y) => (x + y) % 2 === 0,
      (x, y) => y % 2 === 0,
      (x) => x % 3 === 0,
      (x, y) => (x + y) % 3 === 0,
      (x, y) => (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0,
      (x, y) => ((x * y) % 2) + ((x * y) % 3) === 0,
      (x, y) => (((x * y) % 2) + ((x * y) % 3)) % 2 === 0,
      (x, y) => (((x + y) % 2) + ((x * y) % 3)) % 2 === 0,
    ][m];
    for (let y = 0; y < this.size; y++) {
      for (let x = 0; x < this.size; x++) if (!this.fixed[y][x] && flips(x, y)) this.dark[y][x] = !this.dark[y][x];
    }
  }

  // The standard's penalty score; the mask with the lowest is the easiest to scan.
  penalty() {
    const s = this.size;
    const d = this.dark;
    let score = 0;
    const line = (get) => {
      let run = 1;
      let pen = 0;
      for (let i = 1; i <= s; i++) {
        if (i < s && get(i) === get(i - 1)) run++;
        else {
          if (run >= 5) pen += run - 2;
          run = 1;
        }
      }
      const finder = [true, false, true, true, true, false, true];
      for (let i = 0; i + 7 <= s; i++) {
        if (!finder.every((f, k) => get(i + k) === f)) continue;
        const lightBefore = [1, 2, 3, 4].every((k) => i - k < 0 || !get(i - k));
        const lightAfter = [0, 1, 2, 3].every((k) => i + 7 + k >= s || !get(i + 7 + k));
        if (lightBefore || lightAfter) pen += 40;
      }
      return pen;
    };
    for (let y = 0; y < s; y++) score += line((x) => d[y][x]);
    for (let x = 0; x < s; x++) score += line((y) => d[y][x]);
    for (let y = 0; y + 1 < s; y++) {
      for (let x = 0; x + 1 < s; x++) {
        const c = d[y][x];
        if (c === d[y][x + 1] && c === d[y + 1][x] && c === d[y + 1][x + 1]) score += 3;
      }
    }
    const darkCount = d.reduce((n, row) => n + row.filter(Boolean).length, 0);
    const total = s * s;
    score += (Math.ceil(Math.abs(darkCount * 20 - total * 10) / total) - 1) * 10;
    return score;
  }
}

// The modules of the QR code for `text` (UTF-8), as rows of booleans (true is dark).
export function qrMatrix(text) {
  const bytes = [...new TextEncoder().encode(text)];
  let ver = 1;
  while (ver <= 40 && 4 + (ver <= 9 ? 8 : 16) + bytes.length * 8 > dataCodewords(ver) * 8) ver++;
  if (ver > 40) throw new Error("too long for a QR code");
  const codewords = interleave(encodeData(bytes, ver), ver);
  let best = null;
  for (let mask = 0; mask < 8; mask++) {
    const grid = new Grid(ver);
    grid.functionPatterns();
    grid.codewords(codewords);
    grid.mask(mask);
    grid.format(mask);
    const score = grid.penalty();
    if (!best || score < best.score) best = { grid, score };
  }
  return best.grid.dark;
}

// An SVG path drawing the dark modules, one unit each, offset by a quiet zone of `border`.
export function qrPath(matrix, border = 4) {
  let d = "";
  matrix.forEach((row, y) => {
    let x = 0;
    while (x < row.length) {
      if (!row[x]) {
        x++;
        continue;
      }
      const start = x;
      while (x < row.length && row[x]) x++;
      d += `M${start + border} ${y + border}h${x - start}v1h${start - x}z`;
    }
  });
  return d;
}
