import { equal, test } from "./harness.js";
import { dropIndex, linkUrl, moveEntry, pageForAccess, songSelector } from "../js/domains.js";
import { crumbs, savePath } from "../js/files.js";
import { createCommand, mixCandidates, outputChoices, outputValue } from "../js/mixes.js";
import { tempoEdit, tempoLabel } from "../js/tempo.js";

test("a link's role decides its page", () => {
  equal(pageForAccess("/", { role: "musician", musician: "Bass" }), "/monitor");
  equal(pageForAccess("/monitor", { role: "stage" }), "/setlist");
  equal(pageForAccess("/setlist", { role: "admin" }), "/setlist");
  equal(pageForAccess("/", null), "/");
});

test("links point phones at the server's network address when this page is local", () => {
  const link = { path: "/monitor?token=abc" };
  equal(linkUrl(link, ["http://192.168.1.20:8770"], "http://127.0.0.1:8770"), "http://192.168.1.20:8770/monitor?token=abc");
  equal(linkUrl(link, ["http://192.168.1.20:8770"], "http://localhost:8770"), "http://192.168.1.20:8770/monitor?token=abc");
  equal(linkUrl(link, ["http://192.168.1.20:8770"], "http://studio.lan:8770"), "http://studio.lan:8770/monitor?token=abc");
  equal(linkUrl(link, [], "http://127.0.0.1:8770"), "http://127.0.0.1:8770/monitor?token=abc");
});

test("setlist entries reorder and go back as song selectors", () => {
  equal(moveEntry(["a", "b", "c", "d"], 0, 2), ["b", "c", "a", "d"]);
  equal(moveEntry(["a", "b", "c", "d"], 3, 0), ["d", "a", "b", "c"]);
  equal(dropIndex(1, 90, 44, 5), 3);
  equal(dropIndex(1, -200, 44, 5), 0);
  equal(dropIndex(1, 1000, 44, 5), 4);
  equal(songSelector({ guid: "{AB}", name: "Opener" }), "{AB}");
  equal(songSelector({ guid: null, name: "Opener" }), "Opener");
});

test("the mix wizard's choices become mix.create", () => {
  const snap = {
    tracks: [{ id: 1, folder_depth: 1 }, { id: 2, folder_depth: 0 }, { id: 3, folder_depth: -1 }, { id: 9, folder_depth: 0 }],
    mixes: [{ id: 9, output: { output: 2, mono: false } }],
  };
  equal(mixCandidates(snap).map((t) => t.id), [2, 3]);
  const choices = outputChoices([2]);
  equal(choices[0].label, "Out 1/2");
  equal(choices[1].taken, true);
  equal(choices.find((c) => c.value === "m4").label, "Out 5 (mono)");
  equal(outputValue({ output: 4, mono: true }), "m4");
  equal(outputValue(null), "");
  const form = { musician: " Bass ", sources: [2, 3], own: [3, 7], output: { output: 4, mono: false }, limiter: true, levelsFromTracks: true, pansFromTracks: false };
  equal(createCommand(form), { cmd: "mix.create", musician: "Bass", sources: [2, 3], own: [3], output: 4, mono: false, limiter: true, levels_from_tracks: true });
  equal(createCommand({ ...form, output: null, limiter: false, levelsFromTracks: false }), { cmd: "mix.create", musician: "Bass", sources: [2, 3], own: [3], limiter: false });
});

test("the file browser builds save paths and breadcrumbs", () => {
  equal(savePath("/home/band/live/", "Saturday"), "/home/band/live/Saturday.RPP");
  equal(savePath("/home/band/live", "set.rpp"), "/home/band/live/set.rpp");
  equal(savePath("/x", "a/b"), "/x/a-b.RPP");
  const roots = [{ name: "live", path: "/home/band/live" }];
  equal(crumbs(roots, "/home/band/live/2026/sat").map((c) => c.path), ["/home/band/live", "/home/band/live/2026", "/home/band/live/2026/sat"]);
  equal(crumbs(roots, "/home/band/live").map((c) => c.name), ["live"]);
  equal(crumbs(roots, null), []);
});

test("a tempo marker editor sends only what changed", () => {
  const marker = { index: 2, time: 8, bpm: 120, linear: false, time_signature: null, bar_beat: "5.1.00" };
  equal(tempoEdit(marker, { time: 8, bpm: 120, linear: false, timeSignature: null }), { cmd: "tempo.edit", index: 2 });
  equal(tempoEdit(marker, { time: 10, bpm: 96, linear: true, timeSignature: [7, 8] }),
    { cmd: "tempo.edit", index: 2, time: 10, bpm: 96, linear: true, time_signature: [7, 8] });
  equal(tempoEdit({ ...marker, time_signature: [7, 8] }, { timeSignature: null }), { cmd: "tempo.edit", index: 2, time_signature: null });
  equal(tempoLabel({ bpm: 96, time_signature: [7, 8] }), "96 7/8");
});
