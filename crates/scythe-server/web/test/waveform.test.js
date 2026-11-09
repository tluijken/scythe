import { close, equal, test } from "./harness.js";
import { PeakCache, columnsFor, peaksKey } from "../js/waveform.js";

const item = { id: 7, length: 4, volume: 1, active_take: 0, file: "/a.wav" };

test("waveform resolution steps in powers of two", () => {
  equal(columnsFor(10), 64);
  equal(columnsFor(65), 128);
  equal(columnsFor(1000), 1024);
  equal(columnsFor(100000), 4096);
});

test("an item's peaks are asked for once per shape and resolution", async () => {
  const asked = [];
  const cache = new PeakCache(async (id, width) => (asked.push([id, width]), { state: "ready", min: [-0.5], max: [0.5] }));
  const a = await cache.get(item, 128);
  const b = await cache.get({ ...item }, 128);
  equal(a, b);
  equal(asked, [[7, 128]]);
  await cache.get(item, 256);
  await cache.get({ ...item, length: 2 }, 128);
  await cache.get({ ...item, volume: 0.5 }, 128);
  equal(asked.length, 4);
  equal(peaksKey(item, 64) === peaksKey({ ...item, active_take: 1 }, 64), false);
});

test("a file still loading is asked for again, and failures are not kept", async () => {
  let calls = 0;
  const cache = new PeakCache(async () => (++calls < 3 ? { state: "loading", min: [], max: [] } : { state: "ready", min: [0], max: [0.25] }), {
    setTimer: (f) => f(),
  });
  const p = await cache.get(item, 64);
  equal(calls, 3);
  close(p.max[0], 0.25);
  let fail = true;
  const flaky = new PeakCache(async () => {
    if (fail) throw new Error("offline");
    return { state: "ready", min: [], max: [] };
  });
  let failed = false;
  await flaky.get(item, 64).catch(() => (failed = true));
  equal(failed, true);
  fail = false;
  equal((await flaky.get(item, 64)).state, "ready");
});

test("the cache forgets the least recently used", async () => {
  const cache = new PeakCache(async () => ({ state: "ready", min: [], max: [] }), { max: 2 });
  await cache.get({ ...item, id: 1 }, 64);
  await cache.get({ ...item, id: 2 }, 64);
  await cache.get({ ...item, id: 1 }, 64);
  await cache.get({ ...item, id: 3 }, 64);
  equal(cache.entries.size, 2);
  equal([...cache.entries.keys()].map((k) => k.split("|")[0]), ["1", "3"]);
});

test("a file that never finished loading is asked for again next time", async () => {
  let calls = 0;
  const cache = new PeakCache(async () => (calls++, { state: "loading", min: [], max: [] }), { setTimer: (f) => f() });
  equal((await cache.get(item, 64)).state, "loading");
  const first = calls;
  await cache.get(item, 64);
  equal(calls, first * 2);
});
