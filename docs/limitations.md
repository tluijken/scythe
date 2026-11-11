# Not there yet

What Scythe does not do yet, or does differently from REAPER 7.73, by area. The
[features](features.md) list what is there and [verification.md](verification.md) how it was
measured.

- Windows: they are drawn inside the main window (egui), so none can be moved onto a second
  screen or out of the main window; windows opened beside the pointer do not remember where they
  were moved.
- Stock effect editors: ReaFIR's gate, compressor, subtract and convolve modes have no editor
  (they are not emulated); a ReaXcomp band is added by splitting one at the middle of its range
  (where REAPER puts a new band was not measured); a ReaControlMIDI scale edited by hand is saved
  as "Custom" unless it is one of Scythe's scales. Modulation and learn
  from an editor work on a track's own top-level effects only. `TrackFX_Show` with flag 1 shows
  the effect's parameters in the inspector, as Scythe has no FX chain window.
- Scythe EQ: REAPER does not have it, so a project using it plays in REAPER with the slot
  offline (passing audio unchanged) and REAPER keeps its settings for Scythe. Dynamic bands in
  the natural and linear phase modes add one correction kernel per band to the static curve,
  exact when a single dynamic band moves and a first-order approximation when two overlapping
  ones move at once; in natural phase the band's movement between its gain and its range is a
  blend of the two curves rather than the analog filter at each gain in between. In those
  modes mid/side bands act before left/right bands whatever their numbers (in zero latency the
  bands run in order). Cut, notch, band pass and all pass bands cannot be dynamic. Spectral
  bands detect on the EQ's output (or the external sidechain), not on its input, and add an
  STFT's latency in every phase mode. Solo and sidechain audition skip the latency
  compensation of the FIR modes. The analyzer's "Compare", collisions and EQ Match read
  another Scythe EQ instance's signal (or a file); a track without one cannot be compared.
  EQ Match fits bells and shelves only. Changing the phase mode, the linear phase resolution or
  whether any band is spectral makes a new instance (for latency compensation), which restarts
  its filters with a click if it plays. The instance list covers top-level slots on tracks and
  the master (not input, take or monitoring chains or containers). There is no undo history of
  the analyzer settings (they are stored with the instance but not automatable).
- Audio performance: anticipative FX renders ahead only while a device plays (the silent
  clock of `--null-audio` renders every track in the callback). The first change to an effect
  on a track rendered ahead is heard a render-ahead later (200 ms by default), as in REAPER;
  only then does the track render live. A change made inside a plugin's own editor counts only
  while the editor is open in Scythe's window. A seek still lets the effects of the tracks
  rendered ahead hear up to a render-ahead of audio before the jump that is never played, as in
  REAPER. After a jump or a switch the callback waits up to one and a half blocks for the
  worker (an ALSA ring of three periods absorbs that) before rendering every track itself.
  REAPER's per-track
  options ("Allow on tracks without FX", "with open MIDI editors", "in touch/latch/write
  automation mode"), the render-ahead for open FX windows and the media buffer settings are
  not followed; "Allow live FX multiprocessing" is kept in Scythe's own key `scythe_livefxmp`
  (REAPER's bit for it in `workrender` is not known) and is on by default, where REAPER's is
  off. Tracks wait for a whole dependency level
  (all tracks at the same depth) rather than only for their own sources. The track thread count is set by `SCYTHE_TRACK_THREADS`, not in
  the preferences. The performance meter has no CPU graph, media (disk) columns or per-track
  "prevent anticipative FX" flags; the NAM slot reports no separate model cost. NAM models trained at another rate than the
  project's run at the project's, as REAPER's plugin runs them, so a 48 kHz model in a 44.1 kHz
  project sounds a little different from the model at its own rate.
- Audio device page: JACK's MIDI port counts (`jack_midiins`, `jack_midiouts`) and "Auto-connect jack MIDI
  channels" are kept but not used: Scythe's MIDI goes through the ALSA sequencer, which JACK and
  PipeWire bridge. "Auto-suspend PulseAudio" (`linux_auto_pasuspend`, REAPER running its ALSA
  device under `pasuspender`) is not done; on PipeWire systems the system default (PipeWire's
  ALSA `default` device, or the PulseAudio system) is the way to share the hardware, and a card
  PipeWire holds is waited for (see below). The ReWire option of "close audio device when
  inactive" does not apply (no ReWire on Linux), nor does "Virtual loopback audio hardware
  channels". RT priority applies to ALSA as REAPER's ALSA page has it; with JACK the server sets
  it, and PulseAudio streams get Scythe's default promotion. REAPER greys Dummy Audio's channels
  at 0 in and 2 out; the control API lets Dummy have more outputs for routing a large rig without
  hardware. A device missing at start leaves the audio closed with REAPER's "Error opening
  devices" window (Retry, Audio device, Close); unlike REAPER the engine also looks for it again
  every five seconds, as for a device lost while playing. That monitoring and recording share
  one capture stream was checked on a private PulseAudio server (one source output while both
  run); no ALSA setup that refuses a second capture open was available without kernel modules,
  so a `hw:` device was not tried.
- ALSA device lists: REAPER's input and output combo boxes are editable (any ALSA name can be
  typed, `null` say); Scythe's window lists only the cards and the system default, and other
  names go through `--output-device`, `[audio]` or `audio.set_device`. On first start REAPER
  7.73 picks JACK (and, with no JACK server running, opens nothing); Scythe opens the system
  default (PipeWire's or PulseAudio's default device) instead and says so in the window and the
  pill, so a fresh install plays without setup. On JACK the server's period count is not known
  and the latency figure assumes two. The
  busy-device wait was tried with an ALSA PCM of a private configuration made to refuse opens
  with `EBUSY` (an `LD_PRELOAD` shim) and in unit tests, not with a card PipeWire holds; the
  holder's name comes from `/proc/asound/cardN/pcmMp/subS/status`, which the kernel fills only
  while the PCM is open. Rates that a device does not run at are not offered once it has been
  asked; when it cannot be asked (busy and not a USB interface) REAPER's rates are offered and
  the nearest supported rate opens. The PulseAudio path's smaller buffer (two blocks, 512 frames
  when none is set) was not measured: the audio backend's own PulseAudio client is refused with
  "protocol error" by a private PulseAudio 17 server, so only its request is tested, not what a
  server makes of it.
