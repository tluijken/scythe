import { assert, equal, test } from "./harness.js";
import { ApiError, Client, adoptTokenFromUrl, backoffDelay, wsUrl } from "../js/api.js";

test("reconnects back off from 250 ms to 8 s with jitter", () => {
  equal(backoffDelay(0, () => 0), 250);
  equal(backoffDelay(1, () => 0), 500);
  equal(backoffDelay(10, () => 0), 8000);
  equal(backoffDelay(10, () => 1), 10000);
});

test("the WebSocket address carries the token", () => {
  equal(wsUrl("http://10.0.0.5:8770", "a b"), "ws://10.0.0.5:8770/api/ws?token=a+b");
  equal(wsUrl("https://scythe.local", null), "wss://scythe.local/api/ws");
});

test("a token in the address is stored and removed from it", () => {
  const stored = new Map();
  const storage = { setItem: (k, v) => stored.set(k, v), removeItem: (k) => stored.delete(k), getItem: (k) => stored.get(k) };
  let replaced = null;
  const token = adoptTokenFromUrl({ href: "http://x:8770/monitor?mix=Bass&token=s3cret" }, { replaceState: (_, __, url) => (replaced = url) }, storage);
  equal(token, "s3cret");
  equal(stored.get("scythe.token"), "s3cret");
  equal(replaced, "/monitor?mix=Bass");
});

class FakeSocket {
  constructor(url) {
    this.url = url;
    this.sent = [];
    FakeSocket.last = this;
  }
  send(text) {
    this.sent.push(JSON.parse(text));
  }
  close() {
    this.onclose?.();
  }
}

function connected() {
  const fetchImpl = async () => ({ status: 200, json: async () => ({ ok: true, result: { revision: 1 } }) });
  const client = new Client({ base: "http://h:1", token: "t", WebSocketImpl: FakeSocket, fetchImpl, setTimer: () => 0, clearTimer: () => {} });
  return client;
}

const tick = () => new Promise((r) => setTimeout(r, 0));

test("commands are answered by id and events reach listeners", async () => {
  const client = connected();
  const seen = [];
  client.on("event:transport", (e) => seen.push(e.playing));
  client.on("snapshot", (s) => seen.push(`rev ${s.revision}`));
  client.start();
  await tick();
  const ws = FakeSocket.last;
  equal(ws.url, "ws://h:1/api/ws?token=t");
  ws.onmessage({ data: JSON.stringify({ type: "hello", protocol: 1 }) });
  equal(client.status, "open");
  const answer = client.send({ cmd: "track.volume", track: 1, db: -3 });
  const bad = client.send({ cmd: "track.mute", track: 99 });
  equal(ws.sent.map((m) => m.id), [1, 2]);
  ws.onmessage({ data: JSON.stringify({ type: "reply", id: 1, ok: true, result: { volume_db: -3 } }) });
  ws.onmessage({ data: JSON.stringify({ type: "reply", id: 2, ok: false, error: { kind: "not_found", message: "no track with id 99" } }) });
  ws.onmessage({ data: JSON.stringify({ type: "event", event: "transport", playing: true }) });
  equal((await answer).volume_db, -3);
  let error = null;
  await bad.catch((e) => (error = e));
  assert(error instanceof ApiError);
  equal(error.kind, "not_found");
  equal(seen, ["rev 1", true]);
});

test("a lost connection fails what was waiting and reconnects", async () => {
  const client = connected();
  let retries = 0;
  client.setTimer = () => {
    retries += 1;
    return 0;
  };
  client.start();
  await tick();
  FakeSocket.last.onmessage({ data: JSON.stringify({ type: "hello" }) });
  const waiting = client.send({ cmd: "transport.play" });
  FakeSocket.last.onclose();
  let kind = null;
  await waiting.catch((e) => (kind = e.kind));
  equal(kind, "offline");
  equal(client.status, "reconnecting");
  equal(retries, 1);
});

test("a refused token shows as unauthorized without retrying", async () => {
  const fetchImpl = async () => ({ status: 401, json: async () => ({ ok: false, error: { kind: "unauthorized", message: "no" } }) });
  let retries = 0;
  const client = new Client({ base: "http://h:1", WebSocketImpl: FakeSocket, fetchImpl, setTimer: () => retries++, clearTimer: () => {} });
  client.start();
  await tick();
  await tick();
  equal(client.status, "unauthorized");
  equal(retries, 0);
});

test("continuous controls send only the newest value while one is in flight", async () => {
  const client = new Client({ base: "http://h:1" });
  const sent = [];
  const answers = [];
  client.send = (cmd) => {
    sent.push(cmd.db);
    return new Promise((resolve) => answers.push(() => resolve({ db: cmd.db })));
  };
  const first = client.sendLatest("vol", { db: -1 });
  client.sendLatest("vol", { db: -2 });
  const last = client.sendLatest("vol", { db: -3 });
  equal(sent, [-1]);
  answers.shift()();
  await tick();
  equal(sent, [-1, -3]);
  answers.shift()();
  equal((await last).db, -3);
  equal((await first).db, -3);
  client.sendLatest("vol", { db: -4 });
  equal(sent, [-1, -3, -4]);
});
