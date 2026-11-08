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
| `scythe-engine` | cpal output with a null-device fallback, lock-free transport, immutable render graph swapped via `arc-swap`, media decoding (symphonia) and zero-phase sinc resampling, peaks, meters, offline WAV bounce, envelopes, metronome, preview synth, recording, FX chains (ReaEQ/ReaVerb-compatible effects, gate, compressor, convolution), CLAP, LV2, VST 2 and VST 3 hosting with editors and latency compensation, time-stretch |
| `clap-test-gain`, `lv2-test-plugins`, `vst3-test-plugin` | Minimal plugins the host tests load (one per format) |
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
- **Automation modes** per track (`AUTOMODE`: trim/read, read, touch, write, latch, latch
  preview), cycled from the inspector. While playing, fader and pan moves in touch, latch and
  write mode are recorded into the volume and pan envelopes.
- **Tempo map** (`TEMPOENVEX`: tempo changes, linear ramps, time-signature changes) for the
  ruler, grid, snap, metronome and MIDI; tempo markers on the ruler, `Shift+C` inserts one at the
  cursor, the bpm control edits the tempo in effect at the cursor.
- **Metronome** (`C`), **recording** of armed tracks (`Ctrl+R`, mono or stereo inputs per the
  track's REAPER input setting) and **input monitoring**: an armed track with monitoring on
  hears its input through its FX chain, also while stopped (play guitar through a NAM amp and
  a cab IR live). Inspector: `In 1` picks the input, `Mon` toggles monitoring. Recording with
  repeat on loops the time selection and makes every pass a take of one item (the last full
  pass active; incomplete first and last passes are dropped once a full pass exists). Pressing
  record while playing punches in at the playhead, and the "time selection auto-punch" record
  mode keeps only what falls inside the time selection.
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
  right-click. Razor edits: Alt+drag (over items too) marks a time range on every track it
  crosses (Shift+Alt adds more); `Delete` removes what lies inside, copy/cut take those pieces
  and paste keeps the space before them. `Ctrl+G` glues the selected items of a track into one
  rendered file; "Apply track FX to items as new take" renders each item through its track's
  effects into a new, active take.
- **MIDI editor**: double-click a MIDI item (or `E`) for a docked piano roll: click adds a note on
  the grid, drag moves, drag the right edge resizes, right-click deletes. The lane below edits
  velocity or, picked with its label, CC 1/7/10/11/64 or pitch bend (click or drag paints on the
  grid, right-click deletes). Edits are written back into the item's `<SOURCE MIDI` (an edited
  item leaves its pool). Controllers, pitch bend, program changes and aftertouch reach CLAP and
  LV2 instruments as MIDI and VST 3 instruments through their MIDI mapping.
- **MIDI files and input**: `.mid` files import (dialog or drag-and-drop) as in-project MIDI
  items, and "Item: Export MIDI item to file" writes one back out. A track whose record input
  is MIDI (all inputs, optionally one channel) plays its instrument live while armed with
  monitoring on, and records an in-project MIDI item; recording around a loop layers the passes
  into one take, as REAPER's MIDI overdub. A track can also send its MIDI (items and monitored
  input, optionally forced to one channel) to a hardware output (Sends tab), timed to sound
  with the audio; stopping sends all-notes-off. VST 2 instruments receive MIDI too.
- **ReaScript (Lua 5.4)**: "Script: Run ReaScript" runs a `.lua` file against the project as one
  undo step (named by `Undo_EndBlock`), with `ShowConsoleMsg` output in a console window. The
  `reaper.*` subset: tracks (`CountTracks`, `GetTrack`, `GetMasterTrack`, `GetSelectedTrack`,
  `InsertTrackAtIndex`, `DeleteTrack`, `GetTrackName`, `GetSetMediaTrackInfo_String`,
  `Get/SetMediaTrackInfo_Value`, `TrackFX_GetCount/GetFXName/GetEnabled/SetEnabled`), items
  (`CountMediaItems`, `GetMediaItem`, `CountTrackMediaItems`, `GetTrackMediaItem`,
  `GetSelectedMediaItem`, `Get/SetMediaItemInfo_Value`, `SplitMediaItem`, `DeleteTrackMediaItem`),
  markers (`AddProjectMarker`, `CountProjectMarkers`, `EnumProjectMarkers`), transport and time
  (`GetCursorPosition`, `SetEditCurPos`, `GetPlayState`, `OnPlayButton`, `OnStopButton`,
  `GetProjectLength`, `Master_GetTempo`, `TimeMap2_timeToQN/QNToTime`) and undo/console calls.