- Media explorer, project bay and themes: MIDI files are listed and inserted but not previewed;
  the preview plays through the device directly (REAPER's preview output and its "start on
  bar" are not there); metadata is not written back to files, and REAPER's custom columns
  (`USER` lines), loudness columns, waveform and peak display of the preview, image metadata
  and the explorer's folder-tree view are not there. Folder favourites are Scythe's own list
  (`explorer-favorites.txt`; REAPER keeps them elsewhere). The bay has no folders, comments,
  "retain" or move/copy of source media. A theme recolours Scythe's 16 palette colours where
  they are painted, so an element drawn in a colour outside the palette keeps it, and colours of
  a REAPER theme without a Scythe counterpart (meters, MIDI editor, routing, groups) are not
  used.
- Envelopes and automation items: a bezier point at the wrap of a looped item (the last pool
  point bending into the next pass) takes other tangents in REAPER (renders differ by up to
  0.02-0.04, `Envelope_Evaluate` by 0.01-0.02); an empty pool's end ramp, REAPER's options to
  connect items to the underlying envelope or bypass it outside items (`POOLEDENVATTACH`, kept)
  and muted items are not followed; at an item's very first frame REAPER sometimes leaves the
  ramp out (1e-5). LFO shapes are Scythe's own points: REAPER keeps only the dialog settings
  (`LFO`) and the points it generated, so its generator could not be compared. Razor edits on
  envelopes are not there; the lanes dock under the arrange view rather than under each track,
  take envelope lanes show for the selected item only, and the tempo lane does not edit time
  signatures (the tempo marker dialog does). REAPER has an automation mode of its own only for
  sends (`I_AUTOMODE`), which Scythe follows; other envelopes use their track's.
- Mix analysis and reference matching (`analysis.*`): tonal balance is matched with ReaEQ
  shelves and peaks only (no linear-phase or ReaFIR matching), loudness and dynamics come as
  advice rather than applied processing, and the measurements are checked against synthetic
  signals with known answers (EBU Tech 3341/3342 style sines, pink and white noise), not
  against another meter. `script.run` needs the desktop app (the headless server has no
  ReaScript host) and stops scripts that `defer` after their first pass. MCP sessions over HTTP
  only tell agents apart for the agents window: the tool selection of `/mcp?tools=…` and
  subscriptions are shared by every client of that address. The agents' pause judges commands by
  their action's words (`list`, `get`, `probe`… read), so a command that reads under another name
  is refused while paused; MCP progress notifications are not shown (`set_status` is).
- Editing: REAPER's detector for transients is not reproduced exactly (see the table in
  [verification.md](verification.md)), and the dynamic split dialog's gate and "split at" options other than transients are
  not there. Snapping to item edges and markers is a window setting (REAPER keeps its snap
  targets in bits of `GRID` that were not measured); the framerate grid snaps like a plain grid.
  Ripple of edge trims was not measured with the mouse (REAPER's trim actions and nudge
  dialog do not ripple, nor do Scythe's); with a beats timebase REAPER moves items again after
  a ripple delete moves the tempo map, which Scythe does not. Envelope points move with items
  as REAPER's do; automation items do not move along. A ripple move with a locked item later
  on moves the items before it one by one, as REAPER does; points between those items stay.
  Track media/razor edit grouping and razor copies of envelope areas are not there (REAPER's
  split does not crossfade by default, nor does Scythe's).
- MIDI editor: no notation view or MusicXML export, no humanize or quantize dialog settings read from REAPER (humanize is random, so
  Scythe's timing and velocity amounts are its own), and REAPER's quantize of same-key notes
  left overlapping (it merges notes landing on one start, which Scythe does) is the only
  overlap rule. Pooled MIDI is not edited as a pool. The inline editor draws notes only (no lanes). Of
  REAPER's left-drag modifiers in the piano roll the default ones are there; the MIDI editor's
  mouse modifier preferences (`MM_CTX_MIDI_*`) are not read, and Ctrl+Shift+Alt on empty grid
  draws a note. A line of notes painted past a looped source's end lengthens the source.

- Recording: "Output (MIDI)" records the messages the track's effects put out, at the block's
  timing (not compared with REAPER live). Touch-replace replaces notes between the first and
  last note played and each controller lane while it is played, until `midicctouchtimeout`
  (1000 ms) passes without a message. The MIDI overdub and replace modes follow REAPER's
  descriptions and were not recorded in REAPER for comparison (that needs MIDI played into it
  live). A
  track records one file per pass group; "start new files on loop", record formats other than
  24-bit WAV (`<RECORD_CFG`) and the secondary record path are kept but not acted on. File
  names use UTC. "New recording adds lanes in layers" plays like "exclusively" (REAPER saved
  the same `LANESOLO` for both); "record into first available lane" versus at the bottom
  follows `C_LANESETTINGS` &16 only. Comp areas cannot be moved or have their edges dragged,
  and "delete comp area edge", "move comp area up/down" and the comp lane's own edits copied
  back to sources are not there; touching swipes (no overlap) do not cross-fade, as in REAPER.
  Take explode/implode/crop are not there yet. REAPER's project `AUTOXFADE` line is kept;
  Scythe takes the lane options from the preferences (`autoxfade`).

- Opus and WavPack items: an item that starts inside an Opus file (`SOFFS`, or playback started
  past its start) is decoded by REAPER from a fresh decoder 80 ms (3840 samples) before the
  point, so its first ~200 ms differ from Scythe's continuous decode by up to -42 dB (measured:
  REAPER starts at the packet holding the target minus 3840); WavPack's `.wvc` correction file,
  which REAPER applies, is ignored (FFmpeg plays the lossy part), and WavPack needs `ffmpeg` on
  the path. Render sources: razor-edit bounds and marker bounds (6, 7), multichannel and mono
  stem options (4, 16), pre-fader stems (8192), "only channels sent to parent", forced channel
  counts in the region matrix, render tail, second pass and "add rendered items" are read but
  not acted on; a stem bypasses the master and its folders but keeps the project's solo states.
  Region selection is not kept, so selected-region bounds render every region (as REAPER's
  `-renderproject` does). Project tabs: background projects do not play ("Run background
  projects" is off, REAPER's default) and tabs are not restored on start-up.

- Control surfaces: on the FaderPorts, Bank, User, Punch, Mix, Proj and Trns (classic) and
  Bypass, Link, Click, Section, Marker and the encoder push (v2) do nothing (REAPER's were not
  seen to change anything in the probe), Output switches the strip to the master, and the v2's
  pan and volume steps were measured with Shift latched; HUI is
  partial (no assignment, cursor, keypad or edit zones). MIDI device numbers count the
  system's ports as Scythe sees them, not REAPER's `reaper-midihw.ini` order, and a surface's
  input port is also opened for MIDI recording. OSC has no FX parameters, ReaEQ, instrument,
  play rate, scrub, zoom/scroll, virtual keyboard or "last touched" patterns; meters use 12
  steps over 60 dB (MCU, HUI) and a position over 60 dB (OSC), not measured against REAPER
  (which needs playing audio). The `OSC` flags' bit 4 (learn) is kept for action and FX learn.

- Track groups: REAPER remembers where a follower would be past a limit (a pan pushed beyond
  hard right comes back from there; a fader taken to -inf and back returns to where it was);
  Scythe clamps each edit, so after hitting a limit followers keep their clamped offset.
  The global grouping override (`GROUPOVERRIDE`) and media/razor edit grouping are kept in the
  project but not acted on. Safe solo (`I_SOLO` 5 and 6) is kept but plays
  as plain solo; the master's pan mode (`MASTER_PANMODE`) is kept, the master pans as before.
- Folders and selection: the drop depth comes from where the pointer is over a track (thirds),
  not from its horizontal position; track heights per track, hiding tracks in the track view
  or mixer (`SHOWINMIX`), the mixer's "folders on the left" layout and "Move tracks to folder..."
  (42786, a dialog) are not there; selection grouping covers the mixer's fader and pan,
  the header switches, record input and monitoring in the inspector and send levels to the same
  destination (not send pan or mute, nor hardware outputs).
- MIDI sync: REAPER's routing-window defaults for new sends are not measured; Scythe's new sends
  carry no MIDI (`CreateTrackSend` from a script carries all, as REAPER's does). The meaning of
  `SMPTESYNC`'s input field was not measured (REAPER's sync dialog needs a screen): Scythe reads
  it as `I_RECINPUT`-style (4096 + device × 32 for MTC, else an audio channel for LTC); the
  skip and look-ahead fields are kept but not used. Chasing MIDI clock is Scythe's own (REAPER
  does not chase clock) and follows song position, not tempo. LTC chase reads 24/25/30 fps and
  needs the audio input open. REAPER's first MTC quarter-frame run after a start carries a
  stale seconds nibble; Scythe's does not. NTSC LTC rates are not sample-exact. Input
  quantize and the "faders send MIDI volume/pan" flag (kept, not acted on) are not there.
- Parameter modulation: the random LFO shape has its own generator (REAPER's is not
  reproducible); renders that start off the 512-frame grid change modulated values at other
  frames than REAPER (whose blocks start at the render start); links across effects are
  block-exact in renders, which then run 512 frames at a time, but live playback with device
  buffers off the grid can be a block late; MIDI links read bus 0 only; master FX are not
  modulated. Learn: "only when the FX window is focused/open" needs the effect's window, which
  Scythe does not track yet, so such bindings stay inactive; OSC action shortcuts are Scythe's
  own `SCYTHE_OSC` lines (REAPER's OSC shortcut format was not measured); a learned control
  reacts on the session's thread (a frame, not a block); relative modes step 1/762 of the range
  per count, as measured.

- Marker actions follow the SWS extension's rules, read from its source (MIT): REAPER 7.73 does
  not advance playback without an audio device and the test setup has no SWS, so neither when
  SWS stops (it polls on a UI timer, tens of milliseconds after the marker) nor its other
  quirks were measured. Unlike SWS, a marker at the spot playback starts from or wraps to does
  not run, and markers just before a loop end are never skipped. The on/off option is not saved
  (SWS keeps `MarkerActionsEnabled` in reaper.ini).
- Count-in and pre-roll are modelled on REAPER 7.73 and not measured against it yet (REAPER does not
  play without an audio device): no count-in with the
  metronome disabled, the count-in before the pre-roll, "Start count-in at start of measure"
  (`preroll` &8) and "Start pre-roll at start of measure" (stored inverted as `preroll` &4), a
  start within a tenth of a bar of the next bar line moving onto it. REAPER rounds a count-in
  up to whole audio blocks; Scythe's is sample-accurate. REAPER's "Record audio during
  pre-roll" is not there: a pre-roll's audio and MIDI are left out of the take (MIDI only
  roughly when a repeat range overlaps the pre-roll). A setlist's count-in (Scythe's own)
  clicks with the metronome off. The click is Scythe's own synthesised one at the metronome's
  primary volume; REAPER's click sounds, patterns and secondary volume are kept in the project
  but not played.

- FX chain graph: a bypass envelope on a container (rather than an effect) is not followed, nor
  REAPER's FX oversampling. Take FX play the item's last sample where REAPER's take-FX path
  drops it for some positions, and their state is not reset on seeking. Input FX process the
  monitored input only: recordings are taken before them, where REAPER records through them.
  The monitoring chain's file is Scythe's FX chain text; REAPER's own `reaper-hwoutfx.ini` was
  not available to compare with. Freeze renders only the track's own items (not what other
  tracks send it or folder children), in stereo; REAPER's mono and multichannel freezes and
  "freeze to the last FX" are not there, and a freeze needs the project saved first.
