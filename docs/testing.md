# Testing Scythe

Scythe has three layers of tests. The fast ones run on every change; the integration tests
drive the app the way a user, a tablet or an AI agent does and take a few minutes.

| Command | What runs | Time |
|---|---|---|
| `devbox run test` | unit tests of every crate and the in-process server tests (`cargo test --workspace`) | a few minutes, most of it building |
| `devbox run itest` | the UI tests, the end-to-end process tests and the corpus (below) | about five minutes after a build |
| `scripts/cargo.sh run --release -p scythe-engine --example null_check -- x.RPP …` | the null set: probe projects rendered by Scythe against REAPER 7.73's renders of them (see [verification](verification.md)) | minutes |

`devbox run itest [filter]` passes the filter to both `cargo test` runs, so
`devbox run itest setlist` runs every integration test with "setlist" in its name.

The integration tests are left out of `cargo test --workspace`: the UI tests live in
`crates/scythe-app/src/itest` behind the `itest` feature, and the process tests in
`crates/scythe-itest`, whose test targets need its `itest` feature. To run one layer by hand:

```sh
devbox run -- scripts/cargo.sh test -p scythe-app --features itest -- itest::flows
devbox run -- scripts/cargo.sh build -p scythe-server -p scythe-app -p scythe-mcp
devbox run -- scripts/cargo.sh test -p scythe-itest --features itest -- --nocapture
```

Everything the tests write goes to `target/itest` (or `SCYTHE_ITEST_DIR`): projects,
preferences, the NAM library, TONE3000 sessions and each process's XDG folders. Nothing reads
or writes your own settings, and no test plays audio: the app and the servers run on the
null audio device. The plugins installed on the machine are not scanned either
(`SCYTHE_SYSTEM_PLUGINS=0`, also set by `devbox run test`).

## UI tests (`crates/scythe-app/src/itest`)

The whole window runs in [egui_kittest](https://docs.rs/egui_kittest)'s harness on a silent
engine, frame by frame exactly as `ui::frame` draws it for the real window. Tests find widgets
the way a screen reader does, by their AccessKit role and name ("Track Drums", "Item drum
loop", "Add plugin", "Play (Space)"), and press, drag and type on them. Scythe's painted
widgets give AccessKit their names for this (see `ui::widgets::button_info`); a widget a test
cannot find usually needs one.

| Module | Covers |
|---|---|
| `flows` | opening a project; selecting, moving (also to another track) and splitting items with undo and redo; adding tracks; adding ScyEQ from the FX browser and dragging a band in its editor; the guitar rig and NAM panel; the monitor mix wizard; the setlist page (build a setlist, next, play, stop); drawing a note in the MIDI editor; folders and dragging a track header; the action palette; the main toolbar and the action list; Escape closing the focused window before it stops the transport; project tabs; saving and opening the result again |
| `agents` | an agent driving the window's session over MCP at `/mcp` (a control server on a free port, pumped between frames): the agent light's states by name ("Agent activity (working)", "(error)", "(idle)"), the Agents window's status and calls with their undo steps, pause refusing changes but not reads, undoing the agent's changes and disconnecting |
| `nam` | the NAM panel's TONE3000 tab against a local stand-in (`tone3000_mock`): asking for a key, signed out, the lists, a tone's models (in view, also in a rig), preview with revert and keep, HTML error pages and null pages |
| `corpus` | every `.RPP` under `SCYTHE_ITEST_CORPUS` (folders separated by `:`, `~/music` by default) and every `<dir>/*/*.RPP` under `SCYTHE_ITEST_PROBES` opens, draws the arrange view and the setlist and monitor pages, plays a moment and stops without a panic, and is not marked changed by being looked at; the corpus is only read |
| `snapshot` | the arrange view and the setlist page compared against `crates/scythe-app/tests/snapshots/*.png` with a tolerance; the CPU load and the toast are masked. Skipped (with a message) without a GPU adapter. `UPDATE_SNAPSHOTS=1` writes new pictures; a failing comparison leaves `*.new.png` and `*.diff.png` beside them |

The fixture is `crates/scythe-app/tests/fixtures/itest/live.RPP`: Drums, Bass and Keys (a
MIDI item), two songs (Opener at 0 s, Ballad at 6 s with a Chorus cue) each ending at a
`!1016` stop marker, and a sine tone written next to it when a test starts.

Pictures are kept for views that draw the same on every run; everything else asserts on the
session's state, which does not depend on fonts or the GPU.

## End-to-end tests (`crates/scythe-itest`)

The built binaries as processes, each with its own folders and a free loopback port (never
8770, where a Scythe of your own may listen), stopped when the test ends:

| Test | Covers |
|---|---|
| `server` | `scythe-server --null-audio --token`: a live session built over HTTP and the WebSocket (tracks, inputs, monitor mixes, a setlist from the markers, access links), saved and opened again; a setlist song playing to its `!1016` stop; a WebSocket client reconnecting; musician and stage links refused what their role may not do |
| `mcp` | `scythe-mcp` over stdio against a running server and with a session of its own (`--headless --null-audio`), and the server's own `/mcp` over HTTP with and without the token; a stdio bridge listed as an agent (transport `stdio`, not a web client) and held by the pause |
| `app` | `scythe-app --null-audio --control` on an X server of its own (Xvfb), driven over its control API while its window is shown and while it is hidden, with the round-trip latency printed and bounded |
| `web` | `/`, `/monitor` and `/setlist` in headless Chromium over the DevTools protocol, one action on each, checked against the server's state; any page exception or `console.error` fails it |

`app` needs `Xvfb` and `web` needs `chromium`: on `PATH`, in `SCYTHE_XVFB` and
`SCYTHE_CHROMIUM`, or (in `devbox run itest`) from nixpkgs through `nix-build`. Without them
these tests print `SKIPPED …` and pass. The binaries come from `target/debug` or
`SCYTHE_BIN_DIR`.

## Writing a test

- A UI test starts from `TestApp::live(name)` (or `TestApp::open(name, path)`), clicks with
  `t.click("label")` or `t.click_role(Role::Button, "label")`, drags with `t.drag(from, to)`
  and checks `t.project()` or `t.app()`. `t.labels()` lists what AccessKit sees when a label
  is not found. Give a new painted widget a name with `resp.widget_info(...)`.
- Waiting on background work (media, TONE3000, playback) goes through `t.until(what, secs,
  condition)`, never a fixed sleep.
- An end-to-end test starts a process with `Scythe::server(...)` or `Scythe::app(...)` from
  `scythe_itest` and speaks to it with `s.ok(command)`, `http` and the WebSocket client `s.ws(token)`.
