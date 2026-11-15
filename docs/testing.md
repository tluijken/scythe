# Testing Scythe

Scythe has three layers of tests. The fast ones run on every change; the integration tests
drive the app the way a user, a tablet or an AI agent does and take a few minutes.

| Command | What runs | Time |
|---|---|---|
| `devbox run test` | unit tests of every crate and the in-process server tests (`cargo test --workspace`) | a few minutes, most of it building |
| `devbox run itest` | the UI tests, the end-to-end process tests and the corpus (below) | about five minutes after a build |
| `scripts/cargo.sh run --release -p scythe-engine --example null_check -- x.RPP …` | the null set: probe projects rendered by Scythe against REAPER 7.73's renders of them (see [verification](verification.md)) | minutes |
| `devbox run -- scripts/reaper-compare.sh` | ScyEQ-Pro in REAPER, with the plugin, against Scythe (below) | a few minutes after a build |

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

## ScyEQ-Pro in REAPER (`scripts/reaper-compare.sh`)

A local check, run by hand on each platform before a release of the ScyEQ-Pro plugin: REAPER is
not on CI. It needs REAPER, and on Linux Xvfb (from `PATH`, `SCYTHE_XVFB` or nixpkgs).

```sh
devbox run -- scripts/reaper-compare.sh [song.RPP]     # Linux
scripts/reaper-compare.sh [song.RPP]                     # Windows, in Git Bash
```

On Windows the default REAPER is `C:\Program Files\REAPER (x64)\reaper.exe`; in the VM, copy the
tree to `C:\src\scyeq-win` and plugin-format to `C:\src\plugin-format`, point
`crates/scyeq-plugin/Cargo.toml` at the latter, copy the song's `.RPP` over and run the script
from Git Bash (over `ssh winvm` too: REAPER renders without a desktop).

1. It builds the plugin with plugin-format's `scripts/bundle.sh` into a temporary folder, and
   `scythe-server` and the `reaper_projects` example of `scyeq-plugin`
   (`crates/scyeq-plugin/tests/reaper/projects.rs`).
2. `reaper_projects write` writes the test projects, each a 48 kHz track of noise whose level
   steps (so dynamic bands move) through one ScyEQ-Pro CLAP slot: the plugin's defaults, each
   band shape, a dynamic, a spectral and a sidechain band (keyed from a second track on channels
   3/4), every shape with all three band kinds in each phase mode (also rendered from frame
   1013, off the processing grid and REAPER's 512-frame blocks), and a band's gain and frequency
   automated, as REAPER writes those envelopes.
3. REAPER renders each project with `-renderproject`, with a configuration of its own
   (`-cfgfile` in the temporary folder, its own `HOME` on Linux) whose CLAP and VST3 paths hold
   only the fresh plugin, on the dummy audio device at 48 kHz and 512 frames (Linux
   `linux_audio_mode=2`, Windows `[audioconfig] mode=5`; without a device REAPER for Windows asks
   for one and renders nothing). Nothing is installed and your REAPER settings are not touched.
4. `scythe-server --null-audio` renders the same projects through the control API
   (`project.open`, `render.settings` with another file name, `render.project`).
5. `reaper_projects compare` compares the renders sample by sample and fails when one differs by
   more than -120 dBFS peak, or when Scythe's render is the dry signal.
6. The song (Song 2 when it is there) is copied without its items and with every other effect
   offline (each missing plugin or file would stop REAPER at a window), opened in REAPER, and a
   ReaScript checks that each ScyEQ-Pro instance in it loaded and is online.

`KEEP=1` keeps the folder with the projects, renders and REAPER's configuration;
`reaper_projects compare <dir> [limit]` compares again. `REAPER` and `PLUGIN_FORMAT` point at
another REAPER or plugin-format checkout.

On NixOS a plugin built in the devbox shell links devbox's newer glibc, which a REAPER from the
system's nixpkgs cannot load (`GLIBC_2.43 not found`, and REAPER lists the plugin as missing).
When REAPER comes from the Nix store the script builds the plugin with the system nixpkgs'
toolchain instead (`PLUGIN_TOOLCHAIN`, its own target folder `target/reaper-compare`), as the
released plugin is built on an older glibc.

Two REAPER behaviours shape the projects. A project rendered at another rate than the audio
device's is resampled twice unless it forces its own rate (`SAMPLERATE 48000 1 0`). And REAPER's
first block of a render races its parameter flush with the render thread: it takes a
parameter's stored value one run and its envelope's the next, so two renders of the same project
differ until the filters settle. The automated projects therefore start their envelopes at the
stored value and are compared from frame 24000 (0.5 s) on; the others from frame 0.

Results on 2026-11-14 (REAPER 7.73 on Linux, 7.85 on Windows): every project within -120 dBFS
on both, all but natural phase (-144.5 dBFS at most, not every run) bit for bit, and Song 2's 30
instances loaded on both.

Scythe plays ScyEQ-Pro's envelopes as REAPER plays them for the slot's format: read at the last
frame of each 512-frame block, in the parameter's own units on a linear range for the CLAP slot
(Scythe's own form, `PARMENV <index>:<id> <min> <max>`), as VST3 normalised values mapped as the
plugin's VST3 wrapper maps them for the VST3 slot (`0 1`). Projects an older Scythe wrote (no
`/format-2` after the version in their header) have their envelopes, modulation baselines and
pooled automation items, which it kept log-normalised on 0..1, moved to the parameter's units
when they open, pooled items to 0..1 linear over the range (`scythe_engine::upgrade`); projects
REAPER wrote are never converted.
