# ScyEQ-Pro Plugin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** ScyEQ-Pro as a CLAP and VST3 plugin for Linux, Windows and macOS that REAPER users install, so Scythe projects using it play, sound and edit the same in REAPER and come back intact.

**Architecture:** nx-plugin's `nx-format` becomes the repository `plugin-format` and grows what ScyEQ-Pro needs (continuous parameters, sidechain, transport position, editor gestures, track info, resizing, macOS). In Scythe, the EQ's DSP moves to `scyeq-dsp` and its editor to `scyeq-ui` (behind one trait), and `scyeq-plugin` puts both on `plugin-format`. Scythe keeps running the EQ in process.

**Tech Stack:** Rust 2024, egui 0.37, `clap-sys` 0.5, `vst3` 0.3, `baseview` 0.4, `egui-baseview` 0.9, GitHub Actions (ubuntu, windows, macos runners), `clap-validator`, Steinberg's VST3 `validator`.

**Spec:** `docs/superpowers/specs/2026-11-14-scyeq-pro-plugin-design.md`

## Global Constraints

- CLAP id `scythe.eq`; name `ScyEQ-Pro`; vendor `Scythe`; version = Scythe's workspace version.
- VST3 class id: the 16 bytes `b"ScytheScyEQPro01"` (fixed forever once released).
- Bundles: `ScyEQ-Pro.clap` and `ScyEQ-Pro.vst3`.
- State bytes are `scyeq_dsp::params::encode`'s; the format does not change except that unknown values are kept (Task 11).
- Parameter id = index in the state table (`band * BAND_STRIDE + offset`, globals from `GLOBAL`); ids never move.
- The only allowed change to ScyEQ-Pro's sound: the 32-frame grid follows the absolute sample position (Task 10).
- No GPL-family dependency in `plugin-format` or `scyeq-plugin` (licence test, as nx-plugin has).
- Licences: `plugin-format`, `scyeq-dsp`, `scyeq-ui`, `scyeq-plugin` are `MIT OR Apache-2.0`.
- `process` never allocates, locks a contended mutex or does I/O; every exported entry point catches panics and checks pointers.
- macOS ships as a beta: universal (arm64 + x86_64), ad-hoc signed, not notarized.
- Measures: Scythe built-in vs Scythe hosting the plugin: bit-exact. REAPER with plugin vs Scythe: peak difference below -120 dBFS for fixed settings; automation compared at 512-frame blocks in both.
- Conventional commits, no co-author lines, `Claude-Session:` trailer; never push or create a GitHub repository without Thomas confirming.

## Review Focus

- A project whose ScyEQ-Pro slot is titled `CLAP: Scythe EQ (Scythe)` (older Scythe) loads in the plugin with its settings: test in Task 14.
- Sample rate changing between activations (44.1 kHz project, then 96 kHz) gives the same response at the new rate, no stale filters: test in Task 14.
- Blocks of 0 frames, of exactly `max_block`, and a host calling `process` before any parameter flush: no crash, correct output: tests in Tasks 3 and 14.
- An instance destroyed while another instance's editor lists it or uses it as EQ Match source: the editor drops it without a crash: test in Task 16.
- Opening and closing the editor 50 times on one instance, and two instances' editors open at once: no leak of windows or threads, no crash: tests in Tasks 6 and 15.

---

## Phase 1: `plugin-format` (repository `~/projects/prive/plugin-format`)

### Task 1: The repository, with its history, an example plugin and host tests

