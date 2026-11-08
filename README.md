# Scythe

A from-scratch digital audio workstation in Rust, modelled on REAPER's feature set and file
format, with a modern UI based on the "REAPER redesign" mockup.

## Clean-room approach

Scythe contains no code, binaries or data files from REAPER. Compatibility is checked from the
outside: projects go through REAPER's `.RPP` format and back, and renders are compared with
REAPER's ([docs/verification.md](docs/verification.md)). Reading and writing `.RPP` projects is an
interoperability goal.

## Running

```sh
devbox run app                     # opens the generated demo song
devbox run app -- path/to/song.RPP # opens a REAPER project
devbox run test
```

Flags: `--demo` (open the demo song), `--play` (start playback on launch), `--render-project`,
`--no-input` (never
open the audio input for monitoring), `--view=START-END`
(open zoomed to a range in seconds), `--render=out.wav` (render the project without a window).
`scripts/cargo.sh` runs cargo with the devbox library paths: `devbox run -- scripts/cargo.sh clippy`.
`SCYTHE_UI_SCALE=0.75` scales the UI (0.5–3).

The demo stems are synthesised on first launch into `$XDG_DATA_HOME/scythe/demo`.

## Layout

| Crate | Contents |
|---|---|
| `scythe-core` | Project model (tracks, folders, items, takes, sends, markers, tempo map, envelopes, fades, MIDI sources), lossless `.RPP` reader/writer, pan and fader laws, undo history |
| `scythe-engine` | cpal output with a null-device fallback, lock-free transport, immutable render graph swapped via `arc-swap`, media decoding (symphonia) and zero-phase sinc resampling, peaks, meters, offline WAV bounce, envelopes, metronome, preview synth, recording, FX chains (ReaEQ/ReaVerb-compatible effects, gate, compressor, convolution), CLAP and VST 2 hosting |
| `clap-test-gain` | A minimal CLAP plugin the host tests load |
| `scythe-app` | egui UI: arrange view, mixer dock, inspector, action palette (Ctrl+K), demo generator |

`cargo run -p scythe-core --example rpp_roundtrip -- *.RPP` loads projects, writes them back
in memory and lists every line whose content changed. On 33 real REAPER projects
(~237k lines) only the header line differs.

## Status

Working:

- **Playback** of audio items (WAV/FLAC/MP3/OGG/AAC…), reversed and sectioned items
  (`<SOURCE SECTION>`), click-source items and MIDI items (through a built-in preview
  instrument: drums on channel 10, a soft synth elsewhere).
- **Mixing**: volume, pan with REAPER's pan laws, mute/solo, folders, sends, master, peak
  meters, master loudness (BS.1770 momentary and short-term LUFS, in the mixer header),
  REAPER's fader law (0 dB at 71.6 % of the travel, +12 dB at the top).
- **Fades** with REAPER's shapes and curvature, drawn on the items.
- **Track envelopes** (volume, pre-FX volume, trim, pan, mute) with all point shapes, shown
  and editable on the lanes (double-click adds, drag moves, right-click deletes). FX parameter
  envelopes stored in projects play back: bypass for every effect, wet/dry for ReaVerb and
  ReaDelay, and ReaDelay's per-tap enable, volume and feedback.
- **Tempo map** (`TEMPOENVEX`: tempo changes, linear ramps, time-signature changes) for the
  ruler, grid, snap, metronome and MIDI; tempo markers on the ruler, `Shift+C` inserts one at the
  cursor, the bpm control edits the tempo in effect at the cursor.
