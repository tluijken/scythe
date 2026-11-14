<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo/wordmark-dark.png">
    <img src="docs/logo/wordmark-light.png" alt="Scythe" height="72">
  </picture>
</h1>

A from-scratch digital audio workstation in Rust, strongly inspired by the fantastic
[REAPER](https://www.reaper.fm) and compatible with its projects, with a modern UI of its own.

- **REAPER-compatible.** Scythe opens and saves REAPER 7.73 `.RPP` projects losslessly: on 33
  real projects (~237k lines) a round trip changes only the header line. Projects play
  sample-identically to REAPER wherever that was measured, from pan laws and fades to REAPER's
  ReaEQ and ReaComp, JSFX and resampling ([how](#how-scythe-is-verified)).
- **Built for the stage.** In-ear monitor mixes with "more me", setlists with count-in and stop
  markers, footswitches and control surfaces, and a headless server that runs as a service.
- **Drive it with an AI agent.** Claude Code and other MCP clients can build a live session from a
  description of the band, mix against a reference track with measured loudness and EQ, and set
  up each musician's in-ear mix, every change an undo step. A light in the top bar shows when an
  agent is at work; a click shows what it says it is doing and every call it made, with undo,
  pause and disconnect at hand ([how](#remote)).
- **Remote and scriptable.** A web UI for tablets and phones, a JSON/WebSocket control API, an
  MCP server for AI agents (with mix analysis) and ReaScript in Lua, EEL2 and Python.

![Scythe's main window: arrange view, mixer and inspector](docs/screenshots/main-window.png)

## Quick start

Scythe builds with [devbox](https://www.jetify.com/devbox), which brings Rust and the audio
libraries:

```sh
devbox run app                     # opens the generated demo song
devbox run app -- path/to/song.RPP # opens a REAPER project
devbox run app -- --list-devices   # the audio devices, with their ids
devbox run test
```

The audio interface is chosen in the audio device window (Ctrl+P, or click the
`48k · 512 · CPU` pill in the top bar): JACK, ALSA or Dummy, the devices, how many inputs and
outputs to open, rate and block size; on ALSA it lists one entry per sound card, as REAPER does
(`hw:QUADCAPTURE ; USB-Audio - QUAD-CAPTURE`), and the pill shows REAPER's latency figure. It
switches without closing the project and opens the
same device next time; `--output-device`, `--sample-rate` and the other device flags override it
for one run.

Without a screen, for example as an in-ear-monitor mixer in the rack:

```sh
devbox run -- scripts/cargo.sh run -p scythe-server -- song.RPP --output-device Scarlett
devbox run app -- --headless song.RPP          # the same, from the app binary
```

The web UI and the [control API](docs/control-api.md) listen on `127.0.0.1:8770`; to reach them
from phones and tablets on the band's network, listen on the LAN with an admin token and open
`http://<machine>:8770/` in a browser:

```sh
scythe-server --control 0.0.0.0:8770 --token admin-secret --folder ~/live set.RPP
devbox run app -- --control 0.0.0.0:8770 --control-token secret song.RPP  # window + remote
```

For AI agents, Scythe is an [MCP](docs/mcp.md) server:

```sh
devbox run -- scripts/cargo.sh build --release -p scythe-mcp
claude mcp add scythe -- $PWD/target/release/scythe-mcp --url http://127.0.0.1:8770
claude mcp add --transport http scythe http://127.0.0.1:8770/mcp   # or without the binary
```

Every flag is described under [Command line](docs/features.md#command-line) and
[Remote control](docs/features.md#remote-control); [docs/live.md](docs/live.md) walks through a
whole stage setup.

### Windows

On Windows Scythe builds with rustup's MSVC toolchain and the Visual Studio Build Tools; ASIO
needs LLVM as well (see [Licensing notes](#licensing-notes)). It plays through WASAPI, or ASIO
with `--audio-host ASIO`. The installer is an MSI made with
[cargo-wix](https://github.com/volks73/cargo-wix) and WiX 3:

```powershell
cargo build --release -p scythe-app -p scythe-server -p scythe-mcp --features scythe-app/asio
cargo wix -p scythe-app --no-build --nocapture   # target\wix\scythe-app-<version>-x86_64.msi
```

It installs Scythe, `scythe-server` and `scythe-mcp` under Program Files with a Start menu entry;
adding them to `PATH` and opening `.RPP` files with Scythe are features to pick in the setup (the
file association is off by default). The installer is not signed, so SmartScreen warns before it
runs.

### macOS

On macOS Scythe builds with rustup's stable toolchain and the Xcode Command Line Tools, and plays
through Core Audio. It finds CLAP, VST 3, VST 2 and LV2 plugins in `/Library/Audio/Plug-Ins` and
`~/Library/Audio/Plug-Ins`, and keeps its settings in `~/Library/Application Support/scythe`.
Plugin editors do not open yet, and there is no app bundle or installer.

## Features

A summary; [docs/features.md](docs/features.md) has the full description of each area.

### ScyEQ-Pro

Scythe's own equalizer, which REAPER does not have: 24 bands of ten shapes (bell, shelves,
cuts from 6 to 96 dB/oct and brickwall, notch, band pass, tilt shelf, flat tilt, all pass)
that beyond ScyEQ can be

- **dynamic**, moving towards a range while their region (or a sidechain on channels 3/4)
  crosses a threshold, or **spectral**, taming only the bins that poke out inside their region;
- placed on **stereo, left, right, mid or side** each;
- run at **zero latency** (matched filters that keep the analog shape up to Nyquist), in
  **natural phase** or in **linear phase**, with the latency compensated;
- matched to any track, the master, another ScyEQ-Pro or a reference file with **EQ Match**, edited
  across the project in the **instance list**, and checked for **collisions** with any track's spectrum.

The editor draws every band and the summed curve, with each dynamic band moving live, over a
high-resolution analyzer (pre, post and sidechain; resolution, speed, tilt, range, peak hold,
freeze) with a piano strip, note readout and spectrum grab: drag a peak to put a band on it.
Projects keep it as a CLAP slot that REAPER keeps byte for byte, and agents set its bands with
`eq.band` over the control API and MCP. [More](docs/features.md#effects-and-plugins).

![ScyEQ-Pro on a vocal: bands, a dynamic band moving, the analyzer and the piano strip](docs/screenshots/scyeq-pro.png)

#### ScyEQ-Pro plugin

ScyEQ-Pro also runs outside Scythe, as a CLAP and VST 3 plugin for Linux, Windows and macOS
(beta), with the same sound, editor and state: a project made in REAPER with the CLAP plugin
opens in Scythe with the built-in ScyEQ-Pro, and back. Each CI run builds it as
`ScyEQ-Pro-<version>-<os>.zip`; [crates/scyeq-plugin/README.md](crates/scyeq-plugin/README.md)
says where to install it.

### Arrange and editing

- Move, trim, split, fade, crossfade, glue, copy and paste items; razor edits, ripple editing
  (per track or all tracks), the snap grid with swing, locking and item groups.
- Takes, fixed lanes and comping by swiping, as in REAPER 7.
- Folders, multi-track selection and drag-to-reorder with REAPER's drop rules.
- The tempo map with ramps and time signatures; timebases, stretch markers, play rate, pitch
  shifting with SoundTouch and Rubber Band, dynamic split and tab-to-transient.

### Mixing and routing

- REAPER's fader law, pan laws and pan modes (balance, stereo pan with width, dual pan),
  polarity, solo in place, solo defeat and master loudness (LUFS).
- Sends post-fader, pre-FX or pre-fader, hardware outputs, multichannel tracks up to 32
  channels, sidechains and effect pin maps.
- Track groups with lead and follow per parameter, and VCA groups.
- REAPER's audio device page (JACK, ALSA, Dummy) with every input and output of a large
  interface in the track input, hardware output, monitor mix and click menus, named as REAPER
  names them or by your own aliases, kept in REAPER's reaper.ini keys.

### Effects and plugins

- **REAPER-compatible effects**: ScyEQ, ScyComp, ScyXcomp, ScyGate, ScyLimit, ScyDelay,
  ScyVerb, ScyVerbate, ScyFir, ScySynth, ScySampler, ScyInsert and ScyControlMIDI read and write
  the settings of REAPER's stock effects (ReaEQ, ReaComp and the rest; the pairs are listed in
  [docs/features.md](docs/features.md)), measured against REAPER 7.73 renders, so REAPER opens
  them unchanged. Each has a graphical editor drawn from the emulation's own math.
- **Plugin hosting**: CLAP, LV2, VST 2 and VST 3 (instruments too), with their own editors,
  presets in REAPER's preset files and latency compensation across tracks, sends and folders.
  Plugins are scanned in the background with a cache, CLAP and VST 3 in a child process with a
  time limit, so a plugin that hangs or crashes while scanned is only marked as failed.
- **JSFX** from their own source through an EEL2 interpreter that follows REAPER's compiled
  arithmetic: all 219 of REAPER's stock effects, with their `@gfx` views.
- **FX chains** with wet/dry, parallel effects, FX containers with pin routing, take FX, input
  FX, the monitoring chain, freeze, and `.RfxChain`/`.RTrackTemplate` files.
- **Neural Amp Modeler** models run natively, with a model browser, A/B snapshots and click-free
  switching; **guitar rigs** put gate, amp, cab, EQ and level in one REAPER FX container with a
  rig window, snapshots and presets; [TONE3000](docs/tone3000.md) tones can be browsed,
  previewed in the slot and kept.
- Parameter modulation (LFO, audio control signal, parameter links, MIDI), MIDI and OSC learn.

| ScyEQ | ScyComp |
|---|---|
| ![ScyEQ's editor: band handles over the live spectrum](docs/screenshots/scyeq-editor.png) | ![ScyComp's editor: transfer curve, gain reduction and history](docs/screenshots/scycomp-editor.png) |

| Guitar rig | NAM and TONE3000 browser |
|---|---|
| ![The rig window: gate, amp, cab, EQ and level from left to right](docs/screenshots/guitar-rig.png) | ![The rig's model browser on TONE3000 (sample data)](docs/screenshots/nam-tone3000.png) |

![An FX chain with a container in the inspector](docs/screenshots/fx-container.png)

### MIDI

- A docked MIDI editor with piano roll, named notes (drum maps) and event list, controller lanes
  with REAPER 7's CC shapes, quantize with swing, humanize, transpose by scale, step input and
  groove templates; inline editors in the arrange view.
- MIDI files, MIDI recording and overdub, MIDI routing with buses, hardware MIDI outputs, MIDI
  clock, MTC and LTC (generating and chasing), and a virtual MIDI keyboard.

![The MIDI editor with velocity and CC lanes](docs/screenshots/midi-editor.png)

### Recording

- Recording of armed tracks with REAPER's record modes (input, output, MIDI overdub and
  replace), input monitoring through the FX chain (play a guitar through a NAM amp live),
  punch-in, auto-punch, loop recording into takes or lanes.
- Metronome, count-in and pre-roll as REAPER sets them, the click routed to any output or into
  the monitor mixes.

### Automation

- Track, send and FX parameter envelopes with every point shape, in envelope lanes on the
  arrange time axis.
- Automation items (pooled, looped, LFO shapes, baseline and amplitude) played as REAPER plays
  them; automation modes (touch, latch, write, latch preview) and the global override.

![Envelope lanes with an automation item](docs/screenshots/envelopes.png)

### Live and stage

- **Monitor mixes** for in-ear monitoring: a card per musician with "more me", a safety limiter
  and its own output. A mix is an ordinary REAPER track, so REAPER plays it the same.
- **Setlists** made from the project's markers, with stop markers, cues, auto-advance and a
  count-in per song, stored where REAPER keeps them.
- **Marker actions** (`!1016` stops on the marker's frame), footswitches, MIDI pedals, OSC with
  REAPER's `.ReaperOSC` files, Mackie Control, HUI and the PreSonus FaderPorts.
- A **headless server** that installs itself as a systemd service, runs with real-time priority
  and survives an unplugged audio device. See [docs/live.md](docs/live.md).

| Monitor mixes | Setlist |
|---|---|
| ![Monitor mix cards with more me, limiter and output](docs/screenshots/monitor-mixes.png) | ![The setlist's stage view](docs/screenshots/setlist.png) |

### Remote

- **Web UI** served by Scythe itself, no internet needed: the session editor (`/`) for a tablet
  or laptop, "more me" on a musician's phone (`/monitor`) and the drummer's setlist (`/setlist`).
  Access links with QR codes give each phone exactly the role it needs.
- **Control API**: JSON over HTTP and a WebSocket with transport, meter and project events;
  see [docs/control-api.md](docs/control-api.md).
- **MCP** for Claude Code, Claude Desktop and other agents: read and edit a running session,
  import media, find songs in backing tracks, and analyse a mix (loudness, spectrum, width,
  low end) against a reference with proposed EQ; see [docs/mcp.md](docs/mcp.md).
- **Agent activity** in plain sight: a light in the top bar (desktop and web) shows when an agent is
  connected and pulses while it works; its window lists what the agent says it is doing and each
  tool call in words with the undo steps it made, and lets you undo the agent's last change, pause
  agents (they can still read) or disconnect them.

![The Agents window over the Nightfall demo: Claude Code balancing the mix, its status, and each call with its undo step](docs/screenshots/agent-activity.png)

*Claude Code mixing the demo song over MCP, captured live: its status, each call in words with
the undo step it made, and the light by the title pulsing while it measures the mix.*

![The web editor: arrange view, mixer and inspector in a browser](docs/screenshots/web-editor.png)

| Monitor (phone) | Setlist (tablet) |
|---|---|
| <img src="docs/screenshots/web-monitor.png" alt="The monitor page on a phone: more me, a fader per source" width="260"> | <img src="docs/screenshots/web-setlist.png" alt="The setlist page on a tablet: now playing, next up, the songs" width="560"> |

### Scripting

- ReaScript in Lua 5.4, EEL2 and Python: 310 `reaper.*` functions (tracks, items, takes, MIDI,
  envelopes, automation items, markers, tempo, effects, state chunks, actions), `defer` and
  `gfx` windows. A script runs as one undo step.

### Rendering and formats

- Playback of WAV, FLAC, MP3 (with REAPER's MP3 framing), Ogg, Opus, AAC, WavPack, MIDI, video
  and subprojects; every REAPER resample mode including r8brain.
- Renders with the project's own settings (bounds, stems, regions and the region matrix,
  wildcards, the render queue, normalisation, dither) to WAV, Wave64, RF64, AIFF, CAF, MP3,
  FLAC, Ogg Vorbis, Opus, WavPack, GIF, LICEcap, FFmpeg video, CD images and DDP, with metadata.

### Project and media

- Project tabs, subprojects, auto-save and backups.
- A media explorer reading REAPER's media databases, with tags, search, favourites and a
  tempo-matched preview; the project bay and "clean project directory".

![The media explorer](docs/screenshots/media-explorer.png)

### Actions, windows and themes

- One action registry with REAPER's command ids, the action palette (Ctrl+K), keyboard
  shortcuts, custom actions, toolbars and mouse modifiers, kept in REAPER's own files
  (`reaper-kb.ini`, `reaper-menu.ini`, `reaper-mouse.ini`) so a REAPER keymap works in both.
- Window sets, remembered window positions, and themes (REAPER `.ReaperTheme` colours
  imported).
- A **performance meter** like REAPER's: real-time CPU, the longest block, xruns, and the CPU
  each track and effect takes.
- **Anticipative FX** as REAPER's: tracks without live input render up to 200 ms ahead on worker
  threads while armed tracks and what they feed stay at the device block; ALSA periods 2-8.

## How Scythe is verified

Scythe is checked against REAPER 7.73 itself, not against a description of it.
`crates/scythe-engine/examples/reaper_ref.rs`, `reaeq_ref.rs` and `reaverb_ref.rs` write small
projects, let REAPER render them headless and compare with Scythe's output sample by sample;
`examples/null_check.rs` re-renders any project REAPER rendered with `-renderproject` and
compares the two files. Editing, MIDI editor actions, ReaScript calls, recordings and control
surfaces are compared by running the same steps in both (Lua probes in REAPER, the control API
in Scythe). Most areas null exactly or within a stated tolerance; stretched and pitch-shifted
takes cannot (REAPER's élastique is proprietary). The full table, area by area, is in
[docs/verification.md](docs/verification.md).

## Known gaps

- Windows are drawn inside the main window, so none can move to a second screen.
- About 150 of REAPER's ~4300 main-section actions have a counterpart; there is no extension SDK,
  so SWS and other extensions do not load, and ReaScript covers part of the API.
- élastique and REAPER's own stretch modes use Scythe's phase vocoder, so stretched takes sound
  close but do not null.
- Some stock plugins (ReaPitch, ReaTune, ReaVocode, ReaSurround …) are not emulated and pass
  audio through unless REAPER's own Linux binaries are on the VST path.
- Video processors other than three stock presets are left out of renders.

Every known difference, area by area, is in [docs/limitations.md](docs/limitations.md).

## Documentation

| Document | About |
|---|---|
| [docs/features.md](docs/features.md) | every feature in detail |
| [docs/verification.md](docs/verification.md) | what was compared with REAPER and how close it is |
| [docs/limitations.md](docs/limitations.md) | what is not there yet or differs from REAPER |
| [docs/testing.md](docs/testing.md) | the unit, UI and end-to-end tests: `devbox run test` and `devbox run itest` |
| [docs/live.md](docs/live.md) | Scythe on stage: service, real-time audio, click, footswitches |
| [docs/control-api.md](docs/control-api.md) | the control API's endpoints, commands and events |
| [docs/mcp.md](docs/mcp.md) | the MCP server for AI agents |
| [docs/tone3000.md](docs/tone3000.md) | TONE3000 sign-in, browsing and the model library |

## Layout

| Crate | Contents |
|---|---|
| `scythe-core` | Project model (tracks, folders, items, takes, sends, markers, tempo map, envelopes, fades, MIDI sources), lossless `.RPP` reader/writer, pan and fader laws, undo history |
| `scythe-engine` | cpal output with a null-device fallback, lock-free transport, immutable render graph swapped via `arc-swap`, media decoding (symphonia, with REAPER's MP3 framing) and REAPER's resample modes, peaks, meters, offline WAV bounce, envelopes, metronome, preview synth, recording, FX chains (ScyEQ/ScyVerb-compatible effects, gate, compressor, convolution), CLAP, LV2, VST 2 and VST 3 hosting with editors and latency compensation, time-stretch |
| `clap-test-gain`, `lv2-test-plugins`, `vst2-test-plugin`, `vst3-test-plugin` | Minimal plugins the host tests load (one per format) |
| `scythe-session` | The live session without UI: project and undo, engine sync, media loading, recording and monitoring, metronome, automation writing, meters, save/open/auto-save; the control API (`api::Command`, events) and the pump that runs it on the session's thread |
| `scythe-server` | Control API over HTTP and WebSocket (axum) and MCP at `/mcp`, the web UI (`web/`: plain ES modules with a vendored Preact + htm), the headless runner and its config, the `scythe-server` binary |
| `scythe-surface` | Control surfaces on top of the control API: OSC with REAPER's `.ReaperOSC` pattern files, Mackie Control and extenders, HUI, FaderPort and FaderPort v2; run by the app and the server from the `csurf_<i>` preferences |
| `scythe-mcp` | MCP server for AI agents: tools generated from the control API's command types, the compact project overview, resources and prompts, tool selection, subscriptions and notifications, waiting for measuring jobs; the `scythe-mcp` stdio binary |
| `scythe-app` | egui UI around a `Session`: arrange view, mixer dock, inspector, action palette (Ctrl+K), Actions window, toolbars, plugin editor windows, ReaScript, demo generator |

`cargo run -p scythe-core --example rpp_roundtrip -- *.RPP` loads projects, writes them back
in memory and lists every line whose content changed. On 33 real REAPER projects
(~237k lines) only the header line differs.

## Clean-room approach

Scythe contains no code, binaries or data files from REAPER. Compatibility is checked from the
outside: projects go through REAPER's `.RPP` format and back, and renders are compared with
REAPER's ([docs/verification.md](docs/verification.md)). Reading and writing `.RPP` projects is an
interoperability goal.

## Licensing notes

- SoundTouch (LGPL-2.1) is loaded at run time from the system's `libSoundTouchDll`
  (devbox installs it), never vendored or linked statically; without it SoundTouch takes use the
  phase vocoder.
- Rubber Band is GPL-2.0-or-later and only used with the `rubberband` cargo
  feature, off by default (`cargo build -p scythe-app --features rubberband`), also loaded at run
  time (`librubberband`); without it Rubber Band takes use the phase vocoder.
- ASIO on Windows comes with the `asio` cargo feature, off by default
  (`cargo build -p scythe-app --features asio`, LLVM installed for libclang). It builds against
  Steinberg's ASIO SDK, which `asio-sys` downloads at build time; whoever distributes a build
  with it agrees to Steinberg's ASIO license. Without it Windows plays through WASAPI.
- r8brain free (`RENDER_RESAMPLE 10`) is a Rust implementation of r8brain-free-src 6.x (MIT,
  Aleksey Vaneev); marker actions follow the SWS extension's rules, read from its source (MIT).

## License

Scythe is licensed under either of the [MIT license](LICENSE-MIT) or the
[Apache License 2.0](LICENSE-APACHE), at your option. Third-party code, fonts and libraries it
contains or loads are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

REAPER and Cockos are trademarks of Cockos Incorporated; VST and ASIO are trademarks of
Steinberg Media Technologies GmbH. Scythe is not affiliated with or endorsed by either.
