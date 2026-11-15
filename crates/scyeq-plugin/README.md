# ScyEQ-Pro plugin

Scythe's ScyEQ-Pro equalizer as a CLAP and VST 3 plugin. Each zip, `ScyEQ-Pro-<version>-<os>.zip`,
holds `ScyEQ-Pro.clap` and `ScyEQ-Pro.vst3`, with the licenses and third-party notices.

## Install

Copy each into the folder for its format, then let the host rescan its plugins.

| System | CLAP | VST 3 |
|---|---|---|
| Linux | `~/.clap` | `~/.vst3` |
| Windows | `%COMMONPROGRAMFILES%\CLAP` | `%COMMONPROGRAMFILES%\VST3` |
| macOS | `~/Library/Audio/Plug-Ins/CLAP` | `~/Library/Audio/Plug-Ins/VST3` |

On Windows `%COMMONPROGRAMFILES%` is usually `C:\Program Files\Common Files`; copying there needs
an administrator.

On Linux, EQ Match picks a reference file with the desktop's file chooser portal
(xdg-desktop-portal with a FileChooser backend) or, without one, with zenity; with neither the
editor says so instead of opening a dialog.

### NixOS

REAPER from nixpkgs does not have the X11 and OpenGL libraries the editor loads on its library
path (`libX11-xcb.so.1` from libx11, libGL from libglvnd, and the driver in `/run/opengl-driver/lib`),
so it shows its generic parameter list instead of the editor. Add them to `LD_LIBRARY_PATH` when
starting REAPER:

```sh
LD_LIBRARY_PATH="$(nix build --no-link --print-out-paths nixpkgs#libx11.out)/lib:$(nix build --no-link --print-out-paths nixpkgs#libglvnd.out)/lib:/run/opengl-driver/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" reaper
```

In a NixOS or Home Manager configuration the same goes into a wrapper around `reaper`, with
`lib.makeLibraryPath [ pkgs.libx11 pkgs.libglvnd ]` and `/run/opengl-driver/lib`.

## Which format

Use the CLAP plugin for projects you exchange with Scythe: Scythe keeps its own ScyEQ-Pro as the
CLAP plugin's slot, so the project opens in both with the same settings. Use one format per
project; the CLAP and VST 3 plugins are separate plugins to a host, and a project made with one
does not load its settings into the other.

## macOS (beta)

The macOS build is a universal binary (Apple silicon and Intel), signed ad hoc and not notarized,
so macOS blocks it the first time a host loads it. Allow it under System Settings > Privacy &
Security, or remove the quarantine flag before starting the host:

```sh
xattr -dr com.apple.quarantine ~/Library/Audio/Plug-Ins/CLAP/ScyEQ-Pro.clap
xattr -dr com.apple.quarantine ~/Library/Audio/Plug-Ins/VST3/ScyEQ-Pro.vst3
```