- Guitar rigs: switching a stage on or off crossfades over 10 ms (Scythe's own; a stage with
  latency still switches at once); `rig.preview` and `nam.preview` try local files only (the
  TONE3000 previews are the desktop app's); the impulse
  response's length and trim are not offered (ReaVerb's trim is not modelled); the EQ bands'
  widths are set in ReaEQ's editor. REAPER here has no NAM LV2 plugin, so the rig renders compare with the NAM slot
  empty (REAPER passes a missing plugin through).
- NAM models at another rate than the project
  are converted with the 192-point sinc; the plugin's own converter is not matched (no reference). Hosted plugins are tested with the workspace's own test plugins.
- Multichannel: master channels go to the audio device's outputs in order, as many as the
  device offers; channels beyond that reach only multichannel renders. That is REAPER's
  default master output for a stereo master; a master with more channels and REAPER's default
  `MASTERHWOUT 0 …` plays only 1/2 in REAPER. Once the master's outputs differ from the
  default they are followed exactly.
- Hardware outputs cannot be rendered by REAPER, so their mixing is checked against sends
  (which null) and unit tests; their envelopes, `B_MONO` (REAPER does not save it) and
  ReaRoute (`I_DSTCHAN & 512`) are not played. A pre-FX tap is not delayed for the latency of
  the track's own effects. The monitor-mix limiter is REAPER's stock JSFX Master Limiter, which
  needs REAPER's effects folder.
