#!/usr/bin/env bash
# ScyEQ-Pro in REAPER against Scythe (docs/testing.md): renders test projects that hold only
# ScyEQ-Pro instances in REAPER with the plugin built from this tree and in scythe-server, fails
# when any pair differs by more than -120 dBFS peak, then opens a song project in REAPER and
# checks every ScyEQ-Pro instance in it loads. A local check: REAPER is not on CI.
#
# Linux: `devbox run -- scripts/reaper-compare.sh [song.RPP]`; Windows: the same from Git Bash,
# without devbox. The song defaults to Song 2 when it is there. REAPER runs with a configuration
# of its own in a temporary folder that only sees the fresh plugin; nothing is installed and the
# user's REAPER settings are not read or written.
#
#   REAPER             the REAPER binary (default: reaper on PATH, on Windows the usual install)
#   PLUGIN_FORMAT      plugin-format checkout with scripts/bundle.sh (default: the one cargo builds)
#   PLUGIN_TOOLCHAIN   command the plugin is built under (see below)
#   KEEP=1             keep the work folder with the projects and renders
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
case "$(uname -s)" in
    Linux) os=linux ;;
    MINGW* | MSYS* | CYGWIN*) os=windows ;;
    *) echo "unsupported system: $(uname -s)" >&2; exit 2 ;;
esac
if [ $os = linux ]; then . scripts/env.sh; fi

song=${1:-$HOME/music/the_heritance/mix-test/Song 2/Song 2.RPP}
if [ $os = windows ]; then
    reaper=${REAPER:-/c/Program Files/REAPER (x64)/reaper.exe}
else
    reaper=${REAPER:-$(command -v reaper || true)}
fi
[ -x "$reaper" ] || { echo "no REAPER: put it on PATH or set REAPER" >&2; exit 2; }

if [ $os = linux ]; then
    xvfb=${SCYTHE_XVFB:-$(command -v Xvfb || true)}
    if [ -z "$xvfb" ] && command -v nix-build >/dev/null; then
        xvfb=$(nix-build '<nixpkgs>' -A xvfb --no-out-link 2>/dev/null)/bin/Xvfb
    fi
    [ -x "$xvfb" ] || { echo "no Xvfb: put it on PATH or set SCYTHE_XVFB" >&2; exit 2; }
fi

