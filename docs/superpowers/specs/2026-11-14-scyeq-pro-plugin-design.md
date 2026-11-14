# ScyEQ-Pro as a CLAP and VST3 plugin

Date: 2026-11-14. Owner: Thomas. Status: design, awaiting approval (review questions answered).

## Intent

ScyEQ-Pro is Scythe's own 24-band dynamic equalizer (DSP in `crates/scythe-engine/src/scythe_eq`,
editor in `crates/scythe-app/src/ui/fx_editor/scythe_eq.rs`, `analyzer.rs`, `eq_match.rs`,
`model.rs`). Scythe saves it in `.RPP` projects as a CLAP slot, `<CLAP "CLAP: ScyEQ-Pro (Scythe)"
scythe.eq "">` with its state in `<STATE` (older projects: `CLAP: Scythe EQ (Scythe)`). REAPER keeps
that block but cannot play it, so a Scythe project that uses ScyEQ-Pro sounds different in REAPER.

Thomas exchanges projects with REAPER users (bandmates, other mixers). ScyEQ-Pro becomes a plugin
they install, so such a project opens in REAPER with every ScyEQ-Pro playing, sounds the same as in
Scythe, can be edited there with the same editor and features, and comes back to Scythe intact.

Decided in conversation on 2026-11-14:

- Purpose: exchanging projects with REAPER users. They must be able to do everything with the
  plugin that Scythe users can, with the same editor ("exactly the same").
- All platforms: Linux, Windows, macOS. CLAP and VST3.
- Extensive testing; the bar is happy users.
- Cross-track features work within one process: instances in one process find each
  other through shared memory in the plugin library; track names come from the host.
- The plugin layer is the one written for nx-plugin this week (`crates/nx-format` on nx-plugin's
  master, which replaced nih-plug), moved to a repository of its own and shared by nx-plugin and
  Scythe. nih-plug is not used (its VST3 export is GPLv3).

Success: every ScyEQ-Pro setting renders the same samples in REAPER (with the plugin) as in Scythe,
measured per instance on test projects that hold only ScyEQ-Pro (see Testing); Song 2
(`~/music/the_heritance/mix-test/Song 2/Song 2.RPP`, 30 instances) opens in REAPER on each
platform with every instance playing, a band changed in REAPER's editor is there when Scythe opens
the saved project, and back. A whole-mix null of Song 2 is not the measure: its other effects and
Scythe's mixing have their own comparisons with REAPER (`docs/verification.md`).

## Goals

1. A ScyEQ-Pro plugin, CLAP and VST3, for Linux x86_64, Windows x86_64 and macOS (universal:
   arm64 and x86_64).
2. Project exchange:
   - CLAP id `scythe.eq`, so REAPER binds the plugin to the `<CLAP … scythe.eq` slots Scythe
     already writes, including the ones titled `Scythe EQ`.
   - The state the plugin saves is byte for byte what Scythe saves for the same settings, and
     either side loads the other's.
   - The same settings give the same samples (one DSP crate underneath both).
3. The same editor as in Scythe, from the same code: curve and band handles, analyzer, piano
   strip, band panel, dynamic and spectral bands, character, auto gain, output, phase modes,
   EQ Match (against another instance or a reference file), the instance list, collision display,
   undo of editor edits through the host.
4. Instances find each other in one host process: names and colours of their tracks (CLAP
   `track-info`, VST3 `IInfoListener`), their spectra for comparing and matching, the instance
   list with in-place editing of another instance's bands.
5. Host integration a REAPER user expects: every setting a parameter (automatable where it makes
   sense), editor edits as parameter gestures so automation writing and REAPER's undo see them,
   a stereo sidechain input bus for external sidechain bands, reported and changed latency for the
   linear-phase and spectral modes, a resizable editor that follows the host's scale.
6. Scythe itself keeps running ScyEQ-Pro in process (not through the plugin), with the editor
   from the shared crate.
7. The shared plugin layer, `nx-format` moved and extended, serves both nx Stomp and ScyEQ-Pro;
   nx-plugin switches to it with no change in behaviour.
8. Release builds of the plugin for the three platforms, in the release workflow that is still to
   be written, as zips with install instructions.

## Non-goals

- AU, AAX, LV2.
- Installers (MSI, pkg) for the plugin in the first version; zips and instructions.
- Cross-track features between instances in different processes (REAPER's "run as separate
  process", Bitwig's sandbox). Each instance then works on its own.
- Seeing tracks that have no ScyEQ-Pro on them from inside REAPER; Scythe can, the plugin cannot.
- Changing ScyEQ-Pro's sound or features while moving it, with one exception (see Decisions after
  review): the 32-frame processing grid becomes independent of block sizes.

## Anti-goals

- A project that opens in REAPER and sounds different, or comes back to Scythe changed.
- Crashing or hanging the host: every exported entry point checks its pointers, catches panics,
  never blocks the audio thread.