- **Metronome** (`C`), **recording** of armed tracks (`Ctrl+R`, mono or stereo inputs per the
  track's REAPER input setting) and **input monitoring**: an armed track with monitoring on
  hears its input through its FX chain, also while stopped (play guitar through a NAM amp and
  a cab IR live). Inspector: `In 1` picks the input, `Mon` toggles monitoring.
- **Editing**: move (snapped, across tracks), trim edges, fade handles, split, copy/cut/paste,
  duplicate, reverse, delete, markers, undo/redo. Several items: Ctrl+click or drag a marquee on
  empty lane space; moving, deleting, muting, copying (with track spacing) and duplicating then
  act on all of them. Overlapping items crossfade automatically after a move or trim.
  With a time selection (drag on the ruler): split items at it (`Shift+S`), remove its contents,
  remove them and close the gap (`Ctrl+Alt+X`, markers and envelopes follow), or insert empty
  space. Takes (`T` cycles), item mute (`U`), marker
  navigation (`[` `]`), fixed lanes drawn as lanes, snap grid 1/4…1/32 or bars (`Shift+G` cycles,
  `Alt+G` toggles snap), tracks moved with `Ctrl+↑/↓` and into or out of folders with
  `Ctrl+→/←`, track height with `PageUp`/`PageDown` (compact rows below 56 points). Click a
  track's colour dot for a palette; markers jump on click, rename on double-click and delete on
  right-click.
- **MIDI editor**: double-click a MIDI item (or `E`) for a docked piano roll: click adds a note on
  the grid, drag moves, drag the right edge resizes, right-click deletes, velocity lane below.
  Edits are written back into the item's `<SOURCE MIDI` (an edited item leaves its pool).
- **FX**: chains on tracks and master, rendered in parallel across tracks. ReaEQ, ReaVerb and
  ReaSamplOmatic5000 and ReaDelay settings stored in projects play through built-in equivalents
  (the sampler receives the track's MIDI; musical delay lengths follow the tempo map); ReaGate,
  ReaComp and ReaLimit through approximations. The same effects are offered as built-ins in the plugin
  browser (equalizer, gate, compressor, delay, convolution reverb) with editable parameters, and
  are saved in REAPER's own format, so REAPER opens them as ReaEQ, ReaGate, ReaComp, ReaDelay
  and ReaVerb. Neural Amp Modeler
  slots (the LV2 plugin) run their `.nam` WaveNet models natively, with the stored input and
  output levels (all 310 models in `~/music/Samples/NAM` load; ~11× realtime per instance). CLAP plugins are
  hosted, instruments included (scan `~/.clap`, `/usr/lib/clap`, `$CLAP_PATH`; add them from the
  inspector; click a slot for its parameters; their state is saved in the project) and Linux
  VST 2 plugins from `$SCYTHE_VST_PATH`/`$VST_PATH`/`~/.vst`.
- **Files**: lossless `.RPP` load/save, `<name>.RPP-bak` on save, auto-save every 5 minutes
  to `Backups/`, media import (dialog and drag-and-drop), render to WAV (whole project, time
  selection or stems from the action palette; headless with
  `scythe-app project.RPP --render=out.wav [--range=START-END]`), or with the project's own
  render settings like REAPER's `-renderproject` (`--render-project` or the palette): bounds
  project, time selection, custom range or all regions, `RENDER_FILE` and wildcards such as
  `$project`, `$regionnumber`, `$regionname`, date and time. MP3 renders go through the `lame`
  encoder (in the devbox packages) at the project's bitrate; other sinks (FLAC …) are written as
  WAV for now.

### Verified against REAPER

`crates/scythe-engine/examples/reaper_ref.rs`, `reaeq_ref.rs` and `reaverb_ref.rs` write small
projects, let REAPER 7.73 render them headless (with its own throw-away config directory) and
compare with scythe's output sample by sample:

| Area | Result |
|---|---|
| Pan laws (0/-3/-6 dB, compensated, sine/linear tapers), take pan, item/take gain | exact |
| Fade shapes 0–6, curvature for shapes 0–4 | exact; shapes 5–6 with curvature and shape 1 with negative curvature approximate |
| Envelopes: volume/trim/pre-FX/pan/mute, shapes linear/square/slow/fast start/fast end | exact; bezier approximate |
| Reversed sections | exact |
| ReaEQ, every band type, bandwidths 0.3–3 oct, shelves ±17 dB, output gain | exact (35/35 cases) |
| ReaVerb file-loader IR (a cab IR and a plate from `~/music/Samples`) | exact (≤ 2e-6) |
| ReaSamplOmatic5000 (kick and snare from `drumtest.RPP`, 44.1 kHz samples in a 48 kHz project) | levels and pan per channel identical; difference -27 to -32 dB from the resampling filter |
| FX parameter envelopes (`PARMENV`): ReaVerb wet ramp, ReaVerb bypass | within -33 dB overall, -50 dB during the ramp (applied every 64 samples) |
| ReaDelay: time, musical length (eighth notes at the project tempo), feedback, pan, wet/dry | exact; its low-/high-pass filters approximate (impulse response within 0.04) |
| Built-in effects written by scythe (EQ, delay, convolution) rendered by REAPER | identical (-130 dB); REAPER reads the stored states as its own ReaEQ/ReaDelay/ReaVerb |
| Overlapping items and fixed lanes (`FIXEDLANES 9`, `YPOS`) | exact (all lanes play) |

Hybrid pan taper at laws between -3 and 0 dB is approximate (≤ 2 %).

### Not there yet

- Plugin GUIs; FX parameter envelopes other than bypass, wet/dry and ReaDelay's taps; VST 3
  and general LV2 hosting. NAM models are run at
  the project rate without resampling to the model's rate.
- MIDI to VST 2 instruments, MIDI controller editing, MIDI input. (MIDI items do play through
  CLAP instruments, the built-in sampler and the preview synth.)
- Plugins without a built-in equivalent (ReaTune, third-party VST 3 such as Amp Room) pass audio
  through; the inspector says so per slot.
- Time-stretch/pitch (playrate is resampled), latency compensation for recordings and plugins.
- Render formats other than WAV and MP3 (FLAC, OGG …), mono renders.

REAPER's own plugins (`reaeq.vst.so` …) are not hosted: they only run inside REAPER and ask
the host for REAPER internals.