**Files:**
- Create: `~/projects/prive/plugin-format/` from `nx-plugin/crates/nx-format` (history kept: `git filter-repo --subdirectory-filter crates/nx-format` on a fresh clone of nx-plugin)
- Create: `Cargo.toml` (workspace: `plugin-format` crate at the root or `crates/plugin-format`, plus `examples/gain`), `LICENSE-MIT`, `LICENSE-APACHE`, `README.md`, `flake.nix` (from nx-plugin's: dev shell and `devShells.windows` with MinGW and Wine)
- Create: `examples/gain/` (cdylib, `export_clap!`/`export_vst3!`): one `Int` gain-in-dB parameter, stereo, no editor content beyond a label
- Move: nx-plugin's `tests/clap_host.rs` and `tests/vst3_host.rs` loaders into `tests/clap_host.rs`, `tests/vst3_host.rs` here, aimed at `examples/gain`; nx-plugin's `tests/licences.rs` into `tests/licences.rs`
- Create: `.github/workflows/ci.yml` (ubuntu, windows: build, clippy `-D warnings`, test)

**Interfaces:**
- Produces: crate `plugin_format` with nx-format's API unchanged (`Info`, `Shared`, `Plugin`, `ParamInfo`, `Kind`, `export_clap!`, `export_vst3!`).

- [ ] **Step 1:** Rename the crate to `plugin-format` (lib name `plugin_format`), update `use` paths, license field `MIT OR Apache-2.0`.
- [ ] **Step 2:** Write `examples/gain` and point the moved host tests at it: `clap_host::loads_scans_and_processes` (one plugin, id `plugin-format.gain`, one parameter, a block at 0 dB passes input through unchanged, at -6 dB scales by 10^(-6/20)); `vst3_host::processes_and_restores_state` (same, plus state save/load round trip).
- [ ] **Step 3:** Run `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`. Expected: pass.
- [ ] **Step 4:** Run the Windows build and host tests under Wine (`nix develop .#windows -c cargo test --target x86_64-pc-windows-gnu`). Expected: pass.
- [ ] **Step 5:** Commit (`chore: plugin-format from nx-format, with an example plugin`). Ask Thomas before creating `tluijken/plugin-format` on GitHub (public, since Scythe is going public and builds against it) and pushing.

### Task 2: Continuous parameters

**Files:**
- Modify: `src/param.rs`, `src/clap.rs` (param info, value text), `src/vst3.rs` (parameter info, normalised conversion)
- Test: `tests/param.rs`

**Interfaces:**
- Produces: `Kind::Float { min: f64, max: f64, map: Map }`, `enum Map { Linear, Log }`; `ParamInfo::{min, max, step_count, clamp, normalise, plain}` handle it (Float: `step_count() == 0`, `clamp` does not round; Log maps `plain = min * (max/min)^n`). `ParamInfo.name` becomes `&'static str` still (generated names are leaked once by the plugin).

- [ ] **Step 1:** Tests: `float_linear_round_trips` (min -30, max 30: `plain(normalise(12.5)) == 12.5`, `normalise(0.0) == 0.5`); `float_log_round_trips` (20..20000 Hz: `normalise(632.455…) ≈ 0.5` within 1e-9, `plain(normalise(1000.0))` within 1e-9 of 1000); `float_clamp_keeps_fractions_and_replaces_nan` (`clamp(1.25) == 1.25`, `clamp(NAN) == default`).
- [ ] **Step 2:** Run, expect FAIL (no `Kind::Float`). Implement. Run, expect PASS.
- [ ] **Step 3:** Extend the gain example with a `Float` (Log) parameter and the host tests: CLAP `get_info` reports no `CLAP_PARAM_IS_STEPPED` for it; VST3 `getParameterInfo` reports `stepCount == 0`; `value_to_text` of 1000.0 is `"1000 Hz"` (the example formats `{:.0} Hz`) and `text_to_value("1000 Hz") == 1000.0`, in both formats.
- [ ] **Step 4:** Commit (`feat: continuous parameters with linear and logarithmic mapping`).

### Task 3: Transport position and a sidechain input

**Files:**
- Modify: `src/lib.rs` (`Plugin::process`, `Info`), `src/clap.rs` (audio ports, transport), `src/vst3.rs` (buses, `ProcessContext`)
- Test: `tests/clap_host.rs`, `tests/vst3_host.rs`

**Interfaces:**
- Produces:
  - `pub struct Block<'a> { pub channels: &'a mut [&'a mut [f32]], pub sidechain: Option<[&'a [f32]; 2]>, pub position: Option<i64>, pub playing: bool }`
  - `Plugin::process(&mut self, block: Block<'_>)` replaces `process(&mut self, channels)`.
  - `Info::sidechain: bool` (a stereo input port/bus named "Sidechain", not main, optional to connect).
  - `position`: CLAP: from `clap_event_transport` when `CLAP_TRANSPORT_HAS_SECONDS_TIMELINE` is set, `song_pos_seconds` (a `clap_sectime`, 2^31 per second) times the sample rate, rounded; VST3 `ProcessContext::projectTimeSamples` when `ProcessContext` is given. `None` otherwise.

- [ ] **Step 1:** Tests: `sidechain_reaches_the_plugin` (example plugin with `sidechain: true` writes `main + sidechain` into the output; host feeds 0.25 main, 0.5 sidechain, expects 0.75; with the sidechain port unconnected expects 0.25) for CLAP and VST3; `position_reaches_the_plugin` (host sends transport at sample 48000; the example exposes the last position through a test-only parameter or shared atomic; expects `Some(48000)`; without transport `None`); `zero_frame_block_is_harmless` (both formats, `frames == 0`, returns ok, no panic).
- [ ] **Step 2:** Run, expect FAIL. Implement. Run, expect PASS. Update nx-plugin's use later (Task 9), not here.
- [ ] **Step 3:** Commit (`feat: sidechain input and transport position in the block`).

### Task 4: Parameter changes from the editor

**Files:**
- Create: `src/gui_context.rs`
- Modify: `src/lib.rs` (`Shared::editor` signature), `src/clap.rs` (output events in `process` and `flush`, `request_flush`), `src/vst3.rs` (`IComponentHandler::beginEdit/performEdit/endEdit`)
- Test: `tests/clap_host.rs`, `tests/vst3_host.rs`

**Interfaces:**
- Produces: `pub struct GuiContext` (cheap `Clone`, `Send`) with `begin(&self, id: u32)`, `set(&self, id: u32, plain: f64)`, `end(&self, id: u32)`, `mark_dirty(&self)`, `request_resize(&self, w: u32, h: u32) -> bool`, `track(&self) -> Option<TrackInfo>` (Task 5 fills it). `Shared::editor(&self, cx: GuiContext) -> Box<dyn FnMut(&mut egui::Ui) + Send>`.
- `set` updates the shared value at once (`Shared::set_param`) and queues the change for the host; CLAP sends `CLAP_EVENT_PARAM_GESTURE_BEGIN`, `CLAP_EVENT_PARAM_VALUE`, `CLAP_EVENT_PARAM_GESTURE_END` from the next `process` or `flush` (calling `host_params->request_flush` when not processing); VST3 calls the component handler on the UI thread. `mark_dirty`: CLAP `host_state->mark_dirty`, VST3 `restartComponent(kParamValuesChanged)`.

- [ ] **Step 1:** Tests with a recording host: `gesture_reaches_the_clap_host_in_order` (editor calls begin(1), set(1, -6.0), end(1) → host receives begin, value -6.0, end for id 1, in that order, on the next flush); `gesture_reaches_the_vst3_host` (component handler sees beginEdit(1), performEdit(1, normalised(-6.0)), endEdit(1)); `mark_dirty_reaches_the_host` (both formats).
- [ ] **Step 2:** Run, expect FAIL. Implement (a lock-free queue of 1024 entries from editor to wrapper; when full, the oldest value for the same id is overwritten rather than dropping a begin/end). Run, expect PASS.
- [ ] **Step 3:** Commit (`feat: parameter gestures from the editor`).

### Task 5: Track name and colour from the host

**Files:**
- Modify: `src/lib.rs` (`TrackInfo`, `Shared::set_track`), `src/clap.rs` (`track-info` extension, `changed`), `src/vst3.rs` (`IInfoListener::setChannelContextInfos`)
- Test: host tests

**Interfaces:**
- Produces: `pub struct TrackInfo { pub name: Option<String>, pub color: Option<[u8; 3]> }`; `Shared::set_track(&self, info: TrackInfo)` with a default empty body; `GuiContext::track()` returns the latest.

- [ ] **Step 1:** Tests: `clap_track_info_reaches_the_plugin` (test host offers `track-info` with name "Bass", colour 255,0,0; after `init` and after `changed()` the example's shared state holds it); `vst3_channel_context_reaches_the_plugin` (attribute list with `ChannelContext::kChannelNameKey` "Bass" and `kChannelColorKey`).
- [ ] **Step 2:** Run, expect FAIL. Implement. Run, expect PASS.
- [ ] **Step 3:** Commit (`feat: track name and colour from the host`).

### Task 6: Resizable editor at the host's scale

**Files:**
- Modify: `src/lib.rs` (`Plugin::EDITOR_SIZE` stays the default; add `const EDITOR_MIN: (u32, u32)`, `const RESIZABLE: bool`), `src/editor.rs`, `src/clap.rs` (`can_resize`, `get_resize_hints`, `adjust_size`, `set_size`, `request_resize`), `src/vst3.rs` (`canResize`, `checkSizeConstraint`, `onSize`, `IPlugFrame::resizeView`)
- Test: `src/editor.rs` tests, host tests

**Interfaces:**
- Produces: the plugin keeps its own window size (in its state if it wants); the layer asks `Shared::editor_size(&self) -> Option<(u32, u32)>` (default `None`) when opening, and calls `Shared::set_editor_size(&self, w, h)` (default no-op) after a resize.

- [ ] **Step 1:** Tests: `adjust_size_respects_the_minimum` (CLAP `adjust_size` of 10x10 returns `EDITOR_MIN`); `vst3_check_size_constraint_clamps`; `editor_opens_and_closes_fifty_times` (Linux under Xvfb in CI, Windows natively: open/close 50 times, thread count back to start, no panic).
- [ ] **Step 2:** Run, expect FAIL. Implement. Run, expect PASS.
- [ ] **Step 3:** Commit (`feat: resizable editor`).

### Task 7: macOS

**Files:**
- Modify: `src/lib.rs` (`export_vst3!` gains `bundleEntry`/`bundleExit` on macOS), `src/clap.rs` (`NATIVE_API` = `CLAP_WINDOW_API_COCOA`), `src/vst3.rs` (`kPlatformTypeNSView`), `src/editor.rs` (baseview's `AppKitWindowHandle` from the `NSView*` parent; the backing scale factor)
- Create: `scripts/bundle.sh` (Linux/Windows/macOS layouts: `X.clap` file or bundle with `Contents/MacOS/X` and `Info.plist`; `X.vst3/Contents/{x86_64-linux,x86_64-win,MacOS}/…`; macOS universal with `lipo` and `codesign --sign -`)
- Modify: `.github/workflows/ci.yml` (macos-latest: build both architectures, bundle, host tests)

- [ ] **Step 1:** Host tests on macOS load the bundles (the CLAP host loads `Contents/MacOS/<name>` and passes the bundle path to `init`; the VST3 host calls `bundleEntry` with a `CFBundleRef`, as Scythe's `vst3_host::mac::with_bundle` does). Expected: the Task 1-5 host tests pass on macos-latest.
- [ ] **Step 2:** `editor_opens_and_closes_fifty_times` runs on macOS in CI (a hidden `NSWindow` as parent).
- [ ] **Step 3:** Commit (`feat: macOS bundles and Cocoa editors`).

### Task 8: Validators in CI

**Files:**
- Modify: `.github/workflows/ci.yml`
- Create: `scripts/validate.sh <bundle dir>` (runs `clap-validator validate <X.clap>` and Steinberg's `validator <X.vst3>`; builds or downloads them per runner and caches them)

- [ ] **Step 1:** CI job on all three runners: bundle `examples/gain`, run `scripts/validate.sh`. Expected: both validators report no failures (warnings listed in the job summary).
- [ ] **Step 2:** Fix what the validators find in the layer.
- [ ] **Step 3:** Commit (`ci: clap-validator and the VST3 validator on every platform`). Tag `v0.1.0` once Thomas agrees to push.

### Task 9: nx-plugin on `plugin-format`

**Files (nx-plugin repository):**
- Delete: `crates/nx-format`
- Modify: `crates/nx-plugin/Cargo.toml` (`plugin-format = { git = "https://github.com/tluijken/plugin-format", tag = "v0.1.0" }`), `src/plugin.rs`, `src/params.rs` (`Block`, `editor(cx)`), `tests/clap_host.rs`, `tests/vst3_host.rs`, `tests/licences.rs`

- [ ] **Step 1:** Switch; nx Stomp ignores `sidechain` and `position`, keeps `RESIZABLE = false`.
- [ ] **Step 2:** Run nx-plugin's full test suite on Linux and its Windows target under Wine. Expected: unchanged results (same asserted behaviour as before).
- [ ] **Step 3:** Commit in nx-plugin (`build: use plugin-format instead of the in-repo nx-format`); push only with Thomas's go.

## Phase 2: ScyEQ-Pro split out of Scythe (Scythe repository)

### Task 10: The 32-frame grid follows the sample position

**Files:**
- Modify: `crates/scythe-engine/src/scythe_eq/mod.rs` (`process` loop: the first sub-block of a call ends at the next multiple of `SUB` of the absolute position)
- Test: `crates/scythe-engine/src/scythe_eq/tests.rs`

**Interfaces:**
- Produces: `ScytheEq::process_block(&mut self, left: &mut [f32], right: &mut [f32], aux: Option<(&[f32], &[f32])>, position: Option<i64>)`; the `Effect::process` impl calls it with `ctx.aux` and `Some(ctx.position)` when playing or rendering. Without a position the grid continues from an internal frame counter (reset by `reset()`).

- [ ] **Step 1:** Test `dynamic_bands_ignore_block_size`: a dynamic band and a spectral band on a 2 s noise burst; render in blocks of 512, of 37, and of random sizes 1..=700 (seeded), positions continuous from 0; outputs bit-identical.
- [ ] **Step 2:** Run, expect FAIL (37-frame blocks differ). Implement. Run, expect PASS.
- [ ] **Step 3:** Run the whole engine suite (`devbox run test`). Expected: pass, except tests that pin exact dynamic-band samples; update those values once and say so in the commit body.
- [ ] **Step 4:** Commit (`fix(eq): keep ScyEQ-Pro's processing grid on the sample position`).

### Task 11: State keeps values it does not know

**Files:**
- Modify: `crates/scythe-engine/src/scythe_eq/params.rs` (`decode` returns `Decoded { values: Vec<f64>, extra: Vec<f64>, version: u32 }`; `encode(values, extra)`), `mod.rs` (`ScytheEq` keeps `extra` and writes it back)
- Test: `crates/scythe-engine/src/scythe_eq/tests.rs`

- [ ] **Step 1:** Tests: `unknown_values_survive_a_round_trip` (state with `TOTAL + 3` values and version `VERSION + 1` decodes, the 3 extra values come back byte-identical from `encode`); `newer_version_is_reported` (`Decoded.version` is the stored one); existing state from Song 2's first ScyEQ-Pro slot round-trips byte-identical (fixture copied into `crates/scythe-engine/tests/fixtures/scyeq-song2.state`).
- [ ] **Step 2:** Run, expect FAIL. Implement. Run, expect PASS.
- [ ] **Step 3:** Commit (`feat(eq): keep state values from newer versions`).

### Task 12: `scyeq-dsp`

**Files:**
- Create: `crates/scyeq-dsp/` (`Cargo.toml` MIT OR Apache-2.0, deps: `realfft`, `rustfft` and whatever `scythe_eq` uses today; no cpal, no scythe-core)
- Move: `crates/scythe-engine/src/scythe_eq/*` → `crates/scyeq-dsp/src/`; `crates/scythe-engine/src/probe.rs` → `crates/scyeq-dsp/src/probe.rs`; `clap_host::ParamInfo` → `scyeq_dsp::ParamInfo`
- Modify: `crates/scythe-engine/src/lib.rs` (`pub use scyeq_dsp::probe;`, `pub mod scythe_eq` becomes a module that re-exports `scyeq_dsp::*` and holds the `Effect` impl and the slot helpers `is_slot`, `slot_values`, `slot`, `DISPLAY`, `OLD_DISPLAY`), `clap_host.rs` (`pub use scyeq_dsp::ParamInfo;`)

- [ ] **Step 1:** Move; keep every existing path in Scythe compiling through the re-exports.
- [ ] **Step 2:** Run `devbox run test` and clippy. Expected: all pass, same count as before the move.
- [ ] **Step 3:** Commit (`refactor: ScyEQ-Pro's DSP in a crate of its own`).

### Task 13: `scyeq-ui` behind one trait

**Files:**
- Create: `crates/scyeq-ui/` (egui 0.37; `src/lib.rs`, `src/host.rs`, `src/editor.rs` from `fx_editor/scythe_eq.rs`, `src/analyzer.rs`, `src/eq_match.rs`, `src/model.rs`, `src/theme.rs` and `src/widgets.rs` with what these use, `src/fonts.rs` with `install(ctx: &egui::Context)` embedding Inter and JetBrains Mono, moved from `scythe-app/assets/fonts`)
- Modify: `crates/scythe-app/src/ui/fx_editor/*` (delegates to `scyeq_ui`), `crates/scythe-app/src/theme.rs` and `ui/widgets.rs` (re-export the moved items so other editors keep their paths), `crates/scythe-app/src/app.rs` (fonts from `scyeq_ui::fonts`)
- Test: existing `crates/scythe-app/src/itest/scythe_eq.rs` and the egui snapshot tests stay green; new `crates/scyeq-ui/tests/editor.rs`

**Interfaces:**
- Produces:

```rust
pub trait EqHost {
    fn values(&self) -> &[f64];
    fn begin(&mut self, id: u32);
    fn set(&mut self, id: u32, value: f64);
    fn end(&mut self, id: u32);
    /// One undo step for a multi-value edit (add band, EQ Match apply).
    fn edit(&mut self, label: &str, f: &mut dyn FnMut(&mut scyeq_dsp::ScytheEq));
    fn meters(&self) -> Option<std::sync::Arc<scyeq_dsp::Meters>>;
    fn probe(&self) -> Option<std::sync::Arc<scyeq_dsp::probe::FxProbe>>;
    fn instances(&self) -> Vec<Instance>;            // other ScyEQ-Pro instances
    fn compare_sources(&self) -> Vec<CompareSource>; // tracks (Scythe) or instances (plugin)
    fn load_reference(&mut self, path: &std::path::Path) -> Result<Reference, String>;
    fn view(&mut self) -> &mut ViewState;            // instance list open, panel sizes, selection
    fn notice(&self) -> Option<String>;              // e.g. "made with a newer ScyEQ-Pro"
}
pub fn show(ui: &mut egui::Ui, host: &mut dyn EqHost, state: &mut EditorState);
```

`Instance`, `CompareSource`, `Reference`, `ViewState`, `EditorState` are plain structs defined in `host.rs`; the builder fills their fields from what `eq_match.rs` and `scythe_eq.rs` read today.

- [ ] **Step 1:** Test in `scyeq-ui/tests/editor.rs` with a fake `EqHost`: `dragging_a_band_sends_one_gesture` (egui_kittest drag on band 1's handle → `begin(freq)`, ≥1 `set`, one `end`); `adding_a_band_is_one_edit` (double-click on the curve → one `edit` call labelled "Add band").
- [ ] **Step 2:** Run, expect FAIL. Move the code and implement `EqHost` for Scythe in `scythe-app` (the app's current `Ctx` calls behind it). Run, expect PASS.
- [ ] **Step 3:** Run `devbox run test` and `devbox run itest scythe_eq`. Expected: pass; UI snapshots unchanged.
- [ ] **Step 4:** Commit (`refactor: ScyEQ-Pro's editor in a crate of its own`).

## Phase 3: The plugin

### Task 14: `scyeq-plugin` on Linux: parameters, state, sound

**Files:**
- Create: `crates/scyeq-plugin/` (`cdylib` + `rlib`; deps `scyeq-dsp`, `scyeq-ui`, `plugin-format` by git tag), `src/lib.rs` (`export_clap!`, `export_vst3!`), `src/params.rs`, `src/plugin.rs`
- Create: `crates/scyeq-plugin/tests/{clap_host.rs,vst3_host.rs,licences.rs}`
- Modify: `crates/scythe-engine/tests/` new `scyeq_plugin_null.rs`; `devbox.json` `test` script builds `scyeq-plugin` first

**Interfaces:**
- Consumes: `plugin_format::{Plugin, Shared, Block, ParamInfo, Kind::Float(Map::Log), GuiContext}`, `scyeq_dsp::{ScytheEq, params}`.
- Produces: `ScyEqPlugin` (audio half) and `ScyEqShared` (values as `AtomicU64` f64 bits, `extra`, `version`, track info, editor size, probes); parameter list from `params::ids()` with names from `params::name`, `Kind::Float(Log)` for FREQ, stepped `Int`/`List` for shape, slope, placement, dynamic mode, phase, resolution, character, `Bool` for used/enabled/solo/audition/auto; automatable = `params::automatable()`.
- Pre/post probes: the plugin taps `FxProbe` before and after `process_block`, as `FxInstance` does in Scythe.

- [ ] **Step 1:** Tests:
  - `clap_host::reports_every_parameter` (count == `params::TOTAL`, ids 0..TOTAL, each parameter's min, max and default equal `params::spec_of(id)`, FREQ parameters are `Kind::Float { map: Map::Log, .. }`).
  - `clap_host::loads_scythe_state` (Song 2 fixture from Task 11 → `save` returns identical bytes).
  - `clap_host::loads_old_titled_slot` (state taken from a slot titled `CLAP: Scythe EQ (Scythe)`).
  - `clap_host::sample_rate_change_rebuilds` (activate at 44100, process; activate at 96000; a peak band at 1 kHz has +6 dB at 1 kHz on a 96 kHz sine, within 0.01 dB).
  - `clap_host::process_before_flush` (process right after activate with default state: output equals input).
  - `vst3_host::` the same five.
  - `scyeq_plugin_null::built_in_and_plugin_null` (Scythe engine: one project rendered with the built-in ScyEQ-Pro and once with the plugin loaded through Scythe's CLAP host, and through its VST3 host; for each band shape, a dynamic band, a spectral band, a sidechain band fed from a second track, each phase mode, at block sizes 64 and 512; difference exactly 0).
  - `scyeq_plugin_null::process_does_not_allocate` (Scythe's `probe_alloc` harness around the plugin's `process`).
- [ ] **Step 2:** Run, expect FAIL. Implement. Run, expect PASS.
- [ ] **Step 3:** Commit (`feat(scyeq-plugin): ScyEQ-Pro as a CLAP and VST3 plugin`).

### Task 15: The editor in the plugin

**Files:**
- Create: `crates/scyeq-plugin/src/host.rs` (`EqHost` for the plugin: values from `ScyEqShared`, `begin/set/end` through `GuiContext`, `edit` applies to a copy of the values and sends each changed parameter as begin/set/end then `mark_dirty`; `load_reference` decodes with the same decoder Scythe uses for EQ Match references)
- Modify: `src/plugin.rs` (`Shared::editor`, `EDITOR_SIZE` = Scythe's default ScyEQ-Pro window 1180x680, `EDITOR_MIN` 760x440, `RESIZABLE = true`, `editor_size`/`set_editor_size` stored in state's view section)
- Test: `crates/scyeq-plugin/tests/editor.rs`

- [ ] **Step 1:** Tests: `editor_drag_reaches_the_host` (plugin editor driven headless through `scyeq_ui::show` with the plugin's `EqHost`: a drag on band 1 produces CLAP gesture events for FREQ); `add_band_marks_dirty_and_sends_params` (USED and FREQ/GAIN/Q of the new band arrive as parameter events, `mark_dirty` once); `two_editors_at_once` (two instances, both editors open and closed 20 times, no panic).
- [ ] **Step 2:** Run, expect FAIL. Implement. Run, expect PASS.
- [ ] **Step 3:** By hand in REAPER on Linux (`~/.clap`, `~/.vst3`): open Song 2, open an instance, drag a band, undo (REAPER's undo restores it), write automation on a gain and play it back. Note results in the commit body.
- [ ] **Step 4:** Commit (`feat(scyeq-plugin): the ScyEQ-Pro editor in the plugin`).

### Task 16: Instances find each other

**Files:**
- Create: `crates/scyeq-plugin/src/registry.rs`
- Modify: `src/host.rs` (`instances`, `compare_sources`), `src/plugin.rs` (register on `new`, leave on drop; `set_track` stores name and colour)
- Test: `crates/scyeq-plugin/tests/registry.rs`

**Interfaces:**
- Produces: `registry::register(shared: &Arc<ScyEqShared>) -> Registration` (drop unregisters); `registry::others(me: u64) -> Vec<Arc<ScyEqShared>>` (live ones, in registration order). Writing another instance's band goes through that instance's own `GuiContext` (stored in its shared state while its host is known), so its host sees the change.

- [ ] **Step 1:** Tests: `instances_see_each_other` (three instances with track names "Kick", "Bass", "Vox": each lists the other two by name and colour); `dropped_instance_disappears` (drop "Bass" while "Kick"'s `EqHost` holds it as EQ Match source: next `instances()` omits it, the match source resets to none, no panic); `editing_another_instance_reaches_its_host` (from "Kick"'s editor, set band 1 gain of "Bass": "Bass"'s CLAP host receives the gesture).
- [ ] **Step 2:** Run, expect FAIL. Implement. Run, expect PASS.
- [ ] **Step 3:** Commit (`feat(scyeq-plugin): instances find each other`).

### Task 17: Version notice

**Files:**
- Modify: `crates/scyeq-plugin/src/host.rs` (`notice`), `crates/scyeq-ui/src/editor.rs` (shows `EqHost::notice` as a bar)
- Test: `crates/scyeq-plugin/tests/clap_host.rs`

- [ ] **Step 1:** Test `newer_state_shows_a_notice` (state with version `VERSION + 1`: `notice()` is `Some("Made with a newer ScyEQ-Pro; update to hear every setting.")`; current version: `None`).
- [ ] **Step 2:** Run, expect FAIL. Implement. Run, expect PASS. Commit (`feat(scyeq-plugin): tell the user when the project needs a newer plugin`).

### Task 18: Scythe recognises a ScyEQ-Pro VST3 slot

**Files:**
- Modify: `crates/scythe-engine/src/scythe_eq.rs` (the slot helpers from Task 12: `is_slot` also matches a `<VST` chunk whose class id is `ScytheScyEQPro01`; `slot_values` reads the component state out of REAPER's VST3 chunk layout with the existing VST3 state decoder in `vst3_host.rs`), `crates/scythe-engine/src/fx.rs` (such a slot runs the built-in EQ), `crates/scythe-engine/src/stock/mod.rs` (`shown_name` maps the VST3 title to `ScyEQ-Pro`)
- Test: `crates/scythe-engine/tests/scyeq_vst3_slot.rs` with a fixture `.RPP` saved by REAPER holding the VST3 plugin (made in Task 15's REAPER session; until then a fixture written by the test from the plugin's own state)

- [ ] **Step 1:** Tests: `vst3_slot_runs_built_in` (status `Native("parametric EQ")`, values equal the plugin's state); `vst3_slot_round_trips` (saving the project leaves the `<VST` block byte-identical when nothing changed); `edited_vst3_slot_saves_vst3` (a band changed in Scythe is written back into the same `<VST` block layout REAPER reads).
- [ ] **Step 2:** Run, expect FAIL. Implement. Run, expect PASS. Commit (`feat(engine): run ScyEQ-Pro VST3 slots as the built-in EQ`).

### Task 19: Windows and macOS builds, CI and install notes

**Files:**
- Create: `.github/workflows/scyeq-plugin.yml` (ubuntu, windows, macos: build the plugin, `plugin-format`'s `scripts/bundle.sh` and `scripts/validate.sh` (fetched at the tag), the plugin's host tests, the Scythe null test of Task 14; upload `ScyEQ-Pro-<version>-<os>.zip` as artifacts)
- Create: `crates/scyeq-plugin/README.md` (install per platform: CLAP to `~/.clap`, `%COMMONPROGRAMFILES%\CLAP`, `~/Library/Audio/Plug-Ins/CLAP`; VST3 likewise; use CLAP for exchanging projects with Scythe; one format per project; macOS beta: allow in System Settings > Privacy & Security or `xattr -dr com.apple.quarantine`)
- Modify: `README.md` (a ScyEQ-Pro plugin section linking the install notes), `THIRD_PARTY_NOTICES.md` (baseview, egui-baseview)

- [ ] **Step 1:** CI green on all three runners, validators without failures, artifacts present.
- [ ] **Step 2:** Windows VM: install the zip, REAPER for Windows opens Song 2 with all 30 instances playing; editor at 150% scaling.
- [ ] **Step 3:** Commit (`ci: build, validate and package ScyEQ-Pro on every platform`).

### Task 20: REAPER against Scythe, automated

**Files:**
- Create: `crates/scyeq-plugin/tests/reaper/` with test projects holding only ScyEQ-Pro instances (one per band shape, dynamic, spectral, sidechain from a second track, each phase mode, an automated gain at 512-frame blocks), and `scripts/reaper-compare.sh` (renders each project with `reaper -renderproject` and with Scythe (`scythe-server --headless --null-audio` and the `render.project` command), compares the two files sample by sample, fails above -120 dBFS peak difference)
- Modify: `docs/testing.md` (how to run it; Linux locally, Windows in the VM)

- [ ] **Step 1:** Run on Linux with REAPER and the installed plugin. Expected: every project within -120 dBFS.
- [ ] **Step 2:** Run in the Windows VM. Expected: the same.
- [ ] **Step 3:** Commit (`test: ScyEQ-Pro in REAPER against Scythe`).

### Task 21: The hand test with Thomas

**Files:**
- Create: `crates/scyeq-plugin/docs/hand-test.md` (the checklist from the spec's Testing layer 6, per platform, with a results table)

- [ ] **Step 1:** With Thomas on Linux and in the Windows VM: install from the zip, Song 2 in REAPER, edit, automate, undo, sidechain from another track, EQ Match against another instance, the instance list, save, open in Scythe, and back; HiDPI and resizing. Record results; fix what fails in the owning task's crate with a test first.
- [ ] **Step 2:** Commit the filled-in checklist (`docs: ScyEQ-Pro hand test results`).
