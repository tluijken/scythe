// A test runner small enough to need nothing: the same files run in any browser
// (test/index.html, also served by Scythe at /test/) and under Node (`node test/run.mjs`).

const tests = [];

export function test(name, fn) {
  tests.push({ name, fn });
}

export function assert(cond, message = "assertion failed") {
  if (!cond) throw new Error(message);
}

export function equal(actual, expected, message = "") {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${message ? message + ": " : ""}expected ${e}, got ${a}`);
}

export function close(actual, expected, eps = 1e-6, message = "") {
  if (!(Math.abs(actual - expected) <= eps)) throw new Error(`${message ? message + ": " : ""}expected ${expected} ± ${eps}, got ${actual}`);
}

export async function run(report) {
  const results = [];
  for (const t of tests) {
    try {
      await t.fn();
      results.push({ name: t.name, ok: true });
    } catch (e) {
      results.push({ name: t.name, ok: false, error: e?.message || String(e) });
    }
    report?.(results[results.length - 1]);
  }
  return results;
}