- **FX**: chains on tracks and master, rendered in parallel across tracks. FX chains save to and
  load from REAPER `.RfxChain` files, and tracks to and from `.RTrackTemplate` files (sends
  between the saved tracks kept), starting in REAPER's own resource folders. FX presets: the
  slot's context menu saves the plugin's current settings under a name and loads them back
  (`~/.local/share/scythe/presets/<plugin>/`). Plugin scans are cached per binary (path, size,
  modification time) in `~/.cache/scythe`, so only new or changed plugins are loaded. ReaEQ, ReaVerb and
  ReaSamplOmatic5000 and ReaDelay settings stored in projects play through built-in equivalents
  (the sampler receives the track's MIDI; musical delay lengths follow the tempo map), as do
  ReaGate, ReaComp and ReaLimit (lookahead, latency compensated). The same effects are offered as built-ins in the plugin
  browser (equalizer, gate, compressor, delay, convolution reverb) with editable parameters, and
  are saved in REAPER's own format, so REAPER opens them as ReaEQ, ReaGate, ReaComp, ReaDelay
  and ReaVerb. Neural Amp Modeler
  slots (the LV2 plugin) run their `.nam` WaveNet models natively, with the stored input and
  output levels (all 310 models in `~/music/Samples/NAM` load; ~11× realtime per instance). CLAP plugins are
  hosted, instruments included (scan `~/.clap`, `/usr/lib/clap`, `$CLAP_PATH`), as are LV2
  plugins (`$LV2_PATH`, `~/.lv2`, `/usr/lib/lv2` …; controls and state in REAPER's `<LV2` layout),
  VST 3 plugins (`$VST3_PATH`, `~/.vst3`, `/usr/lib/vst3` …; state in REAPER's VST 3 chunk layout)
  and Linux VST 2 plugins from `$SCYTHE_VST_PATH`/`$VST_PATH`/`~/.vst`. Add them from the
  inspector, click a slot for its parameters, and use **Edit** for the plugin's own editor (CLAP,
  VST 2, VST 3 and LV2 X11 UIs, in a separate X11 window). Plugin latency is compensated across tracks,
  sends and folders.
- **Time-stretch and pitch**: a take's play rate keeps its pitch when "Keep pitch" is on (REAPER's
  default), and takes can be pitch-shifted in semitones; both use a phase vocoder, prepared in the
  background. Recordings are shifted by the input and output latency the audio device reports.
- **Files**: lossless `.RPP` load/save, `<name>.RPP-bak` on save, auto-save every 5 minutes
  to `Backups/`, media import (dialog and drag-and-drop), render to WAV (whole project, time
  selection or stems from the action palette; headless with
  `scythe-app project.RPP --render=out.wav [--range=START-END]`), or with the project's own
  render settings like REAPER's `-renderproject` (`--render-project` or the palette): bounds
  project, time selection, custom range or all regions, `RENDER_FILE` and wildcards such as
  `$project`, `$regionnumber`, `$regionname`, date and time. MP3, FLAC and Ogg Vorbis renders go
  through `lame`, `flac` and `oggenc` (in the devbox packages) with the project's settings, and
  mono renders mix both channels down as REAPER does. Other sinks are written as WAV. The
  project's normalisation (`RENDER_NORMALIZE`: LUFS-I, RMS, peak, true peak, LUFS-M/S max to a
  target) and dither (`RENDER_DITHER`: TPDF, optionally noise-shaped, for 16- and 24-bit) apply.

### Verified against REAPER

`crates/scythe-engine/examples/reaper_ref.rs`, `reaeq_ref.rs` and `reaverb_ref.rs` write small
projects, let REAPER 7.73 render them headless (with its own throw-away config directory) and
compare with scythe's output sample by sample:

| Area | Result |
|---|---|
| Pan laws: sine and linear tapers at every law (sweeps from 0 to -6 dB and compensated to +6 dB), all three tapers at every law, pan envelopes on REAPER's 512-frame grid; take pan, item/take gain | exact |
| Fade-in and fade-out shapes 0–6 with every curvature (134 cases) | within 5e-4; shapes 5–6 with curvature use a time warp fitted to REAPER renders |
| Envelopes: volume/trim/pre-FX/pan/mute, all point shapes including bezier (tension, neighbouring points, REAPER's 256-step curve) | exact (bezier within 2e-4; a single frame off at tension ±1) |
| Reversed sections | exact |
| ReaEQ, every band type, bandwidths 0.3–3 oct, shelves ±17 dB, output gain, bands up to 21 kHz (REAPER holds the bandwidth warp at a quarter of the sample rate) | exact (35/35 cases, 10 high-frequency cases) |
| ReaVerb file-loader IR (a cab IR and a plate from `~/music/Samples`) | exact (≤ 2e-6) |
| ReaSamplOmatic5000 (kick and snare from `drumtest.RPP`, 44.1 kHz samples in a 48 kHz project) | levels and pan per channel identical; difference -27 to -32 dB from the resampling filter |
| FX parameter envelopes (`PARMENV`): ReaVerb wet ramp and bypass, a ReaEQ frequency sweep through REAPER's normalised parameter curves, set once per 512-frame block | exact for fixed values and bypass; the ReaEQ sweep within 0.3 % and the ReaVerb ramp at -49 dB, except REAPER's first block of a render, which ignores envelopes |
| ReaDelay: time, musical length (eighth notes at the project tempo), feedback, pan, wet/dry | exact; its low-/high-pass filters approximate (impulse response within 0.04) |
| Built-in effects written by scythe (EQ, delay, convolution) rendered by REAPER | identical (-130 dB); REAPER reads the stored states as its own ReaEQ/ReaDelay/ReaVerb |
| ReaGate (threshold, hysteresis, attack, hold, release, pre-open), ReaComp (ratio up to infinite, attack, release, RMS window) and ReaLimit (threshold, ceiling, release), mono and stereo detection | within 1e-3, ReaComp's soft knee (a fit to REAPER's curve) within 2e-3 (19/20 cases) |
| Send envelopes (`AUXVOLENV`, `AUXPANENV`, `AUXMUTEENV`) and take envelopes (`VOLENV`, `PANENV`, `MUTEENV`), followed sample by sample; mute crossfades over 4 ms with a raised cosine | within 1e-3 (6/6 cases) |
| Render normalisation: peak and LUFS-I (REAPER's gating windows reach back before the start) | exact; RMS within 1 % (REAPER's exact RMS measure is not known) |
| Overlapping items and fixed lanes (`FIXEDLANES 9`, `YPOS`) | exact (all lanes play) |

`examples/null_check.rs` re-renders any project REAPER rendered with `-renderproject` and
compares the two files.

### Not there yet

- FX parameter envelopes on hosted plugins map REAPER's value range linearly onto the
  plugin's parameter range; REAPER's own effects use the curves measured from REAPER. NAM models are run at the project rate without resampling
  to the model's rate. Hosted plugins are tested with the workspace's own test plugins.
- Multichannel tracks and routing: everything runs in stereo (every project in the corpus is
  `NCHAN 2` with plain stereo sends). REAPER's own `.ini` FX presets are not read; presets are
  kept as single-effect `.RfxChain` files instead. `MIDIHWOUT` follows the API's packing
  (device × 32 + channel); REAPER's file encoding of it is unverified.
- ReaScript covers a subset of the API (below) and no `defer` loops; no EEL or Python.
- Plugins that are not installed as Linux plugins (ReaTune, the Windows/macOS VST 3s in the
  corpus such as Amp Room) pass audio through; the inspector says so per slot.
- Time-stretch quality is a phase vocoder, not REAPER's élastique, so stretched takes do not
  null against REAPER.
- Render formats beyond WAV, MP3, FLAC and Ogg Vorbis (Opus, WavPack, video).

REAPER's own plugins (`reaeq.vst.so` …) are not hosted: they only run inside REAPER and ask
the host for REAPER internals.