- FX presets: VST 3 `.vstpreset` files are looked for only where REAPER 7.73 lists them
  (`~/.vst3/presets`), not in the plugin bundle or system folders. REAPER counts a CLAP
  plugin's discovered presets after its own (`TrackFX_GetPresetIndex`) and loads them by name
  (`TrackFX_SetPreset`), but not by index or by navigating, so where they sit in its preset
  menu is unverified; only plugin-internal presets were tested, not preset files.
- Actions and customisation: about 150 of REAPER's ~4300 main-section actions have a Scythe
  counterpart; `KEY`, `ACT` and `SCR` lines of other sections (MIDI editor, Media Explorer) are
  kept but only the main section's run. Custom actions are SWS-free macros; SWS cycle actions
  are not there. Toolbar files follow REAPER's `item_N`/`icon_N`/`tbf_N`/`title` keys (from
  REAPER's strings and shared toolbar files) but were not round-tripped through a REAPER-written
  `reaper-menu.ini`, since REAPER writes one only from its customisation dialog; menus are not
  customisable. Mouse modifiers run actions assigned to clicks and double clicks of items,
  track lanes and headers and the ruler; REAPER's drag behaviours (`13 m` …) are kept but not
  played, and `GetMouseModifier` answers nothing for contexts left at their defaults. Window sets
  are Scythe's own (`scythe-screensets.ini`), not REAPER's window layouts. No `.ReaperTheme`
  colours, and no extension SDK (`reaper_plugin.h` C ABI), so SWS and other extensions do not
  load. Python was not cross-checked for the action and chunk calls.