- `process` allocating, locking a contended mutex or doing I/O, in Scythe or in the plugin.
- A GPL-family dependency in the plugin or the shared layer.
- Two copies of the editor or the DSP that drift apart.

## Constraints

- Rust, egui 0.37 (Scythe's and nx-edit's version), `clap-sys` 0.5, `vst3` 0.3 (MIT/Apache,
  generated from Steinberg's MIT headers), `baseview` 0.4 and `egui-baseview` 0.9 (MIT/Apache;
  baseview supports X11, Win32 and Cocoa).
- The state format is Scythe's (`scythe_eq::params::encode`/`decode`); it does not change.
- Scythe's licence (MIT OR Apache-2.0) for the plugin and the DSP and editor crates; the shared
  layer gets the same licence so both projects can use it.
- Conventional commits without co-author lines; Thomas confirms every push.

## Approaches

- **A. Shared plugin layer in its own repository, extended for ScyEQ-Pro (chosen).** `nx-format`
  leaves nx-plugin for a repository of its own; Scythe and nx-plugin depend on it by git tag.
  What ScyEQ-Pro adds (macOS, float parameters, sidechain, track info, gestures, resizing) lands
  there, so nx Stomp gets it too and fixes happen once. Cost: the two projects share a dependency
  whose changes must keep both working.
- B. Copy `nx-format` into Scythe and grow it there. Quicker start, but two copies that drift;
  rejected for that.
- C. nih-plug. Rejected: its VST3 export makes every VST3 build GPLv3, and nx-plugin just left it.
- D. A new plugin layer from scratch. Rejected: `nx-format` exists, works and is tested.

## The how, as far as decided

### Repositories and crates

- **`plugin-format`** (new repository `tluijken/plugin-format`, crate `plugin-format`, MIT OR
  Apache-2.0): `nx-format` with its history (`git filter-repo --subdirectory-filter`), tagged
  releases. nx-plugin depends on a tag and drops its copy. The name is the designer's call.
- In the Scythe repository:
  - **`scyeq-dsp`**: everything in `scythe-engine/src/scythe_eq` plus the parts it borrows today
    (`ParamInfo`, the probe it publishes analyzer data into). No cpal, no project model, no egui.
    `scythe-engine` wraps it in its `Effect` as now.
  - **`scyeq-ui`**: the editor (`scythe_eq.rs`, `analyzer.rs`, `eq_match.rs`, `model.rs` and the
    widgets and theme they use), on egui only, talking to its surroundings through one trait (below).
    `scythe-app` uses it and implements the trait on the app.
  - **`scyeq-plugin`**: the plugin (`cdylib`), on `scyeq-dsp`, `scyeq-ui` and `plugin-format`;
    implements the trait on the plugin and the instance registry.

### The editor's surroundings (`scyeq-ui`)

One trait the editor calls for everything outside the EQ itself: read and write settings (with
gesture begin/end around drags, one undo step per edit), the probe for the analyzer, the other
instances (name, colour, spectra, settings, and writing their settings), the track to compare with,
loading a reference file, and view state (instance list open, sizes). Scythe implements it on its
session (any track can be compared, as now); the plugin on the host and the registry.

### Shared layer additions (`plugin-format`)

- macOS: Cocoa parent windows (CLAP `cocoa`, VST3 `NSView`), the `.clap`/`.vst3` bundle layouts with
  `Info.plist`, `bundleEntry`/`bundleExit`.
- Parameters with continuous values and a mapping between plain and normalised values (linear,
  logarithmic for frequencies, stepped, lists), value-to-text and text-to-value; hundreds of them.
- Editor-originated parameter changes with gestures: CLAP `params` output events from `flush` and
  `process` plus `request_flush`; VST3 `IComponentHandler::beginEdit/performEdit/endEdit`.
- Per-block parameter changes from the host as now, applied at the start of the block; the
  smoothing ScyEQ-Pro already does handles the steps.
- Audio ports: a stereo main bus and an optional stereo sidechain bus (CLAP `audio-ports`, VST3 an
  aux input bus).
- Track info: CLAP `track-info`, VST3 `IInfoListener`, delivered to the plugin when they change.
- A resizable editor with a minimum size, following the host's scale (CLAP `gui.set_scale`, VST3
  `IPlugViewContentScaleSupport`, macOS backing scale).
- Latency changes as now (`latency.changed`, `restartComponent(kLatencyChanged)`).
- A state-changed signal when the editor changes something that is not a parameter, so the host
  marks the project dirty (CLAP `state.mark_dirty`, VST3 `restartComponent` or a parameter touch).

### Parameters

Every stored value of `scythe_eq::params` (24 bands of `BAND_STRIDE` values plus the globals) is a
parameter with a fixed id equal to its index in the state table, so ids never move. Band shape,
slope, placement and on/off are stepped or list parameters. Which ones are automatable is the
builder's call, following what Scythe allows.

