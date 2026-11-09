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
| `scythe-engine` | cpal output with a null-device fallback, lock-free transport, immutable render graph swapped via `arc-swap`, media decoding (symphonia, with REAPER's MP3 framing) and REAPER's resample modes, peaks, meters, offline WAV bounce, envelopes, metronome, preview synth, recording, FX chains (ReaEQ/ReaVerb-compatible effects, gate, compressor, convolution), CLAP, LV2, VST 2 and VST 3 hosting with editors and latency compensation, time-stretch |
| `clap-test-gain`, `lv2-test-plugins`, `vst2-test-plugin`, `vst3-test-plugin` | Minimal plugins the host tests load (one per format) |
| `scythe-app` | egui UI: arrange view, mixer dock, inspector, action palette (Ctrl+K), demo generator |

`cargo run -p scythe-core --example rpp_roundtrip -- *.RPP` loads projects, writes them back
in memory and lists every line whose content changed. On 33 real REAPER projects
(~237k lines) only the header line differs.

## Status

Working:

- **Playback** of audio items (WAV/FLAC/MP3/OGG/AAC…), reversed and sectioned items
  (`<SOURCE SECTION>`), click-source items and MIDI items (through a built-in preview
  instrument: drums on channel 10, a soft synth elsewhere). MP3s are read the way REAPER reads
  them: items keep the encoder delay and padding and play a Xing/Info header frame as a frame of
  silence, the sampler trims them (gapless) and holds them to 16-bit range, and both scale by
  32767/32768; mid/side granules of MPEG-2 and 2.5 streams come out 3 dB low, as REAPER's
  decoder plays them. Media at another rate, and takes at another play rate without "Keep pitch"
  (a conversion from the file's rate times the play rate), are converted with REAPER's default
  playback mode, the 64-point sinc, for playback; every render follows the project's
  `RENDER_RESAMPLE` mode (16- to 768-point sinc, linear, point, linear with one or two IIR
  low-passes, r8brain free).
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
- **Multichannel tracks** (`NCHAN`, up to 32 in the inspector's Sends tab): items with more
  channels play into channels 3 and up, sends take any channel pair (or one channel) to any
  pair of the destination (`AUXRECV` source and destination channels), folders carry every
  channel to their parent, a track can feed its parent from a channel offset (`MAINSEND`),
  and ReaGate and ReaComp listen to channels 3/4 when their detector input says so (sidechain).
  Effects follow their pin maps (VST and VST 3 state headers, `<JS_PINMAP`, LV2's `<IN_PINS`
  and `<OUT_PINS`): input pins sum the channels mapped to them, output pins replace theirs,
  other channels pass through, and input pins 3/4 of a stereo effect are its sidechain (JSFX
  see it as `spl2`/`spl3`). Effects with more pins (JSFX band splitters and mixers, VST 2 with
  more inputs or outputs, VST 3 buses and CLAP audio ports beyond the main pair, such as a
  sidechain input) work on all of them, by default pin n on channel n. VST 3 buses are set up
  for the track's width as REAPER 7.73 does: it offers each layout up to the track's channel
  count (mono, stereo, 3.0, 4.0, first-order ambisonics, 5.0, 5.1, 7.0, 7.1, then wider ones up
  to 16 channels) on every bus at once and keeps the last one the plugin takes, so a 6-channel
  track gets a 5.1 main bus (and sidechain); a new slot's header maps the input pins of the
  plugin's own buses and an output pin per track channel, and the buses are set up anew when
  the track's width changes. The master can
  have more channels too ("Master: More channels"): master effects work across them, and a
  render with more channels (`RENDER_CHANNELS`) writes them all.
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
  grid, right-click deletes); "Quantize" moves note starts onto the grid. Edits are written back
  into the item's `<SOURCE MIDI` (an edited item leaves its pool). Controllers, pitch bend,
  program changes and aftertouch reach CLAP and LV2 instruments as MIDI and VST 3 instruments
  through their MIDI mapping.
- **MIDI files and input**: `.mid` files import (dialog or drag-and-drop) as in-project MIDI
  items, and "Item: Export MIDI item to file" writes one back out. A track whose record input
  is MIDI (all inputs, optionally one channel) plays its instrument live while armed with
  monitoring on, and records an in-project MIDI item; recording around a loop layers the passes
  into one take, as REAPER's MIDI overdub. A track can also send its MIDI (items and monitored
  input, optionally forced to one channel) to a hardware output (Sends tab), timed to sound
  with the audio; stopping sends all-notes-off. VST 2 instruments receive MIDI too.
- **ReaScript (Lua 5.4, EEL2, Python)**: "Script: Run ReaScript" runs a `.lua`, `.eel` or `.py`
  file against the project as one undo step (named by `Undo_EndBlock`), with `ShowConsoleMsg`
  output in a console window. 246 `reaper.*` functions:
  - tracks: `CountTracks`, `GetTrack`, `GetMasterTrack`, `InsertTrackAtIndex`, `DeleteTrack`,
    `GetTrackName`, `GetSetMediaTrackInfo_String` (`P_NAME`, `GUID`, `P_EXT:`),
    `Get/SetMediaTrackInfo_Value` (volume, pan, mute, solo, arm, record input and monitoring,
    main send and its channel offset, folder depth, automation mode, channels, track number,
    selection, custom color, pan law, fixed lanes, MIDI hardware output, `P_PARTRACK`),
    `Get/SetTrackColor`, `GetParentTrack`, `GetTrackDepth`, `GetTrackUIVolPan`, selection
    (`CountSelectedTracks(2)`, `GetSelectedTrack(2)`, `IsTrackSelected`, `SetTrackSelected`,
    `SetOnlyTrackSelected`);
  - sends and receives: `CreateTrackSend`, `RemoveTrackSend`, `GetTrackNumSends`,
    `Get/SetTrackSendInfo_Value` (volume, pan, mute, channels, `P_DESTTRACK`/`P_SRCTRACK`),
    `GetTrackSendName`, `GetTrackReceiveName`;
  - effects: `TrackFX_GetCount/GetFXName/GetEnabled/SetEnabled/GetOffline/GetInstrument`,
    `TrackFX_AddByName`/`GetByName` (Scythe's own effects and installed CLAP, LV2 and VST 3
    plugins), `TrackFX_Delete`, `TrackFX_CopyToTrack` (copy or move), parameters of running
    effects (`TrackFX_GetNumParams`, `Get/SetParam(Ex)`, `Get/SetParamNormalized`,
    `GetFormattedParamValue`, `GetParamName`) and presets from REAPER's preset files
    (`TrackFX_GetPreset`, `GetPresetIndex`, `SetPreset`, `SetPresetByIndex`, `NavigatePresets`);
  - envelopes of tracks and takes: `CountTrackEnvelopes`, `GetTrackEnvelope(ByName/ByChunkName)`,
    `Count/GetTakeEnvelope(ByName)`, `GetEnvelopeName`, `GetEnvelopeScalingMode`,
    `ScaleTo/FromEnvelopeMode`, `CountEnvelopePoints(Ex)`, `Get/Set/InsertEnvelopePoint(Ex)`,
    `DeleteEnvelopePointEx`, `DeleteEnvelopePointRange(Ex)`, `GetEnvelopePointByTime(Ex)`,
    `Envelope_Evaluate`, `Envelope_SortPoints(Ex)`; volume envelopes give fader-scaled values,
    as REAPER's API does;
  - items, takes and sources: `CountMediaItems`, `GetMediaItem`, `Count/GetTrackMediaItem`,
    `GetMediaItem(_)Track`, `AddMediaItemToTrack`, `CreateNewMIDIItemInProj`, `SplitMediaItem`,
    `DeleteTrackMediaItem`, `MoveMediaItemToTrack`, `Get/SetMediaItemInfo_Value` (position,
    length, mute, volume, loop, fades with their shapes, active take, lane, selection, item
    number), `GetSetMediaItemInfo_String` (`GUID`, `P_EXT:`), `SetMediaItemPosition/Length`,
    selection (`CountSelectedMediaItems`, `GetSelectedMediaItem`, `IsMediaItemSelected`,
    `SetMediaItemSelected`, `SelectAllMediaItems`), `CountTakes`, `GetTake`, `GetActiveTake`,
    `SetActiveTake`, `AddTakeToMediaItem`, `GetMediaItemTake_Item/Track`, `GetTakeName`,
    `GetSetMediaItemTakeInfo_String`, `Get/SetMediaItemTakeInfo_Value` (offset, volume, pan,
    rate, pitch, `P_SOURCE`), `TakeIsMIDI`, `Get/SetMediaItemTake_Source`,
    `PCM_Source_CreateFromFile`, `GetMediaSourceFileName/Type/Length`;
  - MIDI in MIDI takes: `MIDI_CountEvts`, `MIDI_Get/Set/Insert/DeleteNote`,
    `MIDI_Get/Set/Insert/DeleteCC`, `MIDI_Sort`, PPQ conversions; selected and muted events are
    kept (`e`, `Em` lines) and muted ones do not play;
  - markers and regions: `AddProjectMarker(2)`, `CountProjectMarkers`, `EnumProjectMarkers(2)(3)`,
    `SetProjectMarker(2)(3)(4)`, `SetProjectMarkerByIndex(2)`, `DeleteProjectMarker(ByIndex)`,
    `GetLastMarkerAndCurRegion`, `ColorToNative`, `ColorFromNative`;
  - transport, time selection and tempo: `GetCursorPosition(Ex)`, `SetEditCurPos`,
    `MoveEditCursor`, `GetPlayState`, `GetPlayPosition(2)(Ex)`, `OnPlayButton`, `OnStopButton`,
    `GetSet_LoopTimeRange(2)`, `GetSetRepeat(Ex)`, `Master_GetTempo`, `SetCurrentBPM`, tempo and
    time signature markers (`Count/Get/Set/Add/DeleteTempoTimeSigMarker`, a signature moving to
    the next bar line as in REAPER), `TimeMap(2)_timeToQN/QNToTime`,
    `TimeMap(2)_GetDividedBpmAtTime`, `TimeMap_GetTimeSigAtTime`, `TimeMap2_timeToBeats`,
    `TimeMap2_beatsToTime`, `TimeMap_GetMeasureInfo`, `TimeMap2_GetNextChangeTime`,
    `format_timestr(_pos)`, `parse_timestr(_pos)`;
  - the project: `GetProjectName`, `GetProjectPath(Ex)`, `EnumProjects`, `GetProjectLength`,
    `GetSetProjectInfo` (sample rate and its use, render channels and rate),
    `GetSetProjectInfo_String` (title, author, render file and pattern, record path),
    `GetSetProjectNotes`, `GetSetProjectAuthor`, `Set/Get/EnumProjExtState` (kept in the
    project's `<EXTSTATE`, as REAPER writes it), `Get/Set/Has/DeleteExtState` (persisting to
    `reaper-extstate.ini` in Scythe's data folder), `Main_OnCommand` (REAPER's command ids for
    transport, track and item edits, selection, markers and the time selection run inside the
    script; undo, redo, file, render, glue, copy and paste, zoom and other actions Scythe has
    run once the script's changes are in), `NamedCommandLookup`, `GetToggleCommandState`,
    `Undo_*` (`Undo_CanUndo2`, `Undo_DoUndo2` …), `MarkProjectDirty`, `IsProjectDirty`;
  - dialogs and the rest: `ShowMessageBox` and `GetUserInputs` (modal windows; the script
    waits for the answer), `ValidatePtr(2)`, `DB2SLIDER`, `SLIDER2DB`, `mkvolstr`, `mkpanstr`,
    `GetAppVersion`, `GetOS`, `GetResourcePath`, `GetExePath`, `time_precise`, console and
    refresh calls.

  `defer` and `runloop` keep a script running: its deferred functions run once per frame over
  the current project until it stops deferring or "Script: Stop the running script" ends it,
  which runs its `atexit` functions. Lua's `gfx.*` and EEL2's `gfx_*` draw in a window that
  `gfx_init` opens (the JSFX graphics: shapes, text, images, blits), with `gfx_getchar`
  returning -1 once it is closed. EEL2 scripts call the same functions without `reaper.`, with
  outputs passed as variables or `#strings` (`GetTrackName(tr, #name)`) and pointers as
  numbers; `defer("code")` takes code, `eval("code")` runs code, and the file (`fopen`,
  `fread`, `fgets`, `fgetc`, `fwrite`, `fprintf`, `fseek`, `ftell`, `feof`, `fflush`,
  `fclose`) and TCP functions (`tcp_connect`, `tcp_listen`, `tcp_send`, `tcp_recv`,
  `tcp_set_block`, `tcp_close`) work. Python scripts (`from reaper_python import *`, then
  `RPR_GetTrack(0, 0)`) run in a `python3` process and get REAPER's return conventions: a tuple
  of the value and every argument when a call has outputs, pointers as `(MediaTrack*)0x…`
  strings, `RPR_defer("step()")` taking code or a function. As in REAPER, `print` goes to
  Scythe's standard output, not the console, and what a script writes to `sys.stderr` is shown
  above the traceback only when it fails.
- **FX**: chains on tracks and master, rendered in parallel across tracks. FX chains save to and
  load from REAPER `.RfxChain` files, and tracks to and from `.RTrackTemplate` files (sends
  between the saved tracks kept), starting in REAPER's own resource folders. FX presets are
  REAPER's own per-plugin preset files (`presets/vst-<plugin>.ini`, `vst3-…`, `js-…`,
  `clap-…`, `lv2-…` in REAPER's resource folder): the slot's context menu lists the presets
  saved in REAPER, loads one into the plugin, saves the plugin's current settings under a name
  so REAPER lists it too, and renames and deletes them. Like REAPER it first imports a plugin's
  default library (the `.rpl` beside a VST 2 binary, such as REAPER's stock ReaComp presets, or
  beside a JSFX) when the library changed since `LastDefImpTime`, and preset libraries (`.RPL`)
  import into and export from the preset file. Factory presets follow: a VST 3 plugin's
  `.vstpreset` files under `~/.vst3/presets/<vendor>/<plugin>/`, a VST 2 plugin's own programs
  (REAPER restores the chunk, then selects the stored program) and the presets a CLAP plugin's
  preset-discovery provider lists, loaded through its preset-load extension; like REAPER, only
  the provider whose id is the plugin's id is asked. Loading a preset does what REAPER does
  with the `<VST` header: the preset's pin maps and flag bits replace the
  slot's, and its state is taken only when its state kind matches. Presets saved as `.RfxChain`
  files by earlier versions are still listed. Plugin scans are cached per binary (path, size,
  modification time) in `~/.cache/scythe`, so only new or changed plugins are loaded. ReaEQ, ReaVerb and
  ReaSamplOmatic5000 and ReaDelay settings stored in projects play through built-in equivalents
  (the sampler receives the track's MIDI, live input and MIDI an earlier effect sends; musical
  delay lengths follow the tempo map), as do
  ReaGate, ReaComp and ReaLimit (lookahead, latency compensated). JSFX run from their own
  source through an EEL2 interpreter (sections, sliders, imports, the `<? ?>` preprocessor,
  user functions with locals and instances, memory, strings, MIDI, FFT and MDCT in REAPER's
  bin order and scale, files): every JSFX in the corpus and all 219 of REAPER's stock effects.
  Arithmetic follows REAPER's compiled EEL2 on x86-64: an assignment stores 0 for NaN,
  infinities and the smallest values unless its value comes from an operation that cannot make
  them out of clean inputs (`+ 1`, `cos`, `floor`, integer operators, ... are clean; `* 2`,
  `abs`, `min`, negation pass their inputs on; `/`, `*` by a variable, `log`, `exp`, `tan` are
  checked); constant expressions are folded at load (non-finite results become 0, `1 << 40` is
  a 32-bit shift) and `x + 0`, `x * 1`, `x * 0` are simplified; arithmetic flushes subnormal
  results to zero; comparisons with NaN take x86's unordered flags (`<`, `<=`, `==` true);
  `min`/`max` give their first argument for NaN, except that `max` gives its second where
  REAPER compiles it for a pointer result (an argument of `memset`, `sprintf`, `gfx_*` and the
  other builtins taking pointers, or a branch of a `?:` that is assigned or passed on); the
  library functions (`exp`, `pow`, `sin`, ...) flush subnormal results too; integer
  conversions give the most negative integer for NaN and out-of-range values; `<<`/`>>` shift
  the low 32 bits; `invsqrt` is the bit-trick estimate; `loop()` evaluates to 1. EEL2 has no
  exponents: a number run into letters (`1e5`, `1.5e+3`, `2x`) fails to compile, and a section
  that does not compile is left out while the others run, as REAPER does.
  File sliders list their folder under `Data` and are stored by file name as REAPER does;
  `@serialize` state is read from and written to `<JS_SER` (32-bit floats), and `@serialize`
  runs when REAPER runs it: on load after `@init` and `@slider` (writing), again after the
  slot's slider values (`@slider`), reading the stored state and writing once more, and once
  per save; an effect without `@serialize` is initialised once for its first playback;
  `file_open` reads WAV files as samples and text one value per item. Effects with `@gfx`
  (analyzers, scopes, the channel mapper) open their own view from the Edit button: `@gfx`
  runs about 30 times a second on a thread of its own beside the audio, as REAPER runs it,
  sharing the effect's variables, memory, strings and images (each value is an atomic, so the
  two race a value at a time as REAPER's do), with the mouse (buttons, modifiers, wheel),
  typed keys and files dropped onto the view (`gfx_getdropfile`). It draws into pixels as REAPER
  does: rectangles, lines, circles, arcs, triangles, gradients and pixels blend with `gfx_a`
  and `gfx_mode` (additive too), into the screen or any of 1024 images (`gfx_dest`,
  `gfx_setimgdim`), colours taken to a byte as REAPER truncates them; font 0, and a font
  before `gfx_setfont` names a face, is Scythe's own 8-pixel bitmap font with REAPER's
  measures; other text is drawn in the face `gfx_setfont` names (found through fontconfig;
  bold, italic, underline, turned, and outlined, shadowed, inverted and one-colour as LICE
  draws them) and measures as it draws. Text in a box (`gfx_drawstr` with flags) is placed and
  clipped as REAPER places it, font 0 drawing into a window on the box and leaving
  `gfx_x`/`gfx_y` measured from that window, other fonts aligning each line and leaving them
  after the text. `gfx_blit` and `gfx_blitext` scale, turn and filter; `gfx_deltablit` and
  `gfx_transformblit` step through the source in LICE's fixed point with its filter;
  `gfx_loadimg` reads PNG and JPEG files from the effect's folder or `Data`; `gfx_getpixel`,
  `gfx_blurto` and `gfx_muladdrect` work on the pixels; `gfx_showmenu` waits inside the script
  for a choice from a menu at the mouse while the audio goes on; `gfx_clear = -1` keeps what
  was drawn; a script that sets `gfx_ext_retina` gets the screen's pixels per point there and
  draws at the screen's own pixels; and sliders it sets take effect as if moved by hand. The
  view uploads the screen only when it changed. They
  run in REAPER's 512-frame blocks, play the track's MIDI and pass what they send on to the
  next effect in the chain; they are read from `~/.config/REAPER/Effects` and the
  installed REAPER's `InstallData/Effects` (or `$SCYTHE_JSFX_PATH`). The same effects are offered as built-ins in the plugin
  browser (equalizer, gate, compressor, delay, convolution reverb) with editable parameters, and
  are saved in REAPER's own format, so REAPER opens them as ReaEQ, ReaGate, ReaComp, ReaDelay
  and ReaVerb. Neural Amp Modeler
  slots (the LV2 plugin) run their `.nam` WaveNet models natively, with the stored input and
  output levels (all 310 models in `~/music/Samples/NAM` load; ~11× realtime per instance),
  at the model's own sample rate: like the plugin, a project at another rate is converted to
  the model's rate and back (192-point sinc, reported as plugin latency). The NAM slot's **Edit**
  opens the NAM panel: the model's name, gear, author, architecture and rate, input and output
  knobs, previous/next model (Alt+Left/Right), four snapshots per slot to A/B models and levels
  (click to switch, Shift+click to store, Alt+1…4), and a browser with Library, Recent and
  TONE3000 tabs. The library indexes folders you add (read only, e.g. `~/music/Samples/NAM`) and
  models downloaded from TONE3000 (`~/.local/share/scythe/nam`), with search, favourites and
  tags; impulse responses load into a convolution slot after the NAM slot. Switching models is
  click-free: the new model is read off the audio thread, warms up beside the old one and the two
  crossfade over 12 ms. Signing in to TONE3000 (OAuth with PKCE in the system browser, tokens in a
  0600 file) needs a publishable key you register yourself; see [docs/tone3000.md](docs/tone3000.md).
  The model path stays in the LV2 plugin's own state, so REAPER keeps opening the slot; snapshots
  go into the track's `<EXT` extension state, which REAPER keeps. `.nam` files dropped on a track
  load into its NAM slot (or a new one). CLAP plugins are
  hosted, instruments included (scan `~/.clap`, `/usr/lib/clap`, `$CLAP_PATH`), as are LV2
  plugins (`$LV2_PATH`, `~/.lv2`, `/usr/lib/lv2` …; controls and state in REAPER's `<LV2` layout),
  VST 3 plugins (`$VST3_PATH`, `~/.vst3`, `/usr/lib/vst3` …; state in REAPER's VST 3 chunk layout)
  and Linux VST 2 plugins from `$SCYTHE_VST_PATH`/`$VST_PATH`/`~/.vst`. Add them from the
  inspector, click a slot for its parameters, and use **Edit** for the plugin's own editor (CLAP,
  VST 2, VST 3 and LV2 X11 UIs, in a separate X11 window). Plugin latency is compensated across tracks,
  sends and folders.
- **Time-stretch and pitch**: a take's play rate keeps its pitch when "Keep pitch" is on (REAPER's
  default), and takes can be pitch-shifted in semitones, prepared in the background. The take's
  pitch shifter (`PLAYRATE` field 4, else the project's `DEFPITCHMODE`) picks the engine:
  SoundTouch with REAPER's settings for its nine sub-modes (sequence/seek/overlap 82/14/12 ms;
  high quality 41/28/12 ms with a 128-tap anti-alias filter; fast 164/7/6 ms with quick seek and
  no anti-alias filter; multi-mono runs one instance per channel), and Rubber Band (with the
  `rubberband` feature) with REAPER's options per sub-mode (real-time; formants, mid/side,
  independent phase, smoothing, transients, detector, pitch mode, window). Both are fed the way
  REAPER's take reader feeds them: 512 output frames at a time, 512 more source frames whenever
  the shifter falls short, latency kept. élastique (REAPER's default, proprietary) and REAPER's
  own simple windowed, Rrreeeaaa and ReaReaRea modes use Scythe's phase vocoder. Recordings are shifted by the input and output latency the audio device reports.
- **Files**: lossless `.RPP` load/save, `<name>.RPP-bak` on save, auto-save every 5 minutes
  to `Backups/`, media import (dialog and drag-and-drop), render to WAV (whole project, time
  selection or stems from the action palette; headless with
  `scythe-app project.RPP --render=out.wav [--range=START-END]`), or with the project's own
  render settings like REAPER's `-renderproject` (`--render-project` or the palette): bounds
  project, time selection, custom range or all regions, `RENDER_FILE` and wildcards such as
  `$project`, `$regionnumber`, `$regionname`, date and time. MP3, FLAC, Ogg Vorbis, Opus and
  WavPack renders go through `lame`, `flac`, `oggenc`, `opusenc` and `wavpack` (in the devbox
  packages) with the project's settings (Opus: bitrate per channel, VBR/CVBR/hard CBR,
  complexity; WavPack: 16/24/32-bit integer or 32-bit float, fast/normal/high/very high), and
  mono renders mix both channels down (in double precision) as REAPER does. WAV is written as
  8-bit unsigned, 16, 24 or 32-bit integer, 32 or 64-bit float, 4-bit IMA ADPCM or REAPER's
  2-bit cADPCM. Integer samples are scaled by 2^(bits-1) and rounded half up, as REAPER
  writes them; ADPCM follows REAPER's coder (codes rounded down after taking half a step off,
  the prediction wrapping at 16 bits, 1024-byte blocks per channel, the last block filled up by
  coding silence). REAPER's video sink (FFmpeg) goes through `ffmpeg`: AVI, MPEG-1, MPEG-2
  (transport stream), MOV, MP4, Matroska, FLV and WebM with the codecs REAPER lists for each
  (H.264, H.265, AV1, VP8, VP9, MPEG-1/2, XviD, FLV1, FFV1, Huffyuv, MJPEG, DV; MP3, AAC,
  AC-3, MP2, Vorbis, Opus, 16/24-bit and float PCM, or no stream), the video and audio bitrate,
  frame size (a side of 0 follows 16:9 from the other, both 0 is 1920x1080: REAPER's sink does
  not read the project's video size) and rate, MJPEG's quality factor, and the audio and video
  encoder option strings (`key=value` pairs up to a `;` or `#`: `codec`, `tag`, `q`/`qscale`,
  `range=jpeg`, and any other encoder option such as `crf`, `preset`, `g` or `b`). Frames are
  black where no video item plays; video items (top track first) are converted to YV12 and
  scaled with REAPER's scaler (bilinear from the top-left corner, no half-pixel centring) into
  the frame keeping their aspect ratio with black bars, or stretched. A clip shows on the frames
  whose middle it covers, each with the source frame on screen at that middle (B-frame streams
  timed by their decoding timestamps, as REAPER does). Video items play their own audio,
  decoded by FFmpeg. Three stock video processor presets are rendered, recognised by their
  code, as take or track FX: "Track opacity/zoom/pan" (without rotation or a forced size),
  "Invert colors" and "Brightness/Contrast". Audio CD images (`.cue` and a 44.1 kHz 16-bit `.bin`, as REAPER's CD image sink
  writes them) take their tracks from markers (each at least 4 seconds after the previous track;
  `PERF=`, `WRIT=` and `ISRC=` markers fill in the track's cue entries; optionally only `#`
  markers), from regions (audio outside regions left out) or as one track, after an optional
  lead-in of silence. DDP 2.00 filesets (DDPID, DDPMS, PQDESCR, CDTEXT.BIN, the image and
  checksum.md5) follow REAPER's DDP sink: `#` markers start tracks, `!` markers set INDEX 0,
  `@` markers hold the album, `KEY=value|…` metadata becomes ISRC, UPC/EAN and CD-TEXT (title,
  performer, songwriter, composer, arranger, message, disc identification, genre, language), and
  the Red Book rules (2 s before the first track, 4 s minimum tracks) move indexes as REAPER
  moves them. As in REAPER, a CD image or DDP render of other than two channels fails with
  "Unable to create output file …, sample rate or channel count may not be compatible" and
  leaves the empty `.cue` and `.bin` (or image) behind. Other sinks (AIFF, CAF, GIF, LCF …) are written as WAV. The
  project's normalisation (`RENDER_NORMALIZE`: LUFS-I, RMS, peak, true peak, LUFS-M/S max to a
  target) and dither (`RENDER_DITHER`: TPDF, optionally noise-shaped, for 8-, 16- and 24-bit) apply.

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
| ReaEQ, every band type, bandwidths 0.3–3 oct, shelves ±17 dB, output gain, bands up to 21 kHz (REAPER holds the bandwidth warp at a quarter of the sample rate); the alternative band, whose width grows with its gain (the stock "Basic 11 band Kick" preset) | exact (35/35 cases, 10 high-frequency cases, 7 alternative-band cases) |
| Sample-rate conversion of media: impulses at 44.1, 88.2 and 96 kHz in a 48 kHz project, as items and through the sampler (a Blackman-Harris sinc over 96 input frames each side, cut off at 1/1.03 of the output's Nyquist when going down, read from a 32-step table) | exact |
| Resample modes (`RENDER_RESAMPLE` 0–9): impulses up (44.1 → 48 kHz) and down (88.2 → 48 kHz) and white noise up (44.1 → 48 kHz) and down (48 → 44.1 kHz) per mode. Sinc: Blackman-Harris over 8/32/96 input frames each side read from a 32-step table (16/64/192 points), over 192/256/384 computed exactly (384/512/768 points); linear; point (the frame before the read position, so exact hits read the previous frame); linear plus an RBJ low-pass (Q 0.707) at 0.693 (one stage) or 0.792 (two stages) of the lower Nyquist frequency, run at the higher rate | within 5e-6 (35/36 cases); point sampling misses one frame of noise (REAPER reads the exact hit at 147 input frames the other way); r8brain (mode 10) is the next row |
| r8brain free (`RENDER_RESAMPLE 10`), a Rust implementation of r8brain-free-src 6.x (MIT, Aleksey Vaneev) with its `CDSPResampler` defaults: 2 % transition band, 206.91 dB attenuation, linear phase. Found by sweeping the two settings against impulse renders (TB 1/2/3, 136.45/180.15/206.91 dB: only 2/206.91 nulls). Impulses 44.1 → 48 and 88.2 → 48 kHz, noise for 31 rate pairs from 8 to 192 kHz covering every stage r8brain picks (2x/3x FFT convolution, whole-step and spline fractional interpolation, intermediate interpolation, half-band up- and downsamplers) | within 1e-7 (33/33) |
| Play rates without "Keep pitch" (0.5, 0.9, 1.5) in resample modes 0, 1, 3, 4 and 10, and from a 44.1 kHz file in a 48 kHz project: REAPER resamples by the play rate with the render's mode, the same conversion as from the file's rate times the play rate (output frame `j` reads the source at `j * rate`) | within 1e-5 (17/18); the 384-point sinc at 0.9 within 3e-5 |
| SoundTouch and Rubber Band takes (sub-modes 0/1/2/6 and 0/1/2/4/8/16/32/48/144; rates 0.75 and 1.5, +5 semitones) through the system libraries (SoundTouch 2.4.1, Rubber Band 4.0.0), where REAPER ships SoundTouch 1.9.0 and Rubber Band 1.8 (KissFFT, Speex resampler) | no null: same pitch and length, signal envelopes correlate 0.91-0.98 (SoundTouch) and 0.99 (Rubber Band) with REAPER's, 115-333 samples apart; long-term spectra within 0.3-0.8 dB (the phase vocoder standing in for élastique and simple windowed: 1-4.7 dB). SoundTouch 2.x changed the algorithm: no shift of its output comes near REAPER's, while SoundTouch 1.9.0 driven the same way matches REAPER's sample for sample outside each sequence's overlap |
| MP3 media items at their own rate: LAME-tagged (CBR, VBR, 44.1/48/32 kHz, mono, a 160 kbps LAME 3.92 crash cymbal), untagged, MPEG-2 (22.05/24 kHz mono and plain stereo, a 64 kbps 22.05 kHz joint-stereo file) | within 1e-6 (18/18 cases); REAPER plays the Xing/Info frame as 1152 (MPEG-2: 576) frames of silence, keeps encoder delay and padding, and scales by 32767/32768 |
| MP3 mid/side stereo in MPEG-2 and 2.5 (LAME `-m j` and `-m f` at 8/11.025/16/22.05/24 kHz, CBR and VBR, a 22.05 kHz file mixing left/right and mid/side frames; `-m s`, `-m d`, mono and MPEG-1 `-m j`/`-m f` as controls). REAPER's decoder plays every mid/side granule of an LSF stream at exactly 1/sqrt(2) of the correct level (MPEG-1 is right): the mid/side factor is applied twice. Scythe lowers those granules' global gain by 2 steps before symphonia decodes them, which is the same scaling ahead of the IMDCT, so transitions between frame modes match too | within 1e-6 (14/14) |
| MP3 through ReaSamplOmatic5000 (the crash cymbal at 44.1 and 48 kHz) | within 3e-5: gapless, clipped to 16-bit range, one 16-bit step off on 0.06 % of the samples |
| ReaVerb file-loader IR (a cab IR and a plate from `~/music/Samples`) | exact (≤ 2e-6) |
| ReaSamplOmatic5000 (kick and snare from `drumtest.RPP`, 44.1 kHz samples in a 48 kHz project), with its voice limit: a note beyond the limit takes over the oldest voice, which fades out over the 512-frame block holding that note (or never sounds when it started in the same block); retriggers within a block, zero-length notes, 1 to 9 voices, a velocity range | exact (18 cases); a 14-sampler drum kit (seven MP3 cymbals) within 1e-4 |
| FX parameter envelopes on hosted plugins: a ramp, a square step and a jump on CLAP and VST 3 gains (REAPER sends the value at each 512-frame block's last frame) and on a JSFX volume (its first frame, rounded to the slider step, with `@slider` and `@block` running as REAPER runs them) | exact (3 cases) |
| FX parameter envelopes on an LV2 gain: linear, square, bezier and fast/slow shapes, points on and a rounding error short of block boundaries, several points in a block (REAPER sends a block's start value, or, when the block holds points, the value at its first point's frame, the offset floored from the point's distance to the block's start time) | exact (3 cases) |
| JSFX arithmetic with NaN, infinities and subnormals: 394 expressions over 9 probes (assignment checks per operator and function, constant folding, simplifications, comparisons, integer conversions and shifts, `min`/`max`, user function parameters, memory, `loop`/`while` values) | exact |
| FX parameter envelopes (`PARMENV`): ReaVerb wet ramp and bypass, a ReaEQ frequency sweep through REAPER's normalised parameter curves, set once per 512-frame block | exact for fixed values and bypass; the ReaEQ sweep within 0.3 % and the ReaVerb ramp at -49 dB, except REAPER's first block of a render, which ignores envelopes |
| ReaDelay: time, musical length (eighth notes at the project tempo), feedback, pan, wet/dry, low- and high-pass (biquads with a Q of 1, fitted to its impulse responses) | exact (7 cases) |
| Built-in effects written by scythe (EQ, delay, convolution) rendered by REAPER | identical (-130 dB); REAPER reads the stored states as its own ReaEQ/ReaDelay/ReaVerb |
| ReaGate (threshold, hysteresis, attack, hold, release, pre-open), ReaComp (ratio up to infinite, attack, release, RMS window) and ReaLimit (threshold, ceiling, release), mono and stereo detection | within 1e-3, ReaComp's soft knee (a fit to REAPER's curve) within 2e-3 (19/20 cases) |
| Send envelopes (`AUXVOLENV`, `AUXPANENV`, `AUXMUTEENV`) and take envelopes (`VOLENV`, `PANENV`, `MUTEENV`), followed sample by sample; mute crossfades over 4 ms with a raised cosine | within 1e-3 (6/6 cases) |
| Multichannel: 4-channel items, sends from channels 3/4 and single channels, pan and fader on extra channels, folders and parent channel offsets, ReaComp sidechain, effect pin maps (inputs from 3/4, outputs to 3/4 with 1/2 passing through, summed input channels; VST, JSFX and LV2), JSFX with more pins (3-band splitter, 8-into-2 mixer, expander with sidechain) | exact (21 cases) |
| Render normalisation: peak and LUFS-I (REAPER's gating windows reach back before the start) | exact; RMS within 1 % (REAPER's exact RMS measure is not known) |
| JSFX from the corpus (1175 compressor, soft clipper, master limiter, stereo enhancer, volume scaler, delay, time adjustment) with the projects' slider settings | within 1e-6 |
| JSFX FFT and MDCT: `fft`/`ifft`/`fft_real`/`ifft_real`, permutation, `convolve_c`, `mdct`/`imdct` at every size limit (33 probes), and the stock effects built on them (phase adjust, FFT splitter, MDCT filter, volume sweep and pitch shift, auto peak filter, spectral hold, spectral paint, Thunderkick) | within 1e-5 (10/10 effects) |
| JSFX files: text items (expressions, comments, separators), 16/24-bit and float WAV, `file_riff`/`file_avail`/`file_mem` (7 probes); amp model (single and dual, impulse responses chosen by name), MIDI key map and MIDI delay (milliseconds and quarter notes) | exact (6 effects) |
| WavPack renders (16-bit, 24-bit, 32-bit float) decoded and compared with REAPER's, which also checks integer rounding in all renders; Opus at REAPER's default settings | identical samples; Opus within 0.012 (a lossy encoder) |
| CD images and DDP filesets of a test signal: tracks from markers (the 4-second rule, `#`-only, lead-ins of 250, 256 and 333 ms, the first track named by the marker at the start, `PERF=`/`WRIT=`/`ISRC=` markers, a later start), from regions (overlapping, cut by the bounds, gaps left out); DDP with `#`/`!`/`@` markers, Red Book index moves, ISRC, UPC/EAN, every CD-TEXT field, genre and language; 32-bit integer WAV and WavPack | byte-identical files (14 images and filesets); 32-bit integer samples identical |
| CD image and DDP renders of a mono and a 4-channel project | same empty files (`.cue` and `.bin`; `OUT.DAT`) and the same error; 48 kHz projects render at 44.1 kHz as REAPER's do |
| JSFX font 0: `gfx_texth` (0 before the first `@gfx`, then 8), `gfx_measurestr`/`gfx_measurechar` of every byte and of strings with spaces, tabs, `\r` and `\n`, multi-byte UTF-8, the empty string, and `gfx_x`/`gfx_y` after `gfx_drawstr`, `gfx_drawchar` and `gfx_drawnumber` (read back through `gmem` from REAPER's JSFX window) | same values (34 cases) |
| Python ReaScripts: `print`, `sys.stderr` and `RPR_ShowConsoleMsg` | same destinations: standard output, the error report (with the traceback, when the script fails), the console |
| ReaScript API: one Lua script (sends, track colors and selection, markers and regions with colors, time selection, repeat, tempo and time signature markers and the time map, `format_timestr`, `mkvolstr`, fader scaling, a volume envelope's points and `Envelope_Evaluate`, MIDI notes and CCs with selection and mute, PPQ conversions, items, takes and sources, project and global extended state, project title and notes, `Main_OnCommand(40001)`), one EEL2 script (`eval`, `extension_api`, file functions, MIDI with optional arguments left out, `#retval` strings) and one Python script, run in REAPER and in Scythe with their outputs compared | same values (126 lines), but for beat positions REAPER gives as 1e-11 where Scythe gives 0; REAPER 7.73's Python module fails some calls (`RPR_MIDI_InsertNote`, `RPR_GetTakeName` of an unnamed take, `RPR_ValidatePtr`) and gives region names as empty strings, which Scythe does not copy |
| WAV 8-bit, 16-bit and 64-bit float, IMA ADPCM and cADPCM of a test signal with 8-bit rounding ties and clipped samples, stereo and mono | identical sample data (10 files; ADPCM data chunks byte for byte) |
| Video sink: 12 container/codec pairs (AVI XviD/MP3, MPEG-1, MPEG-2 TS, MOV and Matroska with PCM, MP4 AAC, FLV, WebM VP8/Vorbis and VP9/Opus, FFV1, Huffyuv, audio only) without video items, then video items (aspect kept, stretched, two tracks) at 25 and 30 fps | same containers, codecs, sizes, frame rates, frame counts and durations; black frames decode identically; PCM audio identical, lossy audio within the codecs' own error; video items on the same frames showing the same source frames (exact at 30 fps; REAPER's 25 fps render of a 25 fps file at half-frame offsets picks a neighbouring frame now and then), MPEG-4 items scaled within 30-32 dB PSNR (26 and 23 dB with FFmpeg's bilinear scaler) |
| Video sink settings: frame sizes 0x0 (with a 640x360 project video size), 640x0 and 0x480; H.264 option strings (`crf`, `preset`, `g`, `b`, `range=jpeg`, comments), audio `b`; MJPEG quality 0, 50, 80, 95 and 100 with and without `qscale` | 1920x1080 and 640x360 as REAPER (0x480 crashes REAPER); same x264 settings in the stream (rate control, bitrate, keyframe interval, subme, B-frames) and full range; MJPEG sizes within 2% |
| Video item scaling of a high-detail test pattern: 1:1, 2.5x and 4x up, 16:9 with bars and stretched, 4x down | 1:1 identical; up 36.8-40.3 dB (14-17 dB with FFmpeg's bilinear scaler), down 23.2 dB (11 dB) |
| Video items' own audio: PCM in Matroska and MOV at 48 and 44.1 kHz, AAC in MP4, with item position and offset | PCM at the render rate identical; resampled and AAC streams within -30 dB (REAPER's video audio drifts by a fraction of a sample) |
| Video processors: opacity 0.5 over a lower track, zoom and pan with and without aspect and filtering, invert colours (track and take FX), brightness/contrast | brightness/contrast identical; invert 37-40 dB; opacity 22 dB and zoom/pan 18-21 dB (REAPER blits the lower track's own video by nearest neighbour, and its zoomed rectangle is not matched exactly) |
| JSFX `@serialize`: state restored after `@init`/`@slider` (and lost to `@init` at playback start without `ext_noinit`), written byte for byte as REAPER does; ReaSamplOmatic5000 playing the notes a JSFX lets through | exact (3 cases) |
| JSFX run order on load and at render start: `@init`, `@slider` and `@serialize` reads and writes counted by a probe effect, with and without stored state and slider values, and without `@serialize` (renders, and gmem read after adding, saving and opening in REAPER) | exact (5 renders) |
| JSFX `gfx_drawstr` in a box: font 0 and a TrueType font, 41 flag/box/start combinations from 4 starting points (negative, fractional, off the right edge), multi-line text, empty and inverted boxes; `gfx_x`/`gfx_y` after and where the ink lands (`gmem` and `gfx_getpixel` in REAPER's JSFX window) | same positions (font 0 glyphs differ, ink within a column) |
| JSFX font flags `o`, `s`, `v`, `m` and their pairs, `gfx_deltablit` (16 cases: filters, scales, shears, source rectangles, offsets) and `gfx_transformblit` (2 x 2 and 3 x 3 grids, nearest and filtered), read pixel by pixel with `gfx_getpixel` | blits identical to the byte; effects as LICE draws them (a different rasterizer under them) |
| EEL2 against REAPER: number literals (20 forms), `min`/`max` with NaN in 41 contexts, subnormal results of `exp`, `pow`, `^` and the rest | same values; the same sections fail to compile |
| System exclusive and meta text events in MIDI items (`<X` chunks) read by a JSFX with `midirecv_buf`, sent on with `midisyx` (F0/F7 added when missing) and `midisend_buf`, and played into a CLAP instrument | exact (3 cases) |
| JSFX MIDI: the track's notes into a JSFX, transposed, swallowed, partly read (unread messages pass), added to, sent later in the block (held to its last frame), and through a JSFX that ignores MIDI | exact (7 cases) |
| VST 3 and CLAP pin maps on the workspace's test plugins: sidechain bus/port from channels 3/4 or 1/2, outputs to 3/4, summed inputs, sidechain off | exact (9 cases) |
| VST 3 buses: the layouts REAPER asks of the workspace's test gain and synth on 2-, 3-, 4-, 6-, 8- and 16-channel tracks (`setBusArrangements` calls logged by the plugin), the pin maps it saves for them, and a 6-channel render through the gain | same layouts and pin maps (12 cases) |
| FX presets: loading `.ini` presets with other pin maps and flag words into VST 3 and VST 2 slots, VST 2 programs, `.vstpreset` files, ReaComp's stock library (`.rpl`) and its import (`LastDefImpTime`) | same `<VST` lines and preset names (14 cases) |
| Overlapping items and fixed lanes (`FIXEDLANES 9`, `YPOS`), lane soloing on fixed-lane tracks (`FREEMODE 2` with `LANESOLO`: only the soloed lanes, such as a comp lane, play) | exact (11 cases) |

`examples/null_check.rs` re-renders any project REAPER rendered with `-renderproject` and
compares the two files.

### Not there yet

- NAM models at another rate than the project
  are converted with the 192-point sinc; the plugin's own converter is not matched (no reference). Hosted plugins are tested with the workspace's own test plugins.
- Multichannel: master channels go to the audio device's outputs in order, as many as the
  device offers; channels beyond that reach only multichannel renders.
- FX presets: VST 3 `.vstpreset` files are looked for only where REAPER 7.73 lists them
  (`~/.vst3/presets`), not in the plugin bundle or system folders. REAPER counts a CLAP
  plugin's discovered presets after its own (`TrackFX_GetPresetIndex`) and loads them by name
  (`TrackFX_SetPreset`), but not by index or by navigating, so where they sit in its preset
  menu is unverified; only plugin-internal presets were tested, not preset files.
- ReaScript covers part of the API (above); Python needs `python3` on the PATH. Calls that
  take or return chunks (`Get/SetTrackStateChunk` …), automation items, FX containers, take
  FX, hardware outputs, the MIDI editor's API and the arrange view are not there. `extension_api`
  returns 0, as REAPER's does for its own API, since Scythe has no extensions. Scripts' `gfx`
  windows do not show menus (`gfx_showmenu` returns 0) or dock. MIDI events are kept
  in time order, so `noSort` changes nothing. Effect parameters are those of running effects, set live as the inspector does; their presets come from
  REAPER's preset files only. `Track_GetPeakInfo` returns 0.
- JSFX views (`@gfx`): font 0 measures and places text as REAPER's, but its glyphs are Scythe's
  own drawing, and TrueType text comes from another rasterizer, so outlines and shadows follow
  slightly different glyphs. The blur font flag (`r`) is ignored. `gfx_ext_retina` follows
  REAPER's documented behaviour; there was no HiDPI screen to measure REAPER on.
- EEL2: REAPER reorders constants in a chain of operators (`x * 30 / 255` runs as
  `x * (30 / 255)`, `x + 0.2 - 0.3` as `x + (0.2 - 0.3)`; parentheses keep the order), which
  can change the last bit of a result; Scythe keeps the written order.
- Plugins that are not installed as Linux plugins (ReaTune, the Windows/macOS VST 3s in the
  corpus such as Amp Room) pass audio through; the inspector says so per slot.
- Stretched and pitch-shifted takes do not null against REAPER. élastique (REAPER's default) is
  proprietary and REAPER's simple windowed, Rrreeeaaa and ReaReaRea modes are its own, so they
  use Scythe's phase vocoder; SoundTouch and Rubber Band come from the system's libraries, newer
  than REAPER's (above). Shifters process the file from its start, not from the item's source
  offset, and the tempo correction in REAPER's SoundTouch wrapper (it never triggered in the
  probes) is not modelled.
- Licensing: SoundTouch (LGPL-2.1) is loaded at run time from the system's `libSoundTouchDll`
  (devbox installs it), never vendored or linked statically; without it SoundTouch takes use the
  phase vocoder. Rubber Band is GPL-2.0-or-later and only used with the `rubberband` cargo
  feature, off by default (`cargo build -p scythe-app --features rubberband`), also loaded at run
  time (`librubberband`); without it Rubber Band takes use the phase vocoder.
- Playback converts with REAPER's default playback mode (64-point sinc); REAPER's playback
  preference (`playresamplemode` in reaper.ini) has no equivalent setting. Glue and apply-FX use
  the playback media. A rate-changed take whose source offset does not land on a frame of its
  resampled copy is read between frames with linear interpolation.
- MPEG-2/2.5 intensity stereo is untested against REAPER (LAME cannot encode it). A VBRI
  header frame is assumed to play as silence like a Xing/Info frame.
- Render formats beyond WAV, MP3, FLAC, Ogg Vorbis, Opus, WavPack, CD image, DDP and the
  FFmpeg video sink (AIFF, CAF, GIF, LCF; WAV's 8-bit u-law; the WAV sink's BWF and Wave64/RF64
  options). CD image and DDP renders are always 44.1 kHz.
- Video renders: video processors other than the three presets above (text and timecode
  overlays, item fades, chroma key, transitions, blurs …, and any edited code) are left out
  with a warning: they are EEL scripts drawing with REAPER's own graphics API. The opacity
  preset blends over the lower tracks as Scythe composites them rather than re-blitting the
  next track's video by nearest neighbour, and its zoomed rectangle is a few pixels off
  REAPER's. Downscaling is the top-left bilinear scaler, which REAPER's 4x reduction follows
  only loosely; the first frame of a B-frame stream at its very start shows a later frame.
  Option keys `ffmpeg` does not know fail the render (REAPER ignores them). Video sources with item properties beyond the file
  (`AUDIO`, `FLIP`, peaks settings) keep their video but not their audio. Lossy streams are
  FFmpeg's current encoders rather than the FFmpeg 4.4 REAPER loads. DV audio is not written
  (FFmpeg has no encoder for it). REAPER's FFmpeg sink has no transparency or colour-depth
  settings to read (those are a video item's own properties).

REAPER's own plugins (`reaeq.vst.so` …) are not hosted: they only run inside REAPER and ask
the host for REAPER internals.