- ReaScript covers part of the API (see [features.md](features.md#scripting)); Python needs `python3` on the PATH. FX
  containers, take FX, hardware outputs, the MIDI editor's API and the arrange view are not there. `extension_api`
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
- Dynamics: ReaComp's auto release with a release under 4 ms (REAPER then floors the release
  near 2.5 ms in a way not modelled) is within 5e-3, ReaLimit's "inf" release within 1.8e-2, and
  ReaComp's deprecated anti-alias setting (2 and up, no longer in REAPER's UI) is ignored.
  ReaLimit's true-peak flag (bit 0 of the word after the version) is read but not modelled.
  ReaGate's noise is white noise of the same level and spectrum from Scythe's own generator, so
  it cannot null. Multichannel modes of these plugins (beyond the stereo pair and a sidechain)
  and gain-reduction metering are not there. ReaGate and ReaComp states written by earlier
  Scythe versions carry a header REAPER ignores; they get REAPER's header once edited.
- Plugins that are not installed as Linux plugins (ReaTune, the Windows/macOS VST 3s in the
  corpus such as Amp Room) pass audio through; the inspector says so per slot.
- Plugin scanning: REAPER keeps its CLAP cache in `reaper-clap-linux-x86_64.ini` (a section per
  file with two file times, then `id=instrument|name (vendor)`) and reads LV2 anew at every
  start (about 2 s for LSP Plugins); Scythe keeps its own JSON caches instead, since they also
  hold features, ports and failures, and does not read or write REAPER's. A project that uses a
  plugin the cache does not know yet waits for the running scan when it opens (at most 2
  minutes), as REAPER scans before it opens a project. LV2 bundles are read in Scythe's own
  process (they are only text), and there is no list of failed plugins in the window yet:
  `fx.catalog` reports them.
