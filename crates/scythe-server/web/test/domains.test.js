import { equal, test } from "./harness.js";
import { ApiError } from "../js/api.js";
import { AGENT_ACTIVE_MS, agentLight, agentMarks, agoText, countInLabel, detector, healthLine, normalizeMix, normalizeMixes, normalizeSetlists, normalizeStatus, pickMix, songProgress } from "../js/domains.js";

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

test("mixes read as mix.list answers them", () => {
  const mixes = normalizeMixes([
    { id: 40, musician: "Bass", name: "MON Bass", color: "#a6e22e", volume: 1, volume_db: 0, more_me: 3,
      output: { output: 2, mono: false, label: "Out 3/4" }, limiter: true,
      sources: [
        { track: 12, name: "Bass DI", color: null, volume: 1.41, volume_db: 1.5, pan: 0, muted: false, mode: "post_fx", own: true },
        { track: 13, name: "Kick", color: "#f92672", volume: 0.5, volume_db: -6, pan: -0.5, muted: true, mode: "post_fx", own: false },
      ] },
  ]);
  equal(mixes[0].id, 40);
  equal(mixes[0].name, "Bass");
  equal(mixes[0].moreMeDb, 3);
  equal(mixes[0].output, "Out 3/4");
  equal(mixes[0].clickDb, null);
  equal(Math.round(normalizeMix({ click: 0.5, sources: [] }).clickDb), -6);
  equal(mixes[0].sources.map((x) => [x.id, x.db, x.pan, x.muted, x.own]), [[12, 1.5, 0, false, true], [13, -6, -0.5, true, false]]);
});

test("the remembered mix is found by id, else by name", () => {
  const mixes = [{ id: 1, name: "Drums" }, { id: 2, name: "Bass" }];
  equal(pickMix(mixes, { id: 2, name: "Bass" }).id, 2);
  equal(pickMix(mixes, { id: 9, name: "Drums" }).id, 1);
  equal(pickMix(mixes, { name: "Keys" }), null);
  equal(pickMix(mixes, null), null);
});

test("setlists join their songs, status reads entries", () => {
  const songs = [
    { index: 0, number: 3, guid: "{A}", name: "Intro", start: 10, end: 70, duration: 60, ends_at: "stop_marker", sections: [{ name: "Solo", position: 40 }] },
    { index: 1, number: 5, guid: "{B}", name: "Outro", start: 80, end: 180, duration: 100, ends_at: "project_end", sections: [] },
  ];
  const data = normalizeSetlists(
    { setlists: [{ index: 0, name: "Friday", entries: [{ name: "Outro", guid: "{B}", song: 1, duration: 100 }, { name: "Gone", guid: null, song: null, duration: null }, { name: "Intro", guid: "{A}", song: 0, duration: 60 }], duration: 160 }],
      current: 0, auto_advance: true, marker_actions: true },
    songs,
  );
  const list = data.lists[0];
  equal(list.songs.map((x) => [x.name, x.start, x.missing]), [["Outro", 80, false], ["Gone", null, true], ["Intro", 10, false]]);
  equal(list.songs[2].sections, [{ name: "Solo", position: 40 }]);
  equal([data.active, data.autoAdvance], [0, true]);
  const status = normalizeStatus({ setlist: 0, name: "Friday", playing: true, current: { entry: 2, name: "Intro", start: 10, duration: 60 }, next: null,
    elapsed: 5, remaining: 55, auto_advance: false, marker_actions: true });
  equal([status.current, status.next, status.playing, status.autoAdvance], [2, null, true, false]);
  equal(songProgress(list.songs[2], 25), { elapsed: 15, remaining: 45, fraction: 0.25 });
  equal(songProgress(list.songs[2], 100).remaining, 0);
  equal(songProgress(list.songs[1], 100).elapsed, null);
});

test("a running count-in and the health line read for the stage", () => {
  equal(countInLabel({ count_in: { playback: true, position: -1.5, beat: 2, beats: 4 } }), "2 / 4");
  equal(countInLabel({ count_in: { playback: true, position: null, beat: null, beats: null } }), null);
  equal(countInLabel(null), null);
  const ok = healthLine({ device: "USB", sample_rate: 48000, block_size: 128, cpu: 12.4, cpu_peak: 30, xruns: 0, status: "running",
    reason: null, output_latency_ms: 5.3 });
  equal([ok.text, ok.warn], ["48k · 128 · CPU 12%", false]);
  const quad = healthLine({ device: "QUAD", sample_rate: 44100, block_size: 128, cpu: 3, cpu_peak: 5, xruns: 0, status: "running",
    reason: null, output_latency_ms: 13.7, latency: "~2.9/2.9ms" });
  equal(quad.text, "44.1k · 128 ~2.9/2.9ms · CPU 3%");
  const lost = healthLine({ device: "No audio device", sample_rate: 44100, block_size: 512, cpu: 0, cpu_peak: 0, xruns: 3,
    status: "silent", reason: "USB was disconnected", output_latency_ms: 0 });
  equal([lost.text, lost.warn, lost.title], ["44.1k · 512 · CPU 0% · 3 xruns · no audio device", true, "USB was disconnected"]);
  const status = normalizeStatus({ setlist: 0, playing: false, current: null, next: null, auto_advance: false, count_in: true });
  equal(status.countIn, true);
});

test("the agent light follows the activity and fades", () => {
  const client = { id: "a", name: "Claude Code", running: 0 };
  equal(agentLight(null, {}, 0), "off");
  equal(agentLight({ clients: [] }, {}, 0), "off");
  const idle = { clients: [client], calls: [], busy: false };
  equal(agentLight(idle, {}, 0), "idle");
  const done = { clients: [client], calls: [{ id: 1, ok: true }], busy: true };
  let marks = agentMarks({}, done, 1000);
  equal(agentLight(done, marks, 1500), "busy");
  equal(agentLight(done, marks, 1000 + AGENT_ACTIVE_MS + 1), "idle");
  equal(agentLight({ ...idle, clients: [{ ...client, running: 1 }] }, {}, 0), "busy");
  const failed = { clients: [client], calls: [{ id: 2, ok: false }], busy: true };
  marks = agentMarks(marks, failed, 5000);
  equal(agentLight(failed, marks, 5100), "error");
  marks = agentMarks(marks, failed, 9000);
  equal(agentLight(failed, marks, 9100), "busy", "a failure seen before does not light it again");
  equal(agoText(1000, 2000), "now");
  equal(agoText(0, 75_000), "1 min");
});