### Instance registry

A process-global list in the plugin library: each instance registers on creation and leaves on
destruction, publishing its track name and colour, its spectra (from the same probe Scythe's
analyzer reads) and a handle to read and change its settings from another instance's editor (the
change goes through the owning instance as a parameter gesture). Lock-free on the audio side; the
editors poll it at frame rate.

### Platforms and builds

- Linux and Windows build as nx-plugin does today (Windows also cross-built with MinGW); macOS
  builds natively in CI as a universal binary (`lipo`), ad-hoc signed.
- CI (GitHub Actions, free once the repository is public) builds and tests the plugin on all three
  platforms and uploads the bundles as artifacts; the release workflow attaches them as zips.

### Testing

The bar is that nobody exchanging a project is surprised. In layers:

1. **Unit and crate tests**: parameter mapping (every id, plain/normalised/text round trips), state
   round trip (Scythe-written state loads in the plugin and saves back identically, and the
   reverse), the registry with several instances, gesture ordering.
2. **Host tests in the repositories**: the CLAP and VST3 test hosts from `nx-format`, extended:
   load, scan, sidechain bus, a parameter event mid-block, state save/load, latency change,
   editor open/close repeatedly (headless where the platform allows), on all three platforms in CI.
3. **Validators in CI on all three platforms**: `clap-validator` and Steinberg's VST3 `validator`.
4. **Scythe hosting the plugin**: Scythe's own CLAP and VST3 hosts load the built plugin; a test
   renders a project with the built-in ScyEQ-Pro and with the plugin and requires a null (exact),
   for every band shape, dynamic and spectral bands, the sidechain and each phase mode; and a
   real-time allocation check on the plugin's `process` (Scythe has one, `probe_alloc`).
5. **REAPER, automated where it can be**: REAPER renders from the command line; a script renders
   ScyEQ-Pro-only test projects in REAPER with the plugin and compares them with Scythe's renders
   (measures in Decisions after review), and opens Song 2 to check that every instance plays. Run on
   Linux locally and on Windows in the VM; on macOS when a Mac is available.
6. **By hand with Thomas**, per platform: install from the zip, open Song 2 in REAPER, edit in the
   editor, automate a band, undo, sidechain from another track, EQ Match against another instance,
   the instance list, save, open in Scythe, and back; HiDPI and window resizing.

## Left to the builder

The exact trait boundaries in `scyeq-ui` and `plugin-format`; how much of the theme and widget code
moves with the editor; the parameter display texts; which parameters are automatable; the VST3
class id (fixed once chosen); the order of the work (a sensible order: move the layer and switch
nx-plugin, split the DSP crate, split the editor crate, the plugin on Linux, then Windows, then
macOS, with tests growing at every step); the CI layout.

## Decisions after review (designer's calls)

The builder check raised four questions; three are decided here, the fourth by Thomas.

1. **What has to match.** `ScytheEq::process` restarts its 32-frame grid (`SUB`) on every call, so
   dynamic and spectral bands give different samples at different block sizes, and REAPER's blocks
   differ from Scythe's. The grid moves to the absolute sample position (carried across calls), so
   the output no longer depends on block sizes. This changes Scythe's own output of dynamic and
   spectral bands by a tiny amount, once; it is the only sound change allowed. The measures:
   - Scythe hosting the built plugin against its built-in ScyEQ-Pro, same machine: bit-exact for
     fixed settings at any block size.
   - REAPER with the plugin against Scythe, per platform, on test projects holding only ScyEQ-Pro
     instances: within -120 dBFS peak difference for fixed settings (f64 maths may differ in the
     last bits between compilers and operating systems). Automation is compared on a fixed
     512-frame block size in both hosts.
2. **VST3 projects.** CLAP is the exchange path and the install notes say so. Scythe also
   recognises a ScyEQ-Pro VST3 slot (by its class id) and runs it as its built-in EQ from the state
   inside REAPER's VST3 chunk, so a project where someone picked the VST3 still comes back whole.
   CLAP and VST3 instances in one process do not see each other (separate libraries); the install
   notes recommend one format per project. No cross-library shared memory.
3. **Versions.** The plugin is built from the Scythe repository and released together with Scythe,
   under the same version. The state keeps values it does not know: `decode` holds on to values past
   `TOTAL` and `encode` writes them back, so an older plugin round-trips a newer Scythe's state
   without losing anything. When the state's version is newer than the plugin's, the editor shows a
   notice asking to update; the sound uses what the plugin knows.
4. **macOS release (Thomas, 2026-11-14).** macOS ships as a beta: built and tested in CI (host
   tests, validators), universal and ad-hoc signed but not notarized, so users allow it once in
   System Settings; the install notes say how. Notarization with an Apple Developer account and a
   hand test on a real Mac come later, if the project grows.
