# Third-party notices

Scythe is licensed under MIT or Apache-2.0 (see `LICENSE-MIT` and `LICENSE-APACHE`). It contains
or is derived from the following third-party work, each under its own license.

| Component | Where | License | Notes |
|---|---|---|---|
| [cpal](https://github.com/RustAudio/cpal) 0.18.2 | `vendor/cpal/` | Apache-2.0 (`vendor/cpal/LICENSE`) | Changed for configurable ALSA periods; see `vendor/cpal/MODIFICATIONS.md` |
| [r8brain-free-src](https://github.com/avaneev/r8brain-free-src) 6.x, Aleksey Vaneev | `crates/scythe-engine/src/r8brain.rs` | MIT (notice in the file) | Rust implementation of its converter design, with its filter tables |
| [Preact](https://preactjs.com) and [htm](https://github.com/developit/htm) | `crates/scythe-server/web/vendor/` | MIT and Apache-2.0 (`LICENSE-preact.txt`, `LICENSE-htm.txt`) | Bundled unchanged |
| [Inter](https://rsms.me/inter/) | `crates/scyeq-ui/assets/fonts/` | SIL OFL 1.1 (`Inter-OFL.txt`) | |
| [JetBrains Mono](https://www.jetbrains.com/lp/mono/) | `crates/scyeq-ui/assets/fonts/` | SIL OFL 1.1 (`JetBrainsMono-OFL.txt`) | |
| [cargo-wix](https://github.com/volks73/cargo-wix) template | `crates/scythe-app/wix/main.wxs` | Apache-2.0 | The installer definition started from its template |
| [SWS extension](https://github.com/reaper-oss/sws) | marker actions and cue buses | MIT | Behaviour follows its rules; no code is copied |

Rust crates Scythe depends on are fetched by Cargo and keep their own licenses: almost all MIT
or Apache-2.0, some BSD, ISC, Zlib, Unicode-3.0 or BSL-1.0, and `symphonia` under MPL-2.0
(used unchanged). Binary releases list every crate with its license text.

The ScyEQ-Pro plugin (`crates/scyeq-plugin`) links its crates statically into one library, among
them [baseview](https://github.com/RustAudio/baseview) and
[egui-baseview](https://codeberg.org/RustAudio/egui-baseview) for its editor window (MIT or
Apache-2.0) and `symphonia` for EQ Match reference files (MPL-2.0, used unchanged).

Libraries loaded at run time and not shipped with Scythe: SoundTouch (LGPL-2.1), Rubber Band
(GPL-2.0-or-later, only with the `rubberband` feature), and on Windows the ASIO SDK (Steinberg's
license, only with the `asio` feature). See "Licensing notes" in the README.

## Trademarks

REAPER, Cockos and the names of REAPER's plugins (ReaEQ, ReaComp and the like) are trademarks of
Cockos Incorporated. VST and ASIO are trademarks of Steinberg Media Technologies GmbH. Scythe is
not affiliated with, endorsed or sponsored by either; the names are used only to say what Scythe
is compatible with.