- Stock plugins: ReaSynth's portamento (REAPER then glides monophonically) is ignored, and a
  decay towards a sustain below 0 dB is within 2e-4 (REAPER pauses it for a frame at a block
  boundary in a way not modelled). ReaControlMIDI's two user CC sliders are kept but not sent,
  and its scale type comes only from the stored state. ReaFIR's two-point curves and steep
  many-point curves are within 2e-4 of REAPER's response (6e-3 on program material), FFT sizes
  other than 2048 to 8192 within 3e-4; its gate, compressor, subtract and convolve modes and
  minimum-phase processing are not emulated. ReaVerb's jittered echoes and reverb generator
  follow REAPER's own random sequence, and its filter, normalise and channel tool modules are
  not decoded. ReaInsert's ping-detected delay takes effect when the chain is next rebuilt (an
  edit or reload), and the ping is a -6 dB click detected above -20 dB. Not emulated (Scythe
  hosts REAPER's own Linux binaries when they are on the VST path, otherwise the slots pass
  audio through): ReaPitch (its default algorithm is the project's, élastique), ReaTune,
  ReaVocode, ReaVoice, ReaSynDr (its snare's noise differs between REAPER's own renders, so it
  cannot null), ReaSurround/ReaSurroundPan, ReaStream, ReaNINJAM and ReaCast (network
  clients). ReaNINJAM (playing along with others over the internet through a NINJAM server,
  intervals of encoded audio) and ReaCast (streaming the mix to an Icecast/Shoutcast server or
  recording it) are network services rather than audio processing and are out of scope.
- The master play-rate envelope (`MASTERPLAYSPEEDENV`) is kept but not applied; REAPER evaluates
  it per block. FX parameter envelopes are not retimed by the project play rate or by tempo
  edits. Resampled stretch markers at play rates other than 1 and sloped markers do not null
  (see [verification.md](verification.md)). Track timebase has no API or UI yet.
- Stretched and pitch-shifted takes do not null against REAPER. élastique (REAPER's default) is
  proprietary and REAPER's simple windowed, Rrreeeaaa and ReaReaRea modes are its own, so they
  use Scythe's phase vocoder; SoundTouch and Rubber Band come from the system's libraries, newer
  than REAPER's (see [verification.md](verification.md)). Shifters process the file from its start, not from the item's source
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
- CD image and DDP renders are always 44.1 kHz. Render metadata covers the tags listed in [features.md](features.md#playback-rendering-and-formats);
  others (ID3 pictures and non-text frames, `IXML:USER:…`, `CART:` timers, ASWG, the XMP keys
  beyond `dc/title`, `dc/description` and `dc/rights` as text elements) are left out, and
  which wins when both `BWF:Description` and the project-file option are set is a guess. A
  forced Wave64 render with the tempo option leaves out the ACID chunk (REAPER writes a
  malformed one there). GIF frames of video items get exact colour tables only up to 256
  colours (more are mapped to the nearest of the first 256, not REAPER's palette), and their
  colours come from FFmpeg's decoder, a few levels off REAPER's; GIF transparency is only
  matched for a single-colour frame.
- Video renders: video processors other than the three presets in [features.md](features.md#playback-rendering-and-formats) (text and timecode
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
