# Scythe features

The full feature list, grouped by area. The [README](../README.md) has the short version; what is
missing or differs from REAPER is in [limitations.md](limitations.md), and how each part was
compared with REAPER 7.73 is in [verification.md](verification.md).

- [Arrange and editing](#arrange-and-editing)
- [Mixing and routing](#mixing-and-routing)
- [Effects and plugins](#effects-and-plugins)
- [MIDI](#midi)
- [Recording](#recording)
- [Automation](#automation)
- [Live and stage](#live-and-stage)
- [Remote control](#remote-control)
- [Scripting](#scripting)
- [Playback, rendering and formats](#playback-rendering-and-formats)
- [Project and media](#project-and-media)
- [Actions, windows and themes](#actions-windows-and-themes)
- [Command line](#command-line)

## Arrange and editing

- **Editing**: move (snapped, across tracks), trim edges, fade handles, split, copy/cut/paste,
  duplicate, reverse, delete, markers, undo/redo. Several items: Ctrl+click or drag a marquee on
  empty lane space; moving, deleting, muting, copying (with track spacing) and duplicating then
  act on all of them. Overlapping items crossfade automatically after a move or trim.
  With a time selection (drag on the ruler): split items at it (`Shift+S`), remove its contents,
  remove them and close the gap (`Ctrl+Alt+X`, markers and envelopes follow), or insert empty
  space. Takes (`T` cycles), item mute (`U`), marker
  navigation (`[` `]`), fixed lanes drawn as lanes, tracks moved with `Ctrl+↑/↓` and into or out of folders with
  `Ctrl+→/←` (on every selected track), track height with `PageUp`/`PageDown` (compact rows below 56 points). Click a
  track's colour dot for a palette; markers jump on click, rename on double-click and delete on
  right-click. Razor edits: Alt+drag (over items too) marks a time range on every track it
  crosses (Shift+Alt adds more); `Delete` removes what lies inside, copy/cut take those pieces
  and paste keeps the space before them. `Ctrl+G` glues the selected items of a track into one
  rendered file; "Apply track FX to items as new take" renders each item through its track's
  effects into a new, active take.
- **Editing tools** (REAPER's actions and project settings, `scythe_core::edit`): switches above
  the track headers for ripple editing (off, per-track, all tracks: `RIPPLE`), the snap grid
  (click toggles snap, right-click opens the grid window: 1/128 to 4 whole notes, triplets,
  swing and measure grids from the project's `GRID`, snapping to item edges and markers too),
  auto-crossfade (`AUTOXFADE`), locking (`LOCK`, with REAPER's eleven lock modes) and item
  grouping (`GROUPOVERRIDE`). Ripple follows moves, deletes, cuts, pastes and duplicates as
  REAPER's does (later items, envelope points with REAPER's edge points, and with all tracks
  markers and regions); deleting with ripple all tracks removes that time from every track.
  Item groups (`GROUP`, "Item grouping: Group items" and remove/select): clicking selects the
  group, moves and splits take the group along. Locked items (`LOCK`, "Item properties:
  Lock") do not move, split, delete or ripple, and lose their edge and fade handles. Takes:
  explode across tracks, in place or in order, implode across tracks or on one track, crop to
  the active take, delete the active take, previous/next take. Fades and crossfades: REAPER's
  seven fade types for fade-ins, fade-outs and crossfades, crossfade overlapping items or
  neighbours over the time selection, and a crossfade editor window (shape, curvature, type,
  length, both sides of a crossfade with their curves drawn); all eight shapes (`5.1` is the
  sharp S-curve, `1.1` the equal-power one) play as REAPER renders them. Dynamic split
  (tab-to-transient too) with REAPER's sensitivity and threshold, splitting at the transients
  or adding stretch markers there. The nudge/set window and REAPER's move-by-grid, "a bit"
  (one pixel) and edge grow/shrink actions, with every unit of REAPER's nudge dialog. Razor
  edits also on envelopes (Ctrl+Alt+drag, points removed keeping the envelope's shape) and the
  razor area move actions. ReaScript: `GetSetProjectGrid`, `SnapToGrid`, `ApplyNudge`,
  `C_LOCK`, `I_GROUPID`, fade shape 7, and these actions through `Main_OnCommand`.
- **Fixed lanes and comping** (REAPER 7): lanes with names, playing lanes (`LANESOLO`), "comp
  into new empty lane", and **swiping**: Ctrl+Shift+drag across a lane makes a comp area
  (`LINKEDLANE`); the new area wins its range and the areas it overlaps cross-fade into it over
  10 ms with REAPER's equal-power curve, and the comp lane's items are rebuilt from the areas as
  REAPER rebuilds them. As with REAPER's big lanes (`FIXEDLANES` &8, the default) each lane is
  one track height tall, so a track grows with its lanes; a line separates the lanes and the
  header has a button per lane with its name, lit while it plays (click plays only that lane,
  Ctrl+click adds or removes it).
- **Track selection and folders** (`scythe_core::folders`, REAPER 7.73's behaviour measured with
  Lua probes and by dragging in its track view): click selects a track, Ctrl+click toggles one,
  Shift+click selects a range; "Track: Select all tracks" (40296) and "Unselect all" (40297).
  Mute, solo, arm and polarity clicked on a selected track, and a selected strip's fader and
  pan, carry over to the other selected tracks (REAPER's selection grouping; faders by the same
  dB, pan by the same amount), as do record input and monitoring in the inspector and send
  levels to the same destination. The selection is read from the tracks' `SEL` lines when a
  project opens and written back on save, as REAPER keeps it. Drag track headers, or mixer strips by their name, to reorder:
  a line shows where they land, at its folder depth; as in REAPER the top third of a track drops
  before it, the middle third makes it a folder holding the dragged tracks, the bottom third
  drops after it at its depth (so the last track of a folder extends the folder). Several
  selected tracks move together, a folder takes its children along, every folder stays closed
  by its last track (`ISBUS` depths rewritten from nesting levels) and a drag is one undo step.
  The folder button on a track header cycles normal, folder parent and last in folder ("Track:
  Cycle track folder state", 1041); the triangle on a folder cycles open, small and collapsed
  ("Cycle folder collapsed state", 1042; `BUSCOMP` field 1, `I_FOLDERCOMPACT`), not an undo step
  as in REAPER. The mixer shows a bar per enclosing folder above each strip and a folder icon
  that hides the children (`BUSCOMP` field 2, "Show/hide children of selected folder tracks in
  mixer", 41665). "Move tracks to new folder" (42785) makes a folder from the selected tracks;
  "Move tracks up/down" (43647/43648), "Remove tracks" (40005, children included), "Mute/unmute
  tracks" (40280), "Solo/unsolo tracks" (40281), the automation mode actions, "Insert new track
  at end of track list" (40702) and "as first track" (43093) act on the selection as REAPER's do.
  The web UI shows the same folders with collapse, drags tracks by the header (mouse) or the
  grip (touch) and hides collapsed mixer children; the control API has `track.move` with a
  `parent`, `track.new_folder` and `track.collapse`.
- **Fades** with REAPER's shapes and curvature, drawn on the items.
- **Tempo map** (`TEMPOENVEX`: tempo changes, linear ramps, time-signature changes) for the
  ruler, grid, snap, metronome and MIDI; tempo markers on the ruler, `Shift+C` inserts one at the
  cursor, the bpm control edits the tempo in effect at the cursor. As in REAPER, right-click a
  tempo marker for "Edit time signature marker..." (also a double-click: tempo, time signature,
  gradual transition) and "Remove time signature marker" (also Alt+click).
- **Item timing**: take stretch markers (`SM`, with slopes) play in every stretch mode: without
  "Keep pitch" the source is read with the render's sinc at the position the markers give, the
  kernel following the local speed; with it (or a pitch change) the item is cut into pieces of
  near-constant speed run through the take's shifter and joined with short crossfades. Take pitch
  envelopes (`PITCHENV`) go through the same pieces. Take channel modes (`CHANMODE`: normal,
  reverse stereo, mono mix, left, right, any single channel or pair). Timebase: the project
  (`TIMELOCKMODE`), a track or an item (`BEAT`) follow tempo edits by time, beats (position,
  length and rate; items with stretch markers spread them instead) or beats position only, and
  tempo markers keep their beat (`TEMPOENVLOCKMODE`); markers and track envelopes move along.
  The project play rate (`PLAYRATE`, resampled or pitch-kept) speeds up playback and renders.
  Stretch markers are drawn on items: drag to move one, Shift-drag to slide the source under
  it, right-click to remove, W adds one at the cursor; the item card has channel mode and
  timebase menus, and right-clicking the tempo sets the project timebase and play rate.
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

## Mixing and routing

- **Mixing**: volume, pan with REAPER's pan laws, mute/solo, folders, sends, master, peak
  meters, master loudness (BS.1770 momentary and short-term LUFS, in the mixer header),
  REAPER's fader law (0 dB at 71.6 % of the travel, +12 dB at the top).
- **Pan modes, width and polarity** (`PANMODE`, `WIDTH`, `VOLPAN` dual pan fields, project
  `PANMODE`): stereo balance / mono pan, REAPER 3.x balance, stereo pan with width, dual pan,
  with width and dual pan envelopes (`WIDTHENV2`, `DUALPANENVL2`, `DUALPANENV2`). Right-click a
  mixer strip's pan for the mode; stereo pan adds a width bar, dual pan two knobs. Polarity
  (`IPHASE`, the strip's Ø) flips the track at its fader, as REAPER does: post-fader sends
  follow it, pre-fader and pre-FX sends do not.
- **Solo modes**: solo in place (REAPER's default: the soloed track is also heard through its
  sends and the busses they reach), solo ignoring routing, and solo defeat (`MUTESOLO` fields 2
  and 3), on the solo button's right-click. Soloing a track keeps the tracks feeding it playing
  into it, passes its signal through its folder parents without their own material, and leaves
  everything else silent (`scythe_core::solo`). Monitor mixes get solo defeat, so soloing in the
  studio never takes a musician's in-ear mix away.
- **Track groups** (`GROUP_FLAGS`, `GROUP_FLAGS_HIGH`, `GROUP_NAME`): 64 groups with lead and
  follow per parameter (volume, pan, width, mute, solo, record arm, polarity, automation mode,
  media edits), reverse flags and "no lead when following". A lead's edits carry over as REAPER
  carries them: volume as a dB offset (capped at +12 dB), pan and width as an offset, switches
  as the same state, through leads of other groups. VCA leads scale their followers instead
  (fader, volume, trim and mute envelopes multiply; pan and pan envelopes offset; width
  multiplies), nested VCAs included, and pre-FX followers take the level at their input. The
  grouping matrix ("Track: Show track grouping matrix", or a group badge on a track header or
  mixer strip) names groups and ticks memberships; the control API has `group.*`.
- **Send modes and hardware outputs**: sends tap post-fader, pre-FX or pre-fader (post-FX),
  with polarity and mono downmix (`AUXRECV` fields 2, 6 and 7). Track and master audio
  hardware outputs (`HWOUT`, `MASTERHWOUT`: device output with the mono flag, send mode,
  volume, pan, mute, polarity, source channels, pan law) play into the device's outputs, with
  the same pan law and mono averaging as sends, line up with the master through plugin delay
  compensation, and follow the track's own output (muting silences them; a track that is only
  heard because it feeds a soloed bus keeps its sends but not its main or hardware outputs, as
  in REAPER). The device opens with as many channels as the outputs reach. Edited in the
  inspector's Sends tab, whose output menu lists every output the device opened, by name
  (see [Audio device](#audio-device)).
- **Multichannel tracks** (`NCHAN`, up to 32 in the inspector's Sends tab): items with more
  channels play into channels 3 and up, sends take any channel pair (or one channel) to any
  pair of the destination (`AUXRECV` source and destination channels), folders carry every
  channel to their parent, a track can feed its parent from a channel offset (`MAINSEND`),
  and ReaGate and ReaComp listen to the channels their detector input names (channels 3/4 for a
  sidechain, stored as REAPER's UI stores it).
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

## Effects and plugins

- **FX chain graph**: per-slot wet/dry (`WET`, with delta solo and wet/dry envelopes, ramped a
  block late as REAPER does unless the slot is sample-accurate), offline slots (unloaded,
  passing audio), effects running in parallel with the one before (`PARALLEL`, outputs summed,
  MIDI merged when asked) and FX containers (`<CONTAINER`) with their own channel count, input
  and output pin routing, parallel or serial children, nesting and container parameters that
  drive a child's; latency compensated through all of it. Take FX (`<TAKEFX`) process their
  take after its volume and fades on the take's channel count (`TAKEFX_NCH`), with two seconds
  of tail past the item; input FX (`<FXCHAIN_REC`) process a monitored track's live input (they run, for their
  parameters, on tracks that are not monitored too); the
  monitoring chain processes only what the device plays and is kept in `reaper-hwoutfx.ini`
  beside the preferences. Freeze (actions 41223/41644, `track.freeze`) renders a track through
  its effects to `reaper_freeze_<name>.wav` and keeps the effects and items in a `<FREEZE`
  level, stacking and unfreezing as REAPER does. The inspector has wet, delta, parallel, offline
  and container controls in each slot's menu, the container's effects under it, and adds to the
  track, input or monitoring chain.
- **FX**: chains on tracks and master, rendered in parallel across tracks. FX chains save to and
  load from REAPER `.RfxChain` files, and tracks to and from `.RTrackTemplate` files (sends
  between the saved tracks kept), starting in REAPER's own resource folders. FX presets are
  REAPER's own per-plugin preset files (`presets/vst-<plugin>.ini`, `vst3-…`, `js-…`, `clap-…`,
  `lv2-…` in REAPER's resource folder): the slot's context menu lists the presets saved in REAPER,
  loads one into the plugin, saves the plugin's current settings under a name so REAPER lists it
  too, and renames and deletes them. Like REAPER it first imports a plugin's default library (the
  `.rpl` beside a VST 2 binary, such as REAPER's stock ReaComp presets, or beside a JSFX) when the
  library changed since `LastDefImpTime`, and preset libraries (`.RPL`) import into and export
  from the preset file. Factory presets follow: a VST 3 plugin's `.vstpreset` files under
  `~/.vst3/presets/<vendor>/<plugin>/`, a VST 2 plugin's own programs (REAPER restores the chunk,
  then selects the stored program) and the presets a CLAP plugin's preset-discovery provider
  lists, loaded through its preset-load extension; like REAPER, only the provider whose id is the
  plugin's id is asked. Loading a preset does what REAPER does with the `<VST` header: the
  preset's pin maps and flag bits replace the slot's, and its state is taken only when its state
  kind matches. Presets saved as `.RfxChain` files by earlier versions are still listed.
- **Plugin scanning**: plugins are scanned on a background thread that starts with Scythe, so
  the window, the session and the control API never wait for it; what earlier runs found is
  listed at once from the scan cache (`$XDG_CACHE_HOME/scythe/{clap,lv2,vst3}.json`, per binary or
  LV2 bundle by path, size, modification time and resolved path, so a Nix store update counts as
  a change), and only new or changed plugins are loaded. CLAP and VST 3 binaries are loaded in a
  child process (`scythe --scan-plugin …`) with a 30 s limit: a plugin that hangs or crashes
  while it is scanned is marked as failed in the cache, like REAPER marks it, and is tried again
  when it changes or on a full rescan. LV2 bundles are read from their Turtle files (LSP Plugins'
  195-file bundle in 0.7 s). The FX browser shows "Scanning plugins… n of m" while a scan runs,
  and `fx.catalog` answers straight away with `scanning` and `progress`, `failed`, and `rescan`.
  `SCYTHE_SYSTEM_PLUGINS=0` limits the scan to `$CLAP_PATH`, `$LV2_PATH`, `$VST3_PATH` and
  `$SCYTHE_VST_PATH` (the tests use it).
- **Stock effect emulations**: ReaEQ, ReaVerb and ReaSamplOmatic5000 and ReaDelay settings stored
  in projects play through built-in equivalents (the sampler receives the track's MIDI, live input
  and MIDI an earlier effect sends; musical delay lengths follow the tempo map), as do the
  dynamics: ReaComp (detector input, RMS, detector low/high-pass and filter preview, pre-comp
  lookahead, soft and legacy knee, auto make-up, auto release, classic attack, wet/dry), ReaGate
  (detector filters, RMS, hysteresis, hold, pre-open, noise, wet/dry, both invert modes, MIDI note
  out), ReaLimit (10 ms lookahead, its two-stage release) and ReaXcomp (any number of bands,
  feedback detector, band gains and bypass, output gain), all latency compensated and modelled on
  REAPER 7.73 renders (`scythe-engine/src/stock/`, one file per plugin, dispatched from a table of
  REAPER's VST identities). ReaVerbate (Freeverb with REAPER's ten combs and six allpasses per
  channel, pre-delay, width and wet filters), ReaSynth (its naive oscillators and mix laws,
  envelope, pitch wheel, REAPER's -80 dB voice gating and 512-frame block behaviour) and
  ReaControlMIDI (transpose, snap to a scale, and the bank/program, volume, pan and pitch-wheel
  sends at playback start) are emulated the same way and offered as the built-ins "Algorithmic
  reverb", "Synth" and "MIDI control", and so are ReaFIR's EQ mode (REAPER's curve drawing:
  straight lines in Hz between three or more points, a Bézier in log frequency between two, as a
  linear-phase FIR of the FFT size; built-in "FFT EQ"), ReaVerb's echo generator and reverse
  modules, and ReaInsert ("Hardware insert": sends to two device outputs, plays two device inputs
  back, its delay compensation reported as latency and measured by a ping through the cable;
  silent in offline renders as in REAPER without a device). Settings an emulation does not cover
  (ReaFIR's dynamics modes, other ReaVerb modules) run in REAPER's own binary when it is on the
  VST path.
- **Built-ins**: The same effects are offered as built-ins in the plugin browser (equalizer, gate,
  compressor, delay, convolution reverb, limiter, multiband compressor) with editable parameters
  (the inspector's parameter rows, `fx.params` and `fx.set_param`), and are saved in REAPER's own
  format, so REAPER opens them as ReaEQ, ReaGate, ReaComp, ReaDelay, ReaVerb, ReaLimit and
  ReaXcomp.
- **Stock effect editors**: The stock effects (ReaEQ, ReaComp, ReaXcomp, ReaGate, ReaLimit,
  ReaDelay, ReaFIR, ReaVerbate, ReaVerb, ReaSynth, ReaInsert, ReaControlMIDI) get **Edit** too,
  opening a graphical editor in a resizable window (for a track's or the master's effects, input
  FX, monitoring FX, the active take's FX in the item card and effects inside containers; also
  from ReaScript's `TrackFX_Show`), drawn from the emulation's own math: ReaEQ's
  response per band and in all with draggable handles (drag for frequency and gain, scroll or
  Shift-drag for the width, double-click to switch a band, right-click for its type, add and
  remove bands) over a live spectrum before and after; transfer curves with threshold and ratio
  handles, the detector's level on the curve, gain reduction meters and history for ReaComp and
  ReaLimit; ReaXcomp's crossovers on the spectrum with each band's response, transfer curve and
  meter; ReaGate's detector level with its opening and closing thresholds and when it stood open;
  ReaDelay's taps and their echoes on a time line with a row of values per tap; ReaFIR's curve
  drawn with points over the spectrum; ReaVerbate's decay by frequency; ReaVerb's impulse
  response; ReaSynth's oscillator and envelope; ReaInsert's routing (the device's channel pairs by name) and
  ping; ReaXcomp's bands split and removed; ReaControlMIDI's scale clicked key by key. Every control drags,
  scrolls, takes the arrow keys once clicked, types a value on double-click and offers
  Modulate/Learn on right-click; each drag (and each drag on the parameter rows, which stay a tab
  away) is one undo step. The meters and spectra come from a probe the effect fills only while its
  editor is open: atomics and two sample rings, nothing locked or allocated on the audio thread.
- **Plugin hosting**: CLAP plugins are hosted, instruments included (scan `~/.clap`,
  `/usr/lib/clap`, `/etc/profiles/per-user/$USER/lib/clap`, `$CLAP_PATH`), as are LV2 plugins (`$LV2_PATH`, `~/.lv2`, `/usr/lib/lv2` …;
  controls and state in REAPER's `<LV2` layout), VST 3 plugins (`$VST3_PATH`, `~/.vst3`,
  `/usr/lib/vst3` …; state in REAPER's VST 3 chunk layout) and Linux VST 2 plugins from
  `$SCYTHE_VST_PATH`/`$VST_PATH`/`~/.vst`. Add them from the inspector, click a slot for its
  parameters (input FX, monitoring FX and effects inside containers too), and use **Edit** for the
  plugin's own editor (CLAP, VST 2, VST 3 and LV2 X11 UIs, in a separate X11 window). Plugin
  latency is compensated across tracks, sends and folders.
- **JSFX**: JSFX run from their own source through an EEL2 interpreter (sections, sliders,
  imports, the `<? ?>` preprocessor, user functions with locals and instances, memory, strings,
  MIDI, FFT and MDCT in REAPER's bin order and scale, files): every JSFX in the corpus and all 219
  of REAPER's stock effects. Arithmetic follows REAPER's compiled EEL2 on x86-64: an assignment
  stores 0 for NaN, infinities and the smallest values unless its value comes from an operation
  that cannot make them out of clean inputs (`+ 1`, `cos`, `floor`, integer operators, ... are
  clean; `* 2`, `abs`, `min`, negation pass their inputs on; `/`, `*` by a variable, `log`, `exp`,
  `tan` are checked); constant expressions are folded at load (non-finite results become 0, `1 <<
  40` is a 32-bit shift) and `x + 0`, `x * 1`, `x * 0` are simplified; arithmetic flushes
  subnormal results to zero; comparisons with NaN take x86's unordered flags (`<`, `<=`, `==`
  true); `min`/`max` give their first argument for NaN, except that `max` gives its second where
  REAPER compiles it for a pointer result (an argument of `memset`, `sprintf`, `gfx_*` and the
  other builtins taking pointers, or a branch of a `?:` that is assigned or passed on); the
  library functions (`exp`, `pow`, `sin`, ...) flush subnormal results too; integer conversions
  give the most negative integer for NaN and out-of-range values; `<<`/`>>` shift the low 32 bits;
  `invsqrt` is the bit-trick estimate; `loop()` evaluates to 1. EEL2 has no exponents: a number
  run into letters (`1e5`, `1.5e+3`, `2x`) fails to compile, and a section that does not compile
  is left out while the others run, as REAPER does. File sliders list their folder under `Data`
  and are stored by file name as REAPER does; `@serialize` state is read from and written to
  `<JS_SER` (32-bit floats), and `@serialize` runs when REAPER runs it: on load after `@init` and
  `@slider` (writing), again after the slot's slider values (`@slider`), reading the stored state
  and writing once more, and once per save; an effect without `@serialize` is initialised once for
  its first playback; `file_open` reads WAV files as samples and text one value per item. Effects
  with `@gfx` (analyzers, scopes, the channel mapper) open their own view from the Edit button:
  `@gfx` runs about 30 times a second on a thread of its own beside the audio, as REAPER runs it,
  sharing the effect's variables, memory, strings and images (each value is an atomic, so the two
  race a value at a time as REAPER's do), with the mouse (buttons, modifiers, wheel), typed keys
  and files dropped onto the view (`gfx_getdropfile`). It draws into pixels as REAPER does:
  rectangles, lines, circles, arcs, triangles, gradients and pixels blend with `gfx_a` and
  `gfx_mode` (additive too), into the screen or any of 1024 images (`gfx_dest`, `gfx_setimgdim`),
  colours taken to a byte as REAPER truncates them; font 0, and a font before `gfx_setfont` names
  a face, is Scythe's own 8-pixel bitmap font with REAPER's measures; other text is drawn in the
  face `gfx_setfont` names (found through fontconfig; bold, italic, underline, turned, and
  outlined, shadowed, inverted and one-colour as LICE draws them) and measures as it draws. Text
  in a box (`gfx_drawstr` with flags) is placed and clipped as REAPER places it, font 0 drawing
  into a window on the box and leaving `gfx_x`/`gfx_y` measured from that window, other fonts
  aligning each line and leaving them after the text. `gfx_blit` and `gfx_blitext` scale, turn and
  filter; `gfx_deltablit` and `gfx_transformblit` step through the source in LICE's fixed point
  with its filter; `gfx_loadimg` reads PNG and JPEG files from the effect's folder or `Data`;
  `gfx_getpixel`, `gfx_blurto` and `gfx_muladdrect` work on the pixels; `gfx_showmenu` waits
  inside the script for a choice from a menu at the mouse while the audio goes on; `gfx_clear =
  -1` keeps what was drawn; a script that sets `gfx_ext_retina` gets the screen's pixels per point
  there and draws at the screen's own pixels; and sliders it sets take effect as if moved by hand.
  The view uploads the screen only when it changed. They run in REAPER's 512-frame blocks, play
  the track's MIDI and pass what they send on to the next effect in the chain; they are read from
  `~/.config/REAPER/Effects` and the installed REAPER's `InstallData/Effects` (or
  `$SCYTHE_JSFX_PATH`).
- **Scythe EQ** (Scythe's own, not in REAPER): a 24-band dynamic equalizer, offered in the FX
  browser as "Scythe EQ". It targets the feature set of today's commercial dynamic EQs, built
  from public descriptions and DSP literature in Scythe's own design.
  - *Bands*: bell, low and high shelf, low and high cut, notch, band pass, tilt shelf (−g/2 below,
    +g/2 above the frequency), flat tilt (a constant dB-per-octave tilt across 20 Hz–20 kHz,
    pivoting on the frequency; first-order shelves an octave apart, within 0.5 dB of a straight
    line) and all pass. Cuts slope 6–96 dB/oct (Butterworth) or brickwall; shelves and band pass
    take slopes too (shelves as Butterworth shelving filters after Holters and Zölzer). Gain ±30
    dB, Q 0.025–40; Q above 0.707 gives cuts and shelves a resonance. Gain-Q interaction narrows
    bells by the square root of their linear gain.
  - *Zero latency* runs every band as matched biquads: poles mapped from the analog prototype by
    impulse invariance, the numerator set so the digital magnitude equals the analog one at DC,
    Nyquist and the section's own frequency (Vicanek's matched second-order filters applied to
    every section; cuts are matched as the inverse of the boost they mirror, which keeps narrow
    cuts near Nyquist exact). The response follows the analog prototype instead of cramping
    towards Nyquist: a 10 kHz bell at 44.1 kHz stays within 0.8 dB of the analog curve to
    20 kHz, a 1 kHz bell within 0.05 dB, at 96 kHz and up within a few tenths everywhere (the
    unit tests measure this per shape and rate). Parameter changes glide (15 ms) and the filters
    are recomputed every 32 frames while they do; switching a band's shape or bypassing it
    crossfades over 10 ms; filter memories are flushed below 1e-30 so silence never decays into
    subnormals.
  - *Natural phase* samples the analog prototypes' magnitude and phase on an 8192-point grid
    (at 48 kHz; doubled per doubling of the rate) and runs it as an FIR, so the phase follows the
    analog filter with no cramping, for 1024 frames of latency. *Linear phase* uses the magnitude
    only, as a symmetric FIR of 2048 to 32768 points (resolution low to maximum: 1024–16384
    frames of latency at 48 kHz) with an ideal brickwall. Both run as a partitioned overlap-save
    convolution with a 2×2 kernel matrix for mid/side and single-channel bands, crossfade when
    the curve changes, and report their latency so the chain compensates for it. Changing the
    phase mode, the resolution or whether any band is spectral while playing keeps the old
    processing running until the new one has filled its latency, then crossfades over 30 ms
    (equal power, each at its own timing: the chain moves its compensation to the new latency
    when it is rebuilt right after), so the switch does not click.
  - *Stereo placement* per band: stereo, left, right, mid or side.
  - *Dynamic bands* (bells, shelves and tilts): the band moves from its gain towards gain +
    range while its detector (the band's region filtered out of the band's input, or out of the
    track's channels 3/4 as an external sidechain) is above the threshold, by half the overshoot
    with a 6 dB soft knee, never past the range; attack and release per band; auto threshold
    follows the detector's own average level (1.5 s). In the FIR modes a dynamic band adds a
    correction kernel scaled by its current amount, so the response stays linear (or natural)
    phase while it moves. The sidechain can be auditioned. In zero latency cuts and notches can
    be dynamic too: a cut's frequency moves an octave per 12 dB of range (into the pass band
    for a negative range) while what it removes is loud, and a dynamic notch is transparent at
    rest and as deep as its range when its region is loud (a bell of that depth).
  - *Spectral dynamics*: a spectral band works per frequency bin inside its region (an STFT of
    2048 at 48 kHz, 75 % overlap, after the EQ), so a resonance that pokes out is pulled down
    while its neighbours keep their level; same threshold, range, attack, release and auto
    threshold (per bin) as a dynamic band. It adds 2048 frames of latency while any band uses
    it.
  - *Character*: clean; subtle, `2·tanh(x/2)` (odd harmonics only, under 0.1 % at −12 dBFS);
    warm, `(tanh(x + 0.15) − tanh(0.15)) / (1 − tanh²(0.15))`, whose bias adds even harmonics,
    with a 5 Hz high pass removing its DC. Both have unity small-signal gain.
  - *Output*: gain, auto gain (pink noise through the curve kept at the same A-weighted level:
    −10·log10 of the A-weighted mean of |H|² over 20 Hz–20 kHz), gain scale −100 to 200 % of
    every band's gain and range, output pan (balance) and width.
  - *Editor* (Edit on the slot): the bands' curves and fills, the summed curve, each dynamic
    band's range (dashed) and its live movement over a real-time analyzer, a piano strip under
    the frequency axis (10 Hz–30 kHz) and a readout of frequency, note with cents and level under
    the pointer. Double-click adds a band (a cut near either end), double-click a handle bypasses
    it; drag handles (Shift: fine), drag empty space to select several, Ctrl-click to add to the
    selection; the wheel sets Q; Alt held on a band solos it; right-click for shape, slope,
    stereo placement, dynamics, bypass, solo and remove; arrows nudge the selection (an octave
    twelfth or half a dB, Shift: finer) and Delete removes it. Below the graph: the selected
    band's controls, and the phase mode, resolution, latency, character, Gain-Q, display range,
    auto gain, gain scale, output, pan and width.
  - *Analyzer*: pre (outline), post (filled, fading towards the bottom), the sidechain (in its
    own colour when a band uses it) and another Scythe EQ's signal, on a thread of its own from
    the effect's probe rings; resolution low to maximum (FFTs of 2048 to 16384 samples, the last
    zero padded twice), speed very slow to very fast (separate rise time and fall rate), peak
    hold that falls after 1.5 s, freeze, tilt 0–6 dB/oct (4.5 by default) and a 60, 90 or 120 dB
    range. Bins are resampled onto a log grid: interpolated where a grid point's fractional-octave
    window holds less than two bins (a smooth bass, no steps), elsewhere averaged over the window
    and pulled towards the window's peak by how tonal it is (its spectral flatness), so noise is
    averaged while a sine keeps its level. The display eases between analyzer updates for a
    fluid 60 fps. *Spectrum grab*: a peak near the pointer standing 6 dB over its surroundings is
    highlighted; dragging it makes a bell there with the gain following the pointer.
  - *Compare and collisions*: "Compare" shows any track's or the master's output (after its
    effects, before its fader, tapped by the engine only while someone reads it) or another
    Scythe EQ's; where both signals are loud and within 6 dB of each other the frequencies are
    shaded as collisions.
  - *EQ Match*: learns this instance's input against a track's, the master's or another Scythe
    EQ's signal (both averaged while playing) or against an audio file (measured offline), levels the difference (its mean
    over 100 Hz–10 kHz removed), smooths it (0.05–2 octaves), scales it by the amount and fits up
    to 16 bands (shelves where the difference reaches an end of the spectrum, bells elsewhere,
    gains refined together); Apply adds them to free bands as one undo step.
  - *Instance list* ("Instances"): every Scythe EQ in the project with its curve and where it
    sits (track, input and take chains, the master, monitoring, containers: "Vocals · input",
    "Guitar · Rig › 3"); open or bypass each, click a band to bypass it, drag it up and down for its gain.
  - *Storage*: a CLAP slot, `<CLAP "CLAP: Scythe EQ (Scythe)" scythe.eq "">` with its settings
    in `<STATE` (magic `SCEQ`, a version, then every value as a double in its own units).
    REAPER 7.73 opens such a project with "the following effects … are not available"; the slot
    shows offline, passes audio, and REAPER saves the block byte for byte, envelopes on its
    parameters too (it adds a label to their `PARMENV` line). Scythe opens that file again with
    the EQ intact.
  - *Control API and MCP*: every band and global value is a parameter (`fx.params`, `fx.param`,
    automation, modulation and MIDI/OSC learn; solo and audition are momentary and not stored);
    `eq.instances` (every chain and container), `eq.get`, `eq.band` (add, change or remove a band by number with named
    shapes) `eq.set` (globals) address it directly; `eq.compare` gives this EQ's and a track's (the
    master's, another instance's) third-octave levels with the collisions, and `eq.match` fits
    bands towards a track, an instance or a file (proposed, or applied as one undo step). The web UI's inspector lists the bands
    with shape, placement, frequency, gain and Q.
  - *CPU* (one instance, 48 kHz, 256-frame blocks, eight bands, release build on the test
    machine): zero latency 31 µs per block (0.6 % of one core), natural phase 17 µs (0.3 %),
    linear phase at medium resolution 14 µs (0.3 %); with two dynamic bands and one spectral band
    65, 77 and 74 µs (1.2–1.4 %). `examples/scythe_eq_bench.rs` measures it. With the editor
    open the analyzer thread takes about 3.5 % of a core and drawing the editor about 1 ms per
    frame (`SCYTHE_EQ_PROFILE=1` prints both).
- **Neural Amp Modeler**: Neural Amp Modeler slots (the LV2 plugin) run their `.nam` WaveNet
  models natively, with the stored input and output levels (all 310 models in
  `~/music/Samples/NAM` load; a standard model runs at about 25× realtime on one core of a
  2023 laptop CPU, with AVX2/FMA where the CPU has them), at the project's sample rate whatever
  rate the model was trained at, as the plugin (0.1.4) does in REAPER: no conversion, so no
  latency (measured in REAPER 7.73: a 48 kHz model adds 13.0 frames at 48 and at 44.1 kHz, the
  model's own delay, and Scythe the same; renders at both rates match REAPER's to the same
  residual). Like the plugin, a model that states its loudness (`metadata.loudness`) is
  brought to -18 dB: its output is turned by -18 minus that loudness in dB, before the slot's
  output level; one without plays as it is. This holds for the LV2 slot, the NAM stage of a
  guitar rig and the NAM panel's and TONE3000 previews alike (REAPER 7.73 with the plugin 0.1.4,
  six models from -8.9 to -39.6 dB and one without, at 44.1 and 48 kHz, input and output levels
  0 and +6/-3 dB: output level within 0.09 dB of REAPER's, 0.15 dB for the model without).
  Mixes made in Scythe before this change hear each such NAM slot (-18 - loudness) dB louder or
  quieter than before: 6.9 dB quieter for a model at -11.1 dB, 21.6 dB louder for one at
  -39.6 dB. The NAM slot's **Edit** opens the NAM panel: the model's name,
  gear, author, architecture and rate, input and output knobs, previous/next model
  (Alt+Left/Right), four snapshots per slot to A/B models and levels (click to switch, Shift+click
  to store, Alt+1…4), and a two-pane browser (list beside the picked entry, stacked when the panel
  is narrow) with Library, Recent and TONE3000 tabs. A TONE3000 tone shows its models first;
  **Preview** fetches one into a cache outside the library and plays it in the slot (Left/Right
  step through the tone's models, Up/Down through the list, A/B against the model from before),
  then **Keep** moves it into the library or **Revert** puts the old model back, the whole try-out
  one undo step. The library indexes folders you add (read only, e.g. `~/music/Samples/NAM`) and
  models downloaded from TONE3000 (`~/.local/share/scythe/nam`), with search, favourites and tags;
  impulse responses load into a convolution slot after the NAM slot. Switching models is
  click-free: the new model is read off the audio thread, warms up beside the old one and the two
  crossfade over 12 ms. Signing in to TONE3000 (OAuth with PKCE in the system browser, tokens in a
  0600 file) needs a publishable key you register yourself; see
  [docs/tone3000.md](tone3000.md). The model path stays in the LV2 plugin's own state, so
  REAPER keeps opening the slot; snapshots go into the track's `<EXT` extension state, which
  REAPER keeps. `.nam` files dropped on a track load into its NAM slot (or a new one).
- **Guitar rigs** put the whole tone in one slot: a REAPER FX container named "Rig: <amp> + <cab>"
  holding a gate (ReaGate), the NAM amp, the cab (ReaVerb with the impulse response, and a second
  one in parallel to blend), an EQ (ReaEQ: low cut, bass, mid, treble, high cut) and the output
  level, so REAPER opens and plays it unchanged. The effects list shows it as one slot whose
  **Edit** opens the rig window: the signal chain from left to right with a switch per stage, the
  picked stage's knobs under it (the gate's threshold, attack, release and hysteresis; the EQ's
  cuts, tones and their frequencies), the model browser for the amp and the impulse response browser
  for the cab (library IRs and TONE3000 IR tones, with the same Preview, A/B, Keep and Revert as
  models). Captures that include a cabinet (TONE3000's amp-cab and full-rig tones, or the `.nam`
  file's `gear_type`) switch the cab off and amp-only ones switch it on, which the cab shows;
  switching it by hand overrides that. Snapshots A-D (Alt+1…4, and `_SCYTHE_RIG_SNAPSHOT_A`…`D`
  for footswitches and MIDI learn) and named presets (`~/.local/share/scythe/rigs`, loadable on
  any track, `_SCYTHE_RIG_PRESET_NEXT`/`PREVIOUS`) hold the whole rig; the control API and MCP
  have `rig.*`, with `rig.preview` (an amp model or cab impulse response tried as one undo step),
  `rig.keep` and `rig.revert`, and `nam.preview`/`nam.keep`/`nam.revert` for a bare NAM slot.
  **Make rig** turns a NAM slot and the ReaVerb after it into a rig; a bare NAM slot
  works as before. Switching is click-free: models crossfade, impulse responses crossfade over 20
  ms, a stage switched on or off crossfades over 10 ms, and gate, EQ and level changes reach the
  running effects in place.
- **Parameter modulation and learn** (`<PROGRAMENV`, `PARMLEARN`, `LFOLEARN`): baseline, LFO
  (six shapes, Hz or tempo-synced periods, strength, direction, phase, follow-the-position or
  free-running), audio control signal from any input channel pair (sidechain on 3/4) with
  attack, release, dB range and curve, and links from another effect's parameter (earlier or
  later in the chain, offset and scale) or from MIDI CC/pitch bend on the effect's input, all
  per 512-frame block in REAPER's update order; a parameter envelope stands in for the baseline.
  MIDI learn from the inputs enabled for control (`midiins_cs`) and OSC learn from surfaces
  that allow it: absolute, relative 1-3, toggle, soft takeover, "only when the track is
  selected", LFO retrigger. MIDI shortcuts to actions (pedals to setlist next/previous, mutes,
  markers) are kept in `reaper-kb.ini` as REAPER keeps them. The `~` button on an inspector
  parameter opens the modulation window with its Learn button; the settings window learns
  shortcuts. API: `fx.modulation.*`, `fx.learn.*`, `action.learn.*`.

## MIDI

- **MIDI editor**: double-click a MIDI item (or `E`) for the docked editor of its active take, in
  three modes: piano roll, named notes (a drum map of the track's note names, REAPER's
  `MIDINOTENAMES`; `.txt` note name files load and save) and the event list (every note,
  controller, sysex and text event with its fields editable). Click adds a note on the grid
  (snapped into the scale with key snap); as REAPER's defaults, a left drag on empty grid draws a
  note (drag to extend or change pitch, Shift ignores snap) and a right drag selects with a
  marquee (Shift adds, Ctrl toggles); drag moves the selection, drag a right edge resizes, right-click deletes.
  The other left-drag modifiers are REAPER's defaults: on a note Shift moves ignoring snap, Ctrl
  copies, Ctrl+Shift moves on one axis, Alt edits the velocity, Ctrl+Shift+Alt moves vertically
  ignoring the scale; on empty grid Ctrl copies the selected notes by the drag, Alt erases the
  notes it passes and Ctrl+Alt (Shift+Alt ignoring snap) paints a line of notes. A looped item's
  passes show after the first and edit the same notes;
  double-click opens a note's properties (start, length, key, velocity, off velocity, channel,
  mute). Below the roll sit any number of controller lanes (velocity, off velocity, CC 0-127,
  14-bit pairs, pitch bend, program, channel and poly aftertouch, sysex, text): the draw tool
  paints values on the grid, the line tool draws a line or curve as REAPER 7 does (two events
  joined by a CC shape: linear, slow start/end, fast start, fast end, bezier with tension,
  square), right-click a value for its shape. Commands: quantize (grid, swing grid, strength,
  position / position and end / end / length, a window around the grid line), humanize,
  transpose (by scale steps with key snap), legato (also keeping the notes' relative spacing,
  40766), split on the grid, join, mute, force into
  key, channel for new events and a channel filter, step input (notes played on a MIDI input,
  the virtual keyboard or the editor's keys go in at the step cursor), groove templates
  (`.rgt`, applied with a strength; notes save as one). Other selected MIDI items show behind
  the edited one, at their takes' play rate and in every pass of their loops, and a click on one of their notes edits that item. Alt+double-click opens an
  item's inline editor in the arrange view, as do "Item: Open item inline editors" (40847) for
  the selected items, and 41887 closes them. The editor's settings are stored in the take as
  REAPER stores them (`CFGEDIT` mode, channel, grid and swing; `KEYSNAP`; `EVTFILTER`; one
  `VELLANE` per lane), and key snap's root and scale are the project's key signature, saved in
  its `<KEYSIG` as REAPER saves it (`midi.key`); shaped controllers play as REAPER plays them (steps every 1/`CCINTERP`
  quarter note). The MIDI editor section's actions (32060) run by their REAPER ids through
  `midi.action`. Edits are written back into the item's `<SOURCE MIDI` (an edited item leaves
  its pool). Controllers, pitch bend,
  program changes and aftertouch reach CLAP and LV2 instruments as MIDI and VST 3 instruments
  through their MIDI mapping.
- **MIDI files and input**: `.mid` files import (dialog or drag-and-drop) as in-project MIDI
  items, and "Item: Export MIDI item to file" writes one back out. A track whose record input
  is MIDI plays its instrument live while armed with monitoring on, and records an in-project
  MIDI item; recording around a loop layers the passes into one take, as REAPER's MIDI
  overdub. The input is one device or all of them (and one channel or all), numbered as REAPER
  numbers them: devices keep their number in `reaper-midihw-linux.ini` beside the preferences
  while unplugged, the `REC` line stores them in REAPER's older layout (all inputs as 31, the
  virtual keyboard as 30, ports from 30 up two higher), so a project listens only to its
  keyboard or pad. A track can also send its MIDI (items, monitored input and what it receives,
  after its effects, optionally forced to one channel) to a hardware output (Sends tab), timed
  to sound with the audio; stopping sends all-notes-off. VST 2 instruments receive MIDI too.
- **MIDI routing**: sends carry MIDI as REAPER's `AUXRECV` MIDI field (`I_MIDIFLAGS`) says:
  all or one source channel, moved to a destination channel, from one MIDI bus or all, onto
  another bus; audio and MIDI or MIDI only. Post-fader and post-FX sends carry what leaves the
  source's effects, pre-FX sends what enters them; a muted send or source sends none, and
  routing works between any two tracks in the same block whatever their order; solo routing gates MIDI sends as it gates audio. MIDI buses
  1-16 travel through every chain: JSFX with `ext_midi_bus` read and write them (`midi_bus`),
  other effects and instruments hear the normal bus only and pass the others on, as in REAPER.
  The Sends tab's MIDI pill switches a send between no MIDI, audio and MIDI, and MIDI only
  (right-click: destination channel).
- **MIDI devices, clock and timecode**: "View: MIDI devices" enables inputs and outputs (REAPER's
  `midiins`/`midiouts` masks), shows aliases (`ia`/`oa` in the device cache) and picks the
  outputs that get MIDI clock and song position (`midiouts_clock`, `midiouts_clock_nospp`),
  sent as REAPER sends them: ticks on the 1/24 quarter-note grid while playing, Start from the
  project start and Continue elsewhere, Stop and a song position pointer when stopping or
  moving, and its reset (all sound and notes off, pitch bend centred, sustain off) at play
  unless `midiouts_noreset`. Control surfaces open their MIDI ports by the same device numbers. The timecode generator item (`<SOURCE LTC`, "Insert timecode
  generator") plays LTC audio at -10 dBFS or, with `SEND 2`, MTC to its track's MIDI output (a
  full frame where playback starts or jumps, then quarter frames). External timecode
  synchronization (the project's `SMPTESYNC`, "Toggle external timecode synchronization")
  chases MTC from one input or all, LTC on an audio input, or MIDI clock with song position:
  the transport starts where the timecode is (minus the offset), jumps back in line past the
  resync threshold and stops when the timecode has been gone for the freewheel time.
- **Virtual MIDI keyboard** ("View: Show virtual MIDI keyboard", `Alt+B`): three octaves to
  click, with octave, channel and velocity; it is device 62, so tracks armed with it as their
  input play and record it, and `midi.keyboard` plays it from the control API.

## Recording

- **Metronome** (`C`; right-click the metronome button for count-in, pre-roll, level and
  output), **count-in** before playback and/or recording (REAPER's metronome settings, saved in
  the project's `<METRONOME` chunk) with the playhead waiting at the start, **pre-roll** and
  "start count-in at start of measure" (REAPER's reaper.ini preferences, kept in
  `$XDG_CONFIG_HOME/scythe/preferences.ini` with the default metronome output), the click routed to a
  chosen device output (REAPER's `METRONOMEOUT`) or into monitor mixes at each musician's own
  level, **recording** of armed tracks (`Ctrl+R`, mono or stereo inputs per the
  track's REAPER input setting) and **input monitoring**: an armed track with monitoring on
  hears its input through its FX chain, also while stopped (play guitar through a NAM amp and
  a cab IR live). Inspector: `In 1` picks the input, `Mon` cycles monitoring off, on and auto
  (tape style: heard while stopped or recording, not during playback); right-click it or the
  arm button for the **record mode menu**: REAPER's per-track record modes (input, input forced
  to mono, stereo or multichannel, output stereo/mono/multichannel with or without latency
  compensation tapped post-fader, pre-FX or post-FX, output (MIDI) recording what the track's
  effects put out, MIDI overdub, replace, touch-replace (with REAPER's controller timeout) and
  latch-replace into the MIDI item under the playhead, disable to only monitor), "monitor
  track media when recording" (otherwise a recording track's own items fall silent, as in
  REAPER), automatic record-arm on selection, the project record mode (normal, time selection
  auto-punch, selected item auto-punch) and the lane options. Recordings are laid out as REAPER
  7.73 lays them out: files named `01-Track-yymmdd_hhmm.wav` under the project's record path,
  recording over items splits them at the recording's edges and adds the new take (with empty
  takes where a pass did not reach), repeat makes every pass a take over the loop (incomplete
  first and last passes kept, the last pass active) with what came before the loop start as an
  item of its own, and on a fixed-lane track with "new recording adds lanes" each pass gets a
  lane of its own (the newest playing, and swiped into the comp when there is a comp lane).
  The file keeps what was played during pre-roll, ahead of the take as in REAPER. Pressing
  record while playing punches in at the playhead.

## Automation

- **Track envelopes** (volume, pre-FX volume, trim, pan, mute, width, dual pan) with all point
  shapes, volume envelopes in REAPER's fader scaling (`VOLTYPE 1`: they move linearly in fader
  position, `gain = 10^0.6 / 2 * x * (1 + x)` with `x` the cube of the position), shown
  and editable on the lanes (double-click adds, drag moves, right-click deletes). FX parameter
  envelopes play back: bypass for every effect, wet/dry for ReaVerb and ReaDelay, and
  ReaDelay's per-tap enable, volume and feedback, every hosted plugin's parameters; they follow
  the project play rate and, like track envelopes and their automation items, tempo edits in a
  beats timebase.
- **Envelope lanes** (the envelope button on the left rail): the selected track's envelopes,
  its sends' and its effects' parameters one lane each on the arrange time axis, the selected
  item's take envelopes (volume, pan, mute, pitch) and the tempo map (REAPER's master tempo
  envelope: a point per tempo change, square or linear). Points drag,
  double-click adds and a right-click sets the shape (linear, square, slow start/end, fast
  start, fast end, bezier with its tension); a lane's Shape menu sets every point (in the time
  selection). Arm, bypass and show over the track per envelope, "+ Envelope" for any kind or
  any FX parameter or take envelope, a send lane's own automation mode (REAPER's per-send
  `I_AUTOMODE`, which automation writing follows) and the global override buttons. API:
  `envelope.*`.
- **Automation items** (`<POOLEDENV` pools and `POOLEDENVINST` instances), played as REAPER
  7.73 renders them: pool points sit in quarter notes on the tempo map (a tempo ramp bends an
  item piecewise between them), the start offset and play rate move through the pool, looped
  items wrap from the last point to the first, unlooped ones hold, baseline and amplitude scale
  around the middle of the range (unclamped), overlapping items multiply on volume and add on
  the others, and each edge ramps linearly over the item's transition time (12 ms) from the
  envelope without the item to the envelope with it, except on FX parameters. Insert them from
  the envelope, as LFO shapes (sine, triangle, square, saw up/down over a number of cycles) or as
  pooled copies; drag the strip to move, the right edge to resize; loop, baseline, amplitude,
  duplicate (pooled or not), unpool, delete (keeping the points or not) from the right-click
  menu; the pool's points show and edit inside the item, and every pooled copy follows.
- **Global automation override** (`GLOBAL_AUTO`): one mode for every track, or every envelope
  bypassed (FX parameter envelopes too).
- **Automation modes** per track (`AUTOMODE`: trim/read, read, touch, write, latch, latch
  preview), cycled from the inspector or set with `envelope.mode`. While playing, moves in
  touch, latch and write mode are recorded into armed envelopes (missing ones are created):
  fader, pan, width, mute (square points), send volume, pan and mute, and FX parameters (an
  envelope over the parameter's own range); write mode records every armed envelope. Latch
  preview remembers the moved values; "Write preview values" in the envelope lanes (or
  `envelope.latch_preview_apply`) writes them over the time selection or from or to the cursor.

## Live and stage

- **Monitor mixes** for in-ear monitoring (the headphone rail button, "View: Toggle monitor
  mixes"): one card per musician with a fader, pan, mute and "me" toggle per source, a "more
  me" knob that raises the musician's own sources against the rest, the mix level and meter,
  the device output (a pair or mono) and a safety limiter. A wizard creates a mix from all or
  some tracks (or copies one), and "SWS: Create cue buss from track selection…" makes SWS's
  cue bus with its options (send type, volumes and pans from the tracks, hardware output,
  track template). A monitor mix is an ordinary REAPER track: "MON <name>", pre-fader receives,
  master send off, a hardware output, marked with `P_EXT` values (`scythe_monitor`,
  `scythe_monitor_own`, `scythe_monitor_more_me`); "more me" changes the receive volumes
  themselves, so REAPER plays the mixes the same. The operations are plain functions on the
  project (`scythe_core::monitor`), served to remote clients as the control API's `mix.*`
  commands (hardware outputs as `send.hw_*`, per-mix meters in the `meters` event).
- **Setlists** (rail button or `L`; `scythe-app --setlist set.RPP` opens on it): the project's
  markers become songs. With stop markers (`!1016` and other stopping actions) the first plain
  marker after a stop begins a song that runs to the next stop marker, and markers inside it
  are its cues (drawn on the progress bar); without stop markers every marker is a song up to
  the next. Build several setlists per project from them (`+` or drag a song in, drag to
  reorder, double-click a tab to rename, duplicate, delete), stored in the project's
  `<EXTSTATE` (section `SCYTHE_SETLISTS`, one value per setlist, the same store as the
  ReaScript `SetProjExtState`), which REAPER 7.73 reads (`GetProjExtState`) and saves back
  unchanged. Entries find their song by the start marker's GUID, else by name, so renumbering
  or moving markers keeps them. Export as plain text (title line, one numbered song per line;
  also copied to the clipboard) and import text typed by hand. The stage view shows the song now playing with elapsed/remaining time,
  what is next up and big previous/play-stop/next buttons; `Enter` plays the current song from
  its marker, `N`/`P` move to the next/previous song (cueing it when stopped, playing it when
  playing), `Esc` stops. With auto-advance on, a song that stops at its stop marker cues the
  next one without playing it. With count-in on (per setlist) every song starts after a bar or
  more of click at its own tempo, so the drummer need not count. It runs in the session, so a headless server plays setlists
  too, and the control API has a `setlist.*` domain (songs, editing, play/cue/next, status
  and a `setlist` event; see `docs/control-api.md`).
- **Marker actions**: a marker named `!` plus command ids runs them when the play cursor
  crosses it (REAPER's SWS "marker actions": `!1016` stops, `!1016 40042` stops and goes to the
  start, `!_SWS_…` names a custom/extension command). Sample-accurate on the audio thread:
  `!1016` stops on the marker's frame; starting or seeking onto a marker does not run it, nor
  does a loop wrap landing on it. Stop (1016, 40044) returns to the edit cursor as REAPER's Stop
  does; pause (1008, 40073, 40328) leaves the cursor at the marker; go to start/end/loop start
  (40042, 40043, 40632) seek while playing, 1068 toggles repeat and 1007 is a no-op. Other ids
  run the matching Scythe action (record 1013, metronome 40364, insert marker 40157, undo…);
  named commands only show a notice. "Options: Toggle marker actions" turns it off.
- **Control surfaces**, set up as REAPER keeps them in reaper.ini (`csurf_cnt`,
  `csurf_<i>=OSC "name" flags port "ip" port …` and `MCU offset size in out flags`) under
  Settings or with the control API's `surface.*`, run by the app and by `scythe-server`. OSC
  follows REAPER's `.ReaperOSC` pattern files (REAPER's own files read cleanly; Scythe's default
  has REAPER's addresses plus `/setlist/…`): track names, volumes (REAPER's fader curve, its dB
  and pan texts), pans, mute, solo, arm, monitor, selection, sends, meters, transport, repeat,
  click, tempo, markers and regions, device banks and track selection, `/action` with REAPER
  command ids and Scythe's setlist commands, feedback bundled within the packet size. Mackie
  Control and extenders (shared bank, flip, V-pots, scribble strips, meters, time display,
  automation mode buttons, touch-sensitive faders feeding automation, F1-F8 to markers, the
  user switches as footswitches for the setlist) and HUI partly, as REAPER (faders, touch,
  select/mute/solo/arm, V-pots, four-letter names, meters, transport), and the PreSonus
  FaderPort and FaderPort v2 as REAPER drives them (one strip from the master: fader with
  touch, pan, mute/solo/arm, automation modes, track stepping, transport, the v2's shift and
  pan mode). See
  [docs/live.md](live.md#footswitches-tablets-and-control-surfaces).

### Running as a service

For a rig that has to come up on its own, `scythe-server --install-service [project] [flags]`
writes a systemd user unit running with those settings (`Restart=on-failure`, real-time
priority and locked memory as far as the user's limits allow); `loginctl enable-linger`
starts it at boot without anyone logging in. The audio thread runs with real-time priority
(`SCHED_FIFO` within the user's rtprio limit, else through RealtimeKit). `scythe-server --status`
asks the running session for its device, buffer, CPU load, xruns, reconnects, scheduling and
connected clients. A device that is
unplugged mid-show leaves the session running silently (transport, setlist and clients carry
on) while it looks for the device every five seconds and picks it up again. See
[docs/live.md](live.md) for the whole stage setup.

## Remote control

### Headless (no screen) and remote control

```sh
devbox run -- scripts/cargo.sh run -p scythe-server -- song.RPP --output-device Scarlett
devbox run app -- --headless song.RPP          # the same, from the app binary
devbox run app -- --control 0.0.0.0:8770 --control-token secret song.RPP  # window + remote
```

`scythe-server` (or `scythe --headless`) runs a session without a window and without
X11/Wayland, for example as an in-ear-monitor mixer on a machine in the rack: it opens the
audio device, loads the project, keeps input monitoring and effects running, auto-saves
backups, serves the [control API](control-api.md) on `127.0.0.1:8770` and stops cleanly on
Ctrl+C or SIGTERM (writing unsaved changes to the project's `Backups` folder). Flags:
`--audio-host`, `--output-device`, `--input-device` (exact name or part of it),
`--sample-rate`, `--buffer-size`, `--output-channels`, `--null-audio` (open no device),
`--no-input`, `--control ADDR` (`off` to disable), `--token` (the admin token), `--folder DIR`
(a folder the web UI may browse for projects; repeatable, home by default), `--access-file FILE`
(where access links are kept), `--config FILE`, `--list-devices`. The same settings can live in `$XDG_CONFIG_HOME/scythe/headless.toml`
(see `crates/scythe-server/src/config.rs`); flags win.

The control API is JSON over HTTP (`POST /api/command`) and a WebSocket (`/api/ws`) that also
streams transport, meter and project-changed events. It covers transport, project
open/save/undo, tracks, sends, effects and their parameters, items (with waveform peaks) and
markers, the tempo map, monitor mixes, setlists and access links, plus a full `snapshot`; every change is an undo step like a UI edit. The desktop app serves it too with
`--control ADDR` (or `SCYTHE_CONTROL`), so a tablet can drive a session that has a window.
The window runs the control API, control surfaces and marker actions on a thread of their own,
so they keep their pace (commands answered in a few ms, events at 20 a second) while the window
is hidden or minimised; only actions that need the window wait for its next frame.
Binding beyond localhost without a token prints a warning.

### Web UI (browser, tablet, phone)

The control API serves a web UI on the same address, embedded in the binary (no internet
needed on stage, no Node toolchain to build it):

| Page | For |
|---|---|
| `/` | the session editor (desktop, tablet): transport with bars and tempo, arrange view with waveforms on items (move, split, delete), markers and regions, a tempo lane (drag a tempo or time signature marker, double-tap to edit or add) and the tempo map, track headers, mixer with meters, sends, effects with their parameters, undo and redo, a file browser to open and save projects, monitor mixes (a wizard like the desktop one: musician, sources, own sources, output, limiter; copy, rename, delete, output, limiter, sources), setlists (make, rename, duplicate, delete, add songs, drag to reorder, import and export text, auto-advance) and access links with QR codes |
| `/monitor` | "more me" on a musician's phone: pick your monitor mix (remembered per device), a big more-me knob, a fader and mute per source, a click fader, the mix's meter; nothing else of the session |
| `/setlist` | the drummer's tablet: now playing and next up in large type, progress with cue ticks, big previous / play-stop / next, the setlist's songs to cue with a tap, the count-in beats and a count-in switch |

The monitor and setlist pages appear once the server offers monitor mixes (`mix.*`) and
setlists (`setlist.*`); against an older server they say so instead.

To open it from a phone or tablet on the band's network, listen on the LAN with an admin token:

```sh
scythe-server --control 0.0.0.0:8770 --token admin-secret --folder ~/live set.RPP
```

The admin token gives full control; keep it to whoever runs the session. Everyone else gets an
**access link** made in the editor (the QR button in the top bar, or the QR button on a monitor
mix): a phone scans its code and is in, without typing anything.

| Link | Opens | May |
|---|---|---|
| musician | `/monitor`, locked to their mix | read and change their own monitor mix (levels, pans, mutes, more me, its volume); see the transport and setlist status; nothing else |
| stage | `/setlist` | play the setlist: choose, cue, play, stop, next, previous, auto-advance; plain play and stop; no editing |

Scythe checks every command against the link's role on the session's side, so a link cannot do
more by calling the API itself, and its WebSocket only carries what the role needs (a
musician's meters are their mix's alone). Links are kept in
`$XDG_CONFIG_HOME/scythe/access.json` (or `--access-file`), readable by its owner only, not in
the project: they are secrets, and a phone's link keeps working when another project is
opened. A link finds its mix by the musician's name. Renewing a link gives it a new token;
deleting it locks its phones out at once. When the editor runs on the machine itself
(`localhost`), the codes carry the machine's network address instead, so phones can reach it.

A browser with the admin token can still open `/monitor` (pick any mix, remembered per device;
`/monitor?mix=Bass&lock=1` pins one) and `/setlist`. Pages reconnect by themselves when the
wifi drops or Scythe restarts, and grey out while they are offline.

`SCYTHE_WEB_DIR=crates/scythe-server/web` serves the files from disk instead, for working on
the UI without rebuilding. Its logic tests run with `node crates/scythe-server/web/test/run.mjs`
(part of `devbox run test`) or in any browser at `/test/`.


### AI agents (MCP)

```sh
devbox run -- scripts/cargo.sh build --release -p scythe-mcp
claude mcp add scythe -- $PWD/target/release/scythe-mcp --url http://127.0.0.1:8770
claude mcp add --transport http scythe http://127.0.0.1:8770/mcp   # or without the binary
```

Scythe is an [MCP](mcp.md) server, so Claude Code, Claude Desktop and other agents can
read and edit a running session: a compact project overview (`get_project`), transport,
project open/save/undo, tracks, sends and hardware outputs, effects (catalog search and
parameters), items, markers, monitor mixes and setlists, plus `scythe_command` for any control API command. Every Scythe serving the control
API answers MCP over Streamable HTTP at `/mcp`; the `scythe-mcp` binary serves stdio, bridging
to a running Scythe (`--url`, `--token`) or running a session of its own (`--headless
song.RPP`). Tool schemas are generated from the API's command types; deleting, opening,
starting a new project, saving and running scripts are marked destructive for clients to confirm.

For building live sessions and mixing, agents also get: `media_import` and `media_probe` (audio
and MIDI files as items, with length, rate and loudness), `analysis_segments` (songs in a
backing-track or click file, with tempo, and proposed song and `!1016` stop markers),
`analysis_file`, `analysis_mix`, `analysis_compare` and `analysis_match_eq` (EBU R128 loudness,
true peak, loudness range, crest factor, third-octave spectrum and tonal balance, width and
correlation per band, low-end mono check, brightness, transient density; mixes rendered
offline through the render path, never played; a few limited-gain ReaEQ bands proposed and
applied as one undo step, with compression and limiting advice), `events_since` and
`events_meters` for polling, the access links with their QR payloads, and `script_run` for
ReaScript text in the desktop app. A compact set of tools is listed by default; `--tools
mix,setlist` (or `/mcp?tools=all`) and the `scythe_tools` tool list more, with
`tools/list_changed`. Clients can subscribe to `scythe://project` and get log messages for
notices, transport and device health (stdio, or `GET /mcp` as server-sent events). Prompts
`build_live_session`, `setlist_from_backing_tracks` and `match_reference` walk an agent through
the whole job.

You see what an agent does while it works. A light in the top bar (desktop and web editor) shows
when an agent is connected: dim while idle, a soft pulse while its calls run, pink briefly after
one failed. Its tooltip says which agent and what it reports doing (`set_status`, such as
"Balancing drums against the reference"); a click opens the Agents window with the connected
agents (name and version from the client, HTTP or stdio, since when), every recent tool call in
words ("Set Vocals volume to -3 dB") with its result and the undo steps it made, and three
controls: undo the last agent change, pause agents (their changes are refused, reading still
works) and disconnect. Undo steps carry the agent's name ("Undo: Track volume (Claude Code)")
while their labels stay REAPER's. The same is in the control API (`agents.activity`,
`agents.pause`, `agents.undo`, `agents.disconnect`, the `agent_activity` event).

## Scripting

- **ReaScript (Lua 5.4, EEL2, Python)**: "Script: Run ReaScript" runs a `.lua`, `.eel` or `.py`
  file against the project as one undo step (named by `Undo_EndBlock`), with `ShowConsoleMsg`
  output in a console window. A script that stops on an error keeps what it changed before it,
  as one undo step, as in REAPER. 310 `reaper.*` functions:
  - tracks: `CountTracks`, `GetTrack`, `GetMasterTrack`, `InsertTrackAtIndex`, `DeleteTrack`,
    `GetTrackName`, `GetSetMediaTrackInfo_String` (`P_NAME`, `GUID`, `P_EXT:`),
    `Get/SetMediaTrackInfo_Value` (volume, pan, mute, solo and its modes, solo defeat, polarity,
    pan mode, width, dual pan, arm, record input and monitoring, main send and its channel
    offset, folder depth, automation mode, channels, track number, selection, custom color, pan
    law, fixed lanes, MIDI hardware output, `P_PARTRACK`), `Get/SetTrackColor`, `GetParentTrack`,
    `GetTrackDepth`, `GetTrackUIVolPan`, track groups (`GetSetTrackGroupMembership`, `High`,
    `Ex`; `TRACK_GROUP_NAME:n` in `GetSetProjectInfo_String`), control-surface edits that carry
    over to group followers (`CSurf_OnVolume/Pan/Width/Mute/Solo/RecArmChange(Ex)`,
    `SetTrackUIVolume/Pan/Width/Mute/Solo/RecArm/Polarity`), selection
    (`CountSelectedTracks(2)`, `GetSelectedTrack(2)`, `IsTrackSelected`, `SetTrackSelected`,
    `SetOnlyTrackSelected`), `ReorderSelectedTracks` with its three folder modes;
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
    as REAPER's API does; automation items: `CountAutomationItems`, `InsertAutomationItem`,
    `GetSetAutomationItemInfo(_String)` and the `…Ex` point calls on an item (every pass of
    its pool inside the item, baseline and amplitude applied, as REAPER gives them);
    `Get/SetGlobalAutomationOverride`;
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
    run once the script's changes are in; custom actions run their members), `Main_OnCommandEx`,
    `Undo_*` (`Undo_CanUndo2`, `Undo_DoUndo2` …), `MarkProjectDirty`, `IsProjectDirty`;
  - actions: `NamedCommandLookup`, `ReverseNamedCommandLookup`, `kbd_getTextFromCmd`,
    `SectionFromUniqueID`, `CountActionShortcuts`, `GetActionShortcutDesc`,
    `GetToggleCommandState(Ex)` (a custom action's is its members' when they agree),
    `SetToggleCommandState`, `RefreshToolbar(2)`, `AddRemoveReaScript`, `Get/SetMouseModifier`,
    `get_action_context`;
  - state chunks: `Get/SetTrackStateChunk`, `Get/SetItemStateChunk`,
    `Get/SetEnvelopeStateChunk`, through the same RPP reader and writer as project files;
  - general: `genGuid`, `guidToString`, `stringToGuid`, `GetTrackGUID`, `AnyTrackSolo`,
    `SoloAllTracks`, `MuteAllTracks`, `ClearAllRecArmed`, `GetNumTracks`,
    `GetLastTouchedTrack`, `IsTrackVisible`, `GetMasterMuteSoloFlags`, `file_exists`,
    `RecursiveCreateDirectory`, `EnumerateFiles`, `EnumerateSubdirectories`, `SetEditCurPos2`,
    `OnPauseButton`, `Main_SaveProject`, `GetPlayStateEx`, `MB`, `Undo_OnStateChangeEx(2)`;
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

## Playback, rendering and formats

- **Playback** of audio items (WAV/FLAC/MP3/OGG/AAC…), reversed and sectioned items (`<SOURCE
  SECTION>`), click-source items and MIDI items (through a built-in preview instrument: drums on
  channel 10, a soft synth elsewhere). MP3s are read the way REAPER reads them: items keep the
  encoder delay and padding and play a Xing/Info header frame as a frame of silence, the sampler
  trims them (gapless) and holds them to 16-bit range, and both scale by 32767/32768; mid/side
  granules of MPEG-2 and 2.5 streams come out 3 dB low, as REAPER's decoder plays them. Opus files
  decode with the pure-Rust `opus-pure` (pre-skip and end trim applied, the header's output gain
  too, surround in the file's Vorbis channel order, as REAPER plays them) and WavPack through
  FFmpeg (16/24-bit, float, multichannel, hybrid).
- **Sample-rate conversion**: Media at another rate, and takes at another play rate without "Keep
  pitch" (a conversion from the file's rate times the play rate), are converted with REAPER's
  default playback mode, the 64-point sinc, for playback; every render follows the project's
  `RENDER_RESAMPLE` mode (16- to 768-point sinc, linear, point, linear with one or two IIR
  low-passes, r8brain free).
- **Files**: lossless `.RPP` load/save, `<name>.RPP-bak` on save, auto-save every 5 minutes to
  `Backups/` (held while a song plays, up to 15 minutes, never while recording; the newest 50
  kept), media import (dialog and drag-and-drop), render to WAV (whole project, time selection or
  stems from the action palette; headless with `scythe-app project.RPP --render=out.wav
  [--range=START-END]`), or with the project's own render settings like REAPER's `-renderproject`
  (`--render-project` or the palette): bounds project, time selection, custom range, all or
  selected regions or selected items, and the render sources of `RENDER_STEMS`: the master, stems
  of the selected tracks (a track's own output, without its folders or the master) with or without
  the master, selected tracks via the master, selected items alone through their track or through
  the master, and the region render matrix (`<REGIONRENDER` rows, edited in the region/marker
  manager window). `RENDER_FILE` and wildcards such as `$project`, `$track`, `$tracknumber`,
  `$item`, `$itemnumber`, `$region`, `$regionnumber` (two digits), `$regionname`, date and time;
  names that come out the same get REAPER's `-001`, `-002` suffixes. The render queue: "Add
  project to render queue" writes a copy to REAPER's `QueuedRenders` folder as REAPER does
  (`qrender_<date>_<time>_<name>.RPP` with `QUEUED_RENDER_OUTFILE` lines), "Render all queued
  renders" and `--render-queue` render and remove each, and REAPER's own queued copies render in
  Scythe and the other way round.
- **Render formats**: MP3, FLAC, Ogg Vorbis, Opus and WavPack renders go through `lame`, `flac`,
  `oggenc`, `opusenc` and `wavpack` (in the devbox packages) with the project's settings (Opus:
  bitrate per channel, VBR/CVBR/hard CBR, complexity; WavPack: 16/24/32-bit integer or 32-bit
  float, fast/normal/high/very high), and mono renders mix both channels down (in double
  precision) as REAPER does. WAV is written as 8-bit unsigned, 16, 24 or 32-bit integer, 32 or
  64-bit float, 8-bit u-law, 4-bit IMA ADPCM or REAPER's 2-bit cADPCM. Integer samples are scaled
  by 2^(bits-1) and rounded half up, as REAPER writes them; u-law is G.711 of the sample in 16-bit
  units plus 2, rounded down (negative values one's complemented), as REAPER codes it; ADPCM
  follows REAPER's coder (codes rounded down after taking half a step off, the prediction wrapping
  at 16 bits, 1024-byte blocks per channel, the last block filled up by coding silence). WAV files
  carry the chunks REAPER's WAV sink writes, in its order: the Broadcast Wave `bext` (REAPER as
  originator over the start of the description, the local date and time, the render's start as
  time reference, optionally `RPP:<project file>` as description), the room for a larger header
  (`junk`), an ACID chunk with the project tempo, and project markers and regions (all, markers or
  regions only, optionally only those named `#…`; markers from the start to before the end,
  regions wholly inside) as `cue `, `smpl` loops and `LIST`/`adtl` labels. Wave64 and RF64/BW64
  are written when forced, or when a WAV left room for them outgrows 4 GB (the header is rewritten
  in place; RF64 adds its `r64m` marker chunk, Wave64 its own UTF-16 `cue ` chunk). With "embed
  metadata" (`RENDER_STEMS` flag 512) the project's `<RENDER_METADATA` fills `bext` and an `iXML`
  copy (`BWF:`), `iXML` (`IXML:`), XMP as `_PMX` (`XMP:`), `axml` (`AXML:ISRC`), `cart` (`CART:`),
  an ID3v2.4 `id3 ` chunk (`ID3:` text frames, `COMM`, `TXXX`) and `LIST`/`INFO` (`INFO:`). AIFF
  is written with 8, 16, 24 or 32-bit samples, a beat count (`basc`) when the render is a whole
  number of beats, and `NAME`/`AUTH`/`ANNO`/`(c) ` (`IFF:`), XMP (`APPL`) and ID3 chunks; CAF with
  8 (unsigned, as REAPER writes it), 16, 24 or 32-bit integer or 32/64-bit float samples, markers
  and regions as `mark`, `regn` and `strg`, and the tempo and `CAFINFO:` tags in `info` chunks.
- **Video renders**: REAPER's GIF and LCF sinks write the video frames (black where no video item
  plays) at their size and highest frame rate: GIF with giflib's LZW coder, a frame (its changed
  rectangle, its own colour table) only when the picture changes, delays added up in milliseconds;
  LCF (LICEcap) in groups of 20 frames of BGR565 pixels in 128x16 blocks, each block stored once
  per run of equal frames and deflated at zlib level 9. REAPER's video sink (FFmpeg) goes through
  `ffmpeg`: AVI, MPEG-1, MPEG-2 (transport stream), MOV, MP4, Matroska, FLV and WebM with the
  codecs REAPER lists for each (H.264, H.265, AV1, VP8, VP9, MPEG-1/2, XviD, FLV1, FFV1, Huffyuv,
  MJPEG, DV; MP3, AAC, AC-3, MP2, Vorbis, Opus, 16/24-bit and float PCM, or no stream), the video
  and audio bitrate, frame size (a side of 0 follows 16:9 from the other, both 0 is 1920x1080:
  REAPER's sink does not read the project's video size) and rate, MJPEG's quality factor, and the
  audio and video encoder option strings (`key=value` pairs up to a `;` or `#`: `codec`, `tag`,
  `q`/`qscale`, `range=jpeg`, and any other encoder option such as `crf`, `preset`, `g` or `b`).
  Frames are black where no video item plays; video items (top track first) are converted to YV12
  and scaled with REAPER's scaler (bilinear from the top-left corner, no half-pixel centring) into
  the frame keeping their aspect ratio with black bars, or stretched. A clip shows on the frames
  whose middle it covers, each with the source frame on screen at that middle (B-frame streams
  timed by their decoding timestamps, as REAPER does). Video items play their own audio, decoded
  by FFmpeg. Three stock video processor presets are rendered, recognised by their code, as take
  or track FX: "Track opacity/zoom/pan" (without rotation or a forced size), "Invert colors" and
  "Brightness/Contrast".
- **CD images and DDP**: Audio CD images (`.cue` and a 44.1 kHz 16-bit `.bin`, as REAPER's CD
  image sink writes them) take their tracks from markers (each at least 4 seconds after the
  previous track; `PERF=`, `WRIT=` and `ISRC=` markers fill in the track's cue entries; optionally
  only `#` markers), from regions (audio outside regions left out) or as one track, after an
  optional lead-in of silence. DDP 2.00 filesets (DDPID, DDPMS, PQDESCR, CDTEXT.BIN, the image and
  checksum.md5) follow REAPER's DDP sink: `#` markers start tracks, `!` markers set INDEX 0, `@`
  markers hold the album, `KEY=value|…` metadata becomes ISRC, UPC/EAN and CD-TEXT (title,
  performer, songwriter, composer, arranger, message, disc identification, genre, language), and
  the Red Book rules (2 s before the first track, 4 s minimum tracks) move indexes as REAPER moves
  them. As in REAPER, a CD image or DDP render of other than two channels fails with "Unable to
  create output file …, sample rate or channel count may not be compatible" and leaves the empty
  `.cue` and `.bin` (or image) behind. Other sinks are written as WAV. The project's normalisation
  (`RENDER_NORMALIZE`: LUFS-I, RMS, peak, true peak, LUFS-M/S max to a target) and dither
  (`RENDER_DITHER`: TPDF, optionally noise-shaped, for 8-, 16- and 24-bit) apply.

## Project and media

- **Media explorer** (View: Media explorer, 50124): folders and REAPER media databases
  (`MediaDB/*.ReaperFileList`, the shortcut list in `[reaper_sexplorer]` as reaper.ini keeps
  it, so REAPER's own can be copied over), columns for length, rate, channels, bit depth or
  bitrate, BPM, key, title, artist, genre and comment (from Vorbis comments, ID3, MP4 tags,
  RIFF `INFO`, BWF `bext` and ACID tempo; the columns share the window's width), search
  (words, `-word`, `"phrases"`, subfolders), favourites (the database's favourite flag), a
  preview player that plays straight to the device after the master with tempo match (on, /2,
  x2, pitch kept or not), pitch and volume, and inserting at the edit cursor on the selected or
  a new track, tempo-matched. The explorer section's actions (preview, tempo match, insert,
  favourite, scan) have REAPER's ids.
- **Project bay** (41157) and **clean project directory** (40098): the project's media files
  (with takes, items and tracks using each, missing ones marked), items and effects;
  retargeting a file in every take; the unused files of the recording folder listed as REAPER
  lists them and sent to the trash or deleted.
- **Project tabs**: Project tabs: several open projects, the inactive ones stopped with their undo
  history, an unsaved one named as REAPER names it (`[Unsaved]` on its tab, `[unsaved project]` as
  the title), kept in the preferences under REAPER's `projecttab<n>` keys.
- **Subprojects**: Subprojects (`RPP_PROJECT` items) play their proxy, `<file>-PROX`: a 32-bit
  float WAV of the subproject's master mix at its own rate, rendered when missing and again when
  the subproject is saved; double-clicking such an item opens it in a new tab.

## Actions, windows and themes

- **Actions, shortcuts, toolbars, mouse modifiers**: one action registry with REAPER's command
  ids and names where REAPER has the action (154 of them, names checked against REAPER 7.73's
  action list) and `_SCYTHE_…` named commands for Scythe's own; the palette (Ctrl+K), keys,
  toolbars, mouse clicks, surfaces, learned controls, `!` markers and `Main_OnCommand` all run
  through it, with toggle states. The Actions window (REAPER's "Show action list") searches,
  runs, assigns and removes keyboard shortcuts (capturing the next key), makes custom actions
  (macros of other actions, with "consolidate undo") and registers scripts as actions, and
  imports and exports `.ReaperKeyMap` files. All of it is kept in `reaper-kb.ini` as REAPER
  writes it (`KEY` with REAPER's key encoding and comments, `ACT`, `SCR`, MIDI shortcuts from
  learn), so a REAPER keymap works in Scythe and back. Toolbars (`reaper-menu.ini`, `.ReaperMenu`
  import and export): the main toolbar as a strip under the header and 32 floating toolbars,
  with REAPER's toolbar icons when its `Data/toolbar_icons` is around, lit while their action is
  on, and a toolbar editor; a new button reads the same on every toolbar (the main toolbar's label,
  else the action's name without its category). Mouse modifiers (`reaper-mouse.ini`): actions on clicks and double
  clicks of items, track lanes, track headers and the ruler, per modifier keys, with an editor that names each unassigned combination's default as
  REAPER 7.73's preferences describe it.
  Window sets ("Screenset: Save/Load window set #01-10") keep the pages, windows, toolbars and
  zoom. API: `action.*`, `toolbar.*`, `mouse.*`.
- **Windows** share one card frame and one set of rules: tool windows (actions, regions,
  explorer, bay, grouping, theme, effect editors, the rig window…) resize, dialogs (grid, lock,
  monitor mix, MIDI devices…) size to their contents, floating ones (toolbars, the virtual
  keyboard, JSFX and script graphics) keep their contents' size. A window opens centred on the
  main window, or beside the pointer for those opened on something (crossfade, nudge and dynamic
  split dialogs, tempo marker, note properties, toolbar editor), always fully on screen; a newly
  opened window comes on top. Where each was moved and how it was sized is kept in
  `windows.ini` beside the preferences and comes back in the next session (kept on a smaller
  screen). Escape closes the window last clicked or opened instead of stopping the transport;
  once the arrange or another part of the main window is clicked, Escape is a shortcut again.
- **Themes** (Options: Show theme color controls, 42392): a colour for each of Scythe's 16
  palette colours in `theme.ScytheTheme`, edited in a window, saved and loaded as files, and
  REAPER `.ReaperTheme`/`.ReaperThemeZip` colours imported where they mean the same (arrange
  and panel backgrounds, track lanes, text, the envelope colours). WALTER layouts and images
  are not read.

## Audio device

- **Audio device window** (REAPER's Preferences > Audio > Device page, in Scythe's own rows,
  dropdowns and switches): opened with "Options: Preferences..." (40016, Ctrl+P) or "Options:
  Audio device settings..." from the action palette, the Audio device... button of the settings
  rail's window, or a click on the rate · block · CPU pill in the top bar, as REAPER opens its
  device page from the audio status in its menu bar. Each audio system shows REAPER's settings
  for it: JACK the input and output channels, auto-start (with the launch command, blank for
  libjack's own) and auto-connect to the hardware ports; ALSA the channels with the input and
  output devices, sample rate, block size, bit depth (16, 24 or 32-bit integer, the device's
  own format when it has not that one) and RT priority for the audio thread (0 is non-RT);
  PulseAudio (a sound server of its own, or PipeWire's) the channels, rate and block size,
  opened as 32-bit as REAPER opens it; Dummy Audio the rate and block size. "Allow projects to
  override device sample rate", "Disable power management" (`/dev/cpu_dma_latency` held at 0)
  and "Lock process memory with mlockall()" are there too. Apply stops the transport and opens
  the new device without closing the project: effects keep their state, media is decoded again
  at a new rate, plugin editors close. When the device cannot be opened the error shows in the
  window and the previous device plays on.
- **ALSA device lists as REAPER's**: one entry per sound card, read from `/proc/asound/cards`
  (no device is opened to list them), the same list for input and output, labelled as REAPER
  7.73 labels them: `hw:QUADCAPTURE ; USB-Audio - QUAD-CAPTURE`. Cards without a PCM device (a
  MIDI keyboard) are left out; a card with several PCM devices still shows once (`hw:<id>` is
  its first device). The long tail ALSA also knows (`default`, `pipewire`, `sysdefault`,
  `front`, `surround51`, `iec958`, `plughw` twins) and PulseAudio's sinks, sources and monitors
  are not listed, as REAPER does not list them; `--list-devices --all` and `audio.devices
  {"all": true}` print them, and any ALSA name (`hw:1,0`, `plughw:QUADCAPTURE`, a PCM of your own
  `asoundrc`) can still be set through `--output-device`, `[audio]` or `audio.set_device`.
  REAPER's `alsa_outdev=hw:QUADCAPTURE` opens the same card. "System default (PipeWire)" is the
  entry for no device; while it plays, the window and the top bar pill's tooltip say so, so the
  interface still gets picked.
- **What the chosen device offers**: on choosing a device the window asks it for its rates,
  channel counts and bit depths and offers only those, defaulting to its own (a QUAD-CAPTURE:
  44100 Hz, 6 in, 4 out, 32 bit). A USB interface another program holds is read from
  `/proc/asound/cardN/streamM` instead. A rate the device does not run at opens at the nearest
  one it does (REAPER's ALSA device asks for the nearest rate); a channel count below a
  device's fixed count opens it whole and offers every channel, as REAPER 7.73 shows 6 in / 4
  out of a 6-in, 4-out device asked for 2/2.
- **Latency as REAPER shows it**: the window's "Playing" line and the top bar pill show REAPER's
  figure, `~2.9/2.9ms`: one block in, the periods but one out, in milliseconds cut to a tenth
  below ten (measured in REAPER 7.73: 128 frames in 2 periods at 48 kHz is `~2.6/2.6ms`, 512 in 4
  `~10/32ms`, 64 in 3 at 96 kHz `~0.6/1.3ms`). Their tooltips add the delay the device measures
  (the driver's queue, a USB interface's transfer buffers included) for output and, while the
  input is open, input. ALSA devices open with exactly the block size in the periods the
  window's "Periods" sets (REAPER's `linux_audio_bufs`, 2-8, default 3 as in REAPER; each
  choice shows the latency it gives, the output being the block times the periods but one;
  `--periods`, `periods` in `[audio]` and `audio.set_device`); `audio.device` reports the
  periods and the period and ring sizes the driver granted (`period_frames`, `ring_frames`),
  `transport.health` the periods and ring. The input runs at the same block size; the monitored input is
  played in the block it arrives in, with at most one block of slack between the capture and
  playback clocks. A PulseAudio/PipeWire stream asks for a two-block buffer of the block size
  (512 frames when none is set) instead of the server's default, and opens at the sink's own
  channel count. API: `latency`, `output_latency` and `input_latency` in `audio.device`,
  `latency` in `transport.health`.
- **A device another program holds**: switching closes the old device's streams before opening
  the new one, as REAPER does. A device that is busy (PipeWire keeps an interface open until its
  node suspends, about five seconds after its last stream stops) is tried again every quarter
  second for up to seven seconds while the window and pill say "Waiting for PipeWire to release
  hw:QUADCAPTURE…"; the same happens at start. When it stays busy the previous device comes
  back and the "Error opening devices" window says who holds it (the owner of the PCM from
  `/proc/asound`, PipeWire named as such) and what to do: choose the interface on the PipeWire
  or JACK system, or free it for ALSA (its profile Off in pavucontrol or `wpctl set-profile`).
- **Saved as REAPER saves it**: the choice goes into the preferences under REAPER 7.73's Linux
  reaper.ini keys (`linux_audio_mode` 0 JACK, 1 ALSA, 2 Dummy, 3 PulseAudio; `alsa_indev`,
  `alsa_outdev`, `linux_audio_nch_in`, `linux_audio_nch_out`, `linux_audio_srate`,
  `linux_audio_bsize`, `linux_audio_bits`, `alsa_rtprio`, `jack_options` (bit 1 no auto-start,
  2 no audio auto-connect), `jack_launchcmd`, `linux_audio_srateor`, `linux_disable_pm`,
  `linux_mlockall`), so REAPER's settings carry over. The next start (desktop and
  `scythe-server` without device flags or `[audio]`) opens the same device. One that does not
  open leaves the audio closed and shows REAPER's "Error opening devices" window ("There was an
  error opening the audio hardware.", Retry, Audio device, Close); the engine also looks for it
  again every few seconds. The command-line flags override the choice for one run. API:
  `audio.devices`, `audio.device`, `audio.set_device`, `audio.channel_name`,
  `audio.channel_map` (admin only).
- **Project sample rate**: with "Allow projects to override device sample rate" (REAPER's
  default) a project whose Project settings ask for their rate (`SAMPLERATE <rate> 1`) reopens
  the device at it when it opens; as in REAPER the device stays at that rate when a project that
  asks for none is opened, a new project starts or tabs switch. JACK keeps the server's rate.
- **Closing the device** (REAPER's Audio page, `audiocloseinactive_linux`,
  `audioclosestop`): "Close audio device when stopped and application is inactive" (and, as a
  sub-option, also while tracks are record armed) and "... when stopped and active (less
  responsive)" close the device while stopped and open it again on play or when the window is
  active again.
- **Large interfaces**: every channel the device opens is offered. With no count chosen a
  hardware device with a fixed channel set (an 18-in, 20-out interface's `hw:` device) opens all
  its channels, a sound server or plugin device (`default`, `plughw`, JACK) a pair; the channel
  counts open more. Track record inputs ("Input: Mono" and "Input: Stereo" with every channel
  and pair, as REAPER lists them, the pairs starting on even channels too unless "Show
  non-standard stereo channel pairs" is off, `allstereopairs=0`), track hardware outputs,
  monitor mixes, the metronome output and ReaInsert list them all; monitoring and recording
  reach up to 64 inputs. Monitoring and recording share the device's one capture stream, so a
  device that opens for capture only once records while tracks monitor.
- **Channel names and remapping** (REAPER's "Input/Output channel name aliasing/remapping"):
  channels go by REAPER's names (`Input 3`, `Output 12`; on JACK the system port they connect
  to, `system:capture_3 (Input 3)`), or by the window's channel map: per channel an alias and
  the hardware channel it plays (or none, "(not connected)"), with Add, Remove, Reset, and Load
  and Save of `.ReaperChanMap` files (`[reaper_chanmap]`, in `ChanMaps` beside the
  preferences). Tracks see the map's channels: an input mapped to hardware input 3 records and
  monitors that input, an output mapped to hardware output 3 plays there. The map is kept per
  device as REAPER keeps it (`[alias_in_<device>]` and `[alias_out_<device>]` with `name<i>`,
  `ch<i>` (-1 for none), `map_size`, `map_hwnch`; `useinnc` bit 1 for inputs, 2 for outputs).
- **Performance meter** (REAPER's View: Show performance meter window, 40240): RT CPU (render
  time over buffer time of the last callback), the RT longest block of the last few seconds
  against the block's budget, xruns and late blocks, and a row per track, slowest first: the CPU
  its items and effects take (percent of one core), its longest block, effect count and plugin
  delay in frames, with each effect's share when hovering the name (the master's effects in a row
  of their own). A reading is taken about once a second. API: `transport.performance`;
  `examples/perf_probe.rs` prints the same per track and effect for any project, with block
  time percentiles.
- **Track threads**: tracks that do not feed each other render in parallel, on one thread per
  fast physical core (the performance cores of a hybrid CPU; 2 to 8; `SCYTHE_TRACK_THREADS`
  overrides), each taking the slowest track left by the last block's time. They run at the
  audio thread's real-time priority once it has it (asked for after the audio thread, as
  RealtimeKit grants a user only a few threads), so a busy desktop cannot hold up a block. On
  a 48-track session with six NAM amps (6 P + 8 E cores) 128-frame blocks at 44.1 kHz took
  0.72 ms at the median with six threads and 1.25 ms with one per logical CPU. On a hybrid
  CPU the audio thread and the track threads stay on the performance cores: the real-time
  scheduler otherwise wakes them on whichever CPU runs nothing real-time, and with the window
  and other programs busy on the performance cores that was mostly an efficiency core (90 of
  105 samples), where a track takes about twice as long.
- **Anticipative FX** (REAPER's Preferences > Audio > Buffering, "Anticipative FX processing";
  on by default as in REAPER): while a device plays, the tracks that hear nothing live render
  ahead of the playhead on worker threads, by the render-ahead (200 ms by default, `renderaheadlen`),
  in chunks of about 512 frames, so a heavy chain only has to keep up on average instead of
  finishing within every device block. Live are the armed tracks, tracks with a hardware MIDI
  output or a hardware insert, a monitor mix's click, output recording, REAPER's per-track
  "prevent anticipative FX" (`PERF` 2), and everything they feed (a track sending MIDI to a live
  track too); they render in the callback at the device block, so monitored input, live MIDI
  and virtual instruments keep their latency, with what the tracks rendered ahead send into
  them added in. The master renders in the callback. A jump (seek, loop end, marker action),
  start or stop hands every track back to the callback for that block, and the callback waits
  for the worker's first chunks after it rather than dropping the tracks rendered ahead. No
  effect hears a block twice: an edit or a parameter change on a track rendered ahead applies
  from where the worker is, a render-ahead later, as REAPER does (a ReaDelay change in REAPER
  7.73 with anticipative FX on was heard 232-389 ms after `TrackFX_SetParam`, every echo once).
  A track whose effects are being tweaked (an effect editor open, a parameter changed through
  the window, the control API or a script in the last three seconds) then renders live, so
  further changes are heard in the next block: the worker stops where it has rendered to, the
  callback plays that out and continues every track from that frame, and the worker starts
  again there with the new plan; arming or disarming a track while playing switches the same
  way.
  Meters of tracks rendered ahead move when their audio is played. Playback is sample-identical
  to rendering every track in the callback: Song 2's effects (NAM, ReaVerb, ReaComp, ReaEQ,
  ReaGate, ReaLimit, Scythe EQ) give the same samples at 128, 384, 512 and 1024-frame blocks
  (`examples/block_identity.rs`), and a test plays a project with folders, sends, a delay, an
  EQ and a loop both ways and compares them. Settings in the audio device window (Buffering:
  anticipative FX, render-ahead, "Allow live FX multiprocessing"), `workrender` bit 1,
  `renderaheadlen` and `workthreads` (threads rendering ahead, 0 for one per performance core)
  in the preferences; API `audio.buffering` / `audio.set_buffering`, and `ahead_ms`,
  `ahead_misses` and `live_tracks` in `transport.performance`. On a 48-track session with six
  NAM amps the callback took 0.06 ms at the median and at most 0.4 ms of a 2.9 ms block over
  five minutes (2.37 ms at most without), and the process 100 % of a core against REAPER's
  200-280 % (see docs/verification.md).
- **Window frame rate**: the window draws 30 frames a second while the transport plays, records
  or a meter still moves, whatever the display's refresh rate, and 4 a second otherwise (input,
  edits and control API commands redraw at once). `SCYTHE_UI_PROFILE=1` prints frames per second
  and the time each frame's layout takes.

## Command line

```sh
devbox run app                     # opens the generated demo song
devbox run app -- path/to/song.RPP # opens a REAPER project
devbox run test
```

Flags: `--demo` (open the demo song), `--play` (start playback on launch), `--render-project`,
`--render-queue [dir]` (render every project in REAPER's render queue, `QueuedRenders`),
`--no-input` (never
open the audio input for monitoring), `--audio-host`, `--output-device`, `--input-device`,
`--sample-rate`, `--buffer-size`, `--output-channels`, `--input-channels` (as `scythe-server`
takes them, overriding the saved audio device for this run), `--list-devices` (print the
audio devices the device window lists, as `id ; name`, and exit; with `--all` every name the
audio systems know), `--null-audio` (open no audio device and play on a
silent real-time clock; ALSA's `null` device is no substitute, it takes audio as fast as it is
made), `--view=START-END`
(open zoomed to a range in seconds), `--render=out.wav` (render the project without a window).
A `.lua`, `.eel` or `.py` file among the arguments runs as a ReaScript once the project is open.
`scripts/cargo.sh` runs cargo with the devbox library paths: `devbox run -- scripts/cargo.sh clippy`.
Debug builds compile `scythe-engine` at `opt-level = 1` and all dependencies at 2, so a debug
build plays real projects (a live set's media loads in 10 s instead of 23 s and plays at 5 %
DSP load instead of 13 %), at about 5 s more per engine rebuild.
`SCYTHE_UI_SCALE=0.75` scales the UI (0.5–3).

The demo stems are synthesised on first launch into `$XDG_DATA_HOME/scythe/demo`.