plugin_format=${PLUGIN_FORMAT:-}
if [ -z "$plugin_format" ]; then
    manifest=$(cargo metadata --format-version 1 | grep -o '"manifest_path":"[^"]*plugin-format[^"]*Cargo.toml"' | head -n1)
    manifest=${manifest#\"manifest_path\":\"}
    manifest=${manifest%\"}
    plugin_format=$(dirname "${manifest//\\\\//}")
    if [ $os = windows ]; then plugin_format=$(cygpath -u "$plugin_format"); fi
fi
[ -f "$plugin_format/scripts/bundle.sh" ] || { echo "no bundle.sh in '$plugin_format': set PLUGIN_FORMAT" >&2; exit 2; }

# Paths handed to Windows programs inside text (JSON, the REAPER configuration).
native() { if [ $os = windows ]; then cygpath -w "$1"; else printf '%s\n' "$1"; fi; }
json_path() { native "$1" | sed 's/\\/\\\\/g'; }

work=$(mktemp -d "${TMPDIR:-/tmp}/scyeq-reaper.XXXXXX")
pids=()
cleanup() {
    for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    wait 2>/dev/null || true
    if [ "${KEEP:-0}" = 1 ]; then echo "kept $work"; else rm -rf "$work"; fi
}
trap cleanup EXIT

# A plugin built in the devbox shell needs devbox's glibc, newer than a REAPER from the system's
# nixpkgs has, and REAPER then cannot load it. Released plugins are built on an older glibc; here
# the toolchain of the system's nixpkgs stands in, with a target folder of its own.
plugin_env=(env -u LD_LIBRARY_PATH -u PKG_CONFIG_PATH)
if [ -z "${PLUGIN_TOOLCHAIN+set}" ] && [[ $(readlink -f "$reaper") == /nix/store/* ]]; then
    PLUGIN_TOOLCHAIN="nix shell nixpkgs#rustc nixpkgs#cargo nixpkgs#gcc --command"
    plugin_env+=(CARGO_TARGET_DIR="$root/target/reaper-compare")
fi

echo "== build"
"${plugin_env[@]}" ${PLUGIN_TOOLCHAIN:-} bash "$plugin_format/scripts/bundle.sh" scyeq-plugin ScyEQ-Pro com.scythe.scyeq-pro \
    "$work/plugins" >/dev/null
cargo build --release -p scythe-server
cargo build --release -p scyeq-plugin --example reaper_projects
target=${CARGO_TARGET_DIR:-$root/target}/release
projects=$work/projects
"$target/examples/reaper_projects" write "$(native "$projects")" >/dev/null

mkdir -p "$work/reaper" "$work/home"
cfg=$work/reaper/reaper.ini
if [ $os = windows ]; then
    cat >"$cfg" <<EOF
[REAPER]
clap_path_win64=$(native "$work/plugins")
vstpath64=$(native "$work/plugins")
renderclosewhendone=4
verchk=0
[audioconfig]
mode=5
dummy_srate=48000
dummy_blocksize=512
EOF
else
    cat >"$cfg" <<EOF
[REAPER]
clap_path_linux-x86_64=$work/plugins
vstpath=$work/plugins
linux_audio_mode=2
linux_audio_srate=48000
linux_audio_bsize=512
renderclosewhendone=4
verchk=0
EOF
    exec 3< <("$xvfb" -displayfd 1 -screen 0 1280x1024x24 -nolisten tcp 2>/dev/null)
    pids+=($!)
    read -r -t 20 display <&3 || { echo "Xvfb did not start" >&2; exit 1; }
    export DISPLAY=:$display
fi

# REAPER adds ~/.vst3 to its VST path whatever the configuration says: a home of its own keeps
# it from the user's plugins. Devbox's library path would load libraries REAPER was not built with.
run_reaper() { env -u LD_LIBRARY_PATH HOME="$work/home" "$@"; }

echo "== REAPER"
for rpp in "$projects"/*.RPP; do
    run_reaper timeout 120 "$reaper" -cfgfile "$(native "$cfg")" -nosplash -renderproject "$(native "$rpp")" \
        >/dev/null 2>&1 || true
    [ -f "${rpp%.RPP}.reaper.wav" ] || echo "$(basename "$rpp"): REAPER wrote nothing"
done

echo "== Scythe"
mkdir -p "$work/xdg/config" "$work/xdg/data" "$work/xdg/cache"
XDG_CONFIG_HOME=$work/xdg/config XDG_DATA_HOME=$work/xdg/data XDG_CACHE_HOME=$work/xdg/cache \
    SCYTHE_SYSTEM_PLUGINS=0 "$target/scythe-server" --null-audio --control 127.0.0.1:0 \
    --access-file "$(native "$work/xdg/access.json")" 2>"$work/server.log" &
pids+=($!)
api=
for _ in $(seq 300); do
    api=$(grep -o 'http://[^ ]*/api' "$work/server.log" || true)
    [ -n "$api" ] && break
    sleep 0.1
done
[ -n "$api" ] || { cat "$work/server.log" >&2; exit 1; }
call() { curl -sS -X POST -H 'Content-Type: application/json' -d "$1" "$api/command"; }
latest=0
for rpp in "$projects"/*.RPP; do
    out=${rpp%.RPP}.scythe.wav
    call "{\"cmd\":\"project.open\",\"path\":\"$(json_path "$rpp")\"}" >/dev/null
    call "{\"cmd\":\"render.settings\",\"file\":\"$(json_path "$out")\"}" >/dev/null
    call '{"cmd":"render.project"}' >/dev/null
    for _ in $(seq 600); do
        events=$(call "{\"cmd\":\"events.since\",\"since\":$latest}")
        next=$(grep -o '"latest":[0-9]*' <<<"$events" | head -n1 | cut -d: -f2)
        if [ -z "$next" ]; then
            echo "$(basename "$rpp"): events.since answered ${events:-nothing}" >&2
            break
        fi
        latest=$next
        if grep -q '"message":"Render' <<<"$events"; then break; fi
        sleep 0.1
    done
    [ -f "$out" ] || echo "$(basename "$rpp"): Scythe wrote nothing"
done

echo "== compare"
status=0
"$target/examples/reaper_projects" compare "$(native "$projects")" || status=1

if [ -f "$song" ]; then
    echo "== $(basename "$song") in REAPER"
    copy=$work/song/$(basename "$song")
    mkdir -p "$(dirname "$copy")"
    # The copy has every other effect offline and no items: REAPER stops at a window listing the
    # plugins it cannot find (this REAPER sees only ScyEQ-Pro), and at one for each missing file.
    perl -0pe 's/^(\s*)<ITEM\r?\n.*?^\1>\r?\n//gms;
        s/(BYPASS \d+ )\d+( \d+\r?\n\s*<(?:VST|VST3|CLAP|LV2|JS|AU|DX)\b(?![^\n]*scythe\.eq))/${1}1$2/g' \
        "$song" >"$copy"
    cat >"$work/count.lua" <<'EOF'
local out = io.open(os.getenv("COUNT_OUT"), "w")
local function count(track, label)
  for fx = 0, reaper.TrackFX_GetCount(track) - 1 do
    local _, ident = reaper.TrackFX_GetNamedConfigParm(track, fx, "fx_ident")
    if ident:find("scythe.eq", 1, true) then
      local _, name = reaper.TrackFX_GetFXName(track, fx)
      local params = reaper.TrackFX_GetNumParams(track, fx)
      local offline = reaper.TrackFX_GetOffline(track, fx)
      local state = (params > 0 and not offline) and "loaded" or "missing"
      out:write(string.format("%s\t%s\t%s\t%d\n", state, label, name, params))
    end
  end
end
reaper.Main_openProject("noprompt:" .. os.getenv("SONG"))
count(reaper.GetMasterTrack(0), "master")
for i = 0, reaper.CountTracks(0) - 1 do
  local track = reaper.GetTrack(0, i)
  local _, name = reaper.GetTrackName(track)
  count(track, name)
end
out:close()
reaper.Main_OnCommand(40004, 0)
EOF
    COUNT_OUT=$(native "$work/count.txt") SONG=$(native "$copy") run_reaper timeout 120 "$reaper" \
        -cfgfile "$(native "$cfg")" -nosplash -new "$(native "$work/count.lua")" >/dev/null 2>&1 || true
    expected=$(grep -c 'scythe\.eq' "$song" || true)
    loaded=$(grep -c '^loaded' "$work/count.txt" 2>/dev/null || true)
    echo "$loaded of $expected ScyEQ-Pro instances loaded"
    grep -v '^loaded' "$work/count.txt" 2>/dev/null || true
    [ "$loaded" = "$expected" ] || status=1
else
    echo "== no song project at $song: skipped"
fi
exit $status
