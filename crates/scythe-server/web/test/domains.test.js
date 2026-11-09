import { equal, test } from "./harness.js";
import { ApiError } from "../js/api.js";
import { detector, normalizeMixes, normalizeSetlists, normalizeStatus, pickMix, songProgress } from "../js/domains.js";

test("a domain the server does not know is detected once", async () => {
  let calls = 0;
  const old = { send: async () => (calls++, Promise.reject(new ApiError("invalid", 'unknown command domain "mix"'))) };
  const probe = detector(old, { cmd: "mix.list" });
  equal(await probe(), false);
  equal(await probe(), false);
  equal(calls, 1);
  const fresh = detector({ send: async () => [] }, { cmd: "mix.list" });
  equal(await fresh(), true);
  const offline = detector({ send: async () => Promise.reject(new ApiError("offline", "down")) }, { cmd: "mix.list" });
  equal(await offline(), null);
});

test("mixes read with gain or dB levels and own sources", () => {
  const mixes = normalizeMixes({
    mixes: [
      { mix: 40, name: "Bass", more_me_db: 3, own: [12], sources: [{ track: 12, name: "Bass DI", volume: 0.5, pan: 0, muted: false }, { track: 13, name: "Kick", volume_db: -6, muted: true }] },
    ],
  });
  equal(mixes[0].id, 40);
  equal(mixes[0].moreMeDb, 3);
  equal(mixes[0].sources[0].own, true);
  equal(Math.round(mixes[0].sources[0].db * 10) / 10, -6);
  equal(mixes[0].sources[1].db, -6);
  equal(mixes[0].sources[1].muted, true);
});

test("the remembered mix is found by id, else by name", () => {
  const mixes = [{ id: 1, name: "Drums" }, { id: 2, name: "Bass" }];
  equal(pickMix(mixes, { id: 2, name: "Bass" }).id, 2);
  equal(pickMix(mixes, { id: 9, name: "Drums" }).id, 1);
  equal(pickMix(mixes, { name: "Keys" }), null);
  equal(pickMix(mixes, null), null);
});

test("setlists, status and song progress", () => {
  const data = normalizeSetlists({
    setlists: [{ name: "Friday", songs: [{ name: "Intro", start: 10, end: 70, sections: [["Solo", 40]] }, { name: "Outro", start: 80, duration: 100 }] }],
    active: 0,
    auto_advance: true,
  });
  equal(data.lists[0].songs[0].duration, 60);
  equal(data.lists[0].songs[0].sections, [{ name: "Solo", position: 40 }]);
  equal(data.lists[0].songs[1].number, 2);
  equal(data.autoAdvance, true);
  const status = normalizeStatus({ setlist: 0, current: { entry: 1 }, next: null, playing: true });
  equal([status.current, status.next, status.playing], [1, null, true]);
  equal(songProgress(data.lists[0].songs[0], 25), { elapsed: 15, remaining: 45, fraction: 0.25 });
  equal(songProgress(data.lists[0].songs[0], 100).remaining, 0);
});
