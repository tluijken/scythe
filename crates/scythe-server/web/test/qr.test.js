import { assert, equal, test } from "./harness.js";
import { alignmentPositions, formatBits, qrMatrix, qrPath } from "../js/qr.js";

const rows = (m) => m.map((r) => parseInt(r.map((b) => (b ? 1 : 0)).join(""), 2));

test("format bits match the standard's table for level M", () => {
  const table = [0b101010000010010, 0b101000100100101, 0b101111001111100, 0b101101101001011, 0b100010111111001,
    0b100000011001110, 0b100111110010111, 0b100101010100000];
  equal(table.map((_, mask) => formatBits(mask)), table);
});

test("alignment patterns sit where the standard puts them", () => {
  equal(alignmentPositions(1), []);
  equal(alignmentPositions(5), [6, 30]);
  equal(alignmentPositions(7), [6, 22, 38]);
  equal(alignmentPositions(32), [6, 34, 60, 86, 112, 138]);
});

// Checked by decoding a rendering of it with zbarimg.
test("a short text encodes as a known version 1 symbol", () => {
  equal(rows(qrMatrix("Scythe")), [2087551, 1070657, 1523805, 1531485, 1524829, 1068097, 2086271, 7168, 1500235, 1674533,
    914935, 176521, 685720, 4239, 2088152, 1072077, 1525351, 1531018, 1528452, 1068865, 2085636]);
});

test("a link picks the smallest version that holds it, with finders in three corners", () => {
  const link = "http://192.168.1.20:8770/monitor?token=0123456789abcdef0123456789abcdef";
  const m = qrMatrix(link);
  equal(m.length, 37);
  const finder = [127, 65, 93, 93, 93, 65, 127];
  const s = m.length;
  const at = (x0, y0) => [...Array(7)].map((_, y) => parseInt(m[y0 + y].slice(x0, x0 + 7).map((b) => (b ? 1 : 0)).join(""), 2));
  equal(at(0, 0), finder);
  equal(at(s - 7, 0), finder);
  equal(at(0, s - 7), finder);
  assert(m[s - 8][8], "the dark module is dark");
  equal(qrMatrix("x".repeat(300)).length, 69);
});

test("the path draws one run per row of dark modules", () => {
  const path = qrPath([[true, true, false, true]], 1);
  equal(path, "M1 1h2v1h-2zM4 1h1v1h-1z");
});
