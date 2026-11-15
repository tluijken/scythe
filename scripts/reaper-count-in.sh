#!/usr/bin/env bash
# Count-in and pre-roll in REAPER (docs/verification.md): plays a test project in REAPER on a
# private JACK dummy server, records its outputs with jack_capture and the transport position
# from a ReaScript, and checks the playhead's start, the clicks before it and the count-in's
# length against the starts Scythe models (crates/scythe-core/src/metronome.rs, `starts_match_reaper`
# and `count_ins_are_padded_to_whole_blocks`). Linux only; a local check, REAPER is not on CI.
#
# `devbox run -- scripts/reaper-count-in.sh`. REAPER runs with a configuration and HOME of its
# own in a temporary folder; the JACK server has a name of its own and stops with the script.
#
#   REAPER   the REAPER binary (default: reaper on PATH)
#   BLOCK    frames per block of the dummy device (default 1024)
#   ONLY     a regular expression: run only the cases whose names match it
#   KEEP=1   keep the work folder with the projects, captures and logs
set -euo pipefail
cd "$(dirname "$0")/.."
[ "$(uname -s)" = Linux ] || { echo "Linux only" >&2; exit 2; }
reaper=${REAPER:-$(command -v reaper || true)}
[ -x "$reaper" ] || { echo "no REAPER: put it on PATH or set REAPER" >&2; exit 2; }
python=$(command -v python3 || true)
[ -n "$python" ] || { echo "no python3" >&2; exit 2; }
block=${BLOCK:-1024}

tool() {
    local var=$1 attr=$2 bin=$3 found
    found=$(command -v "$bin" || true)
    if [ -z "$found" ] && command -v nix-build >/dev/null; then
        found=$(nix-build '<nixpkgs>' -A "$attr" --no-out-link 2>/dev/null || true)
        found=${found:+$found/bin/$bin}
    fi
    [ -x "$found" ] || { echo "no $bin: put it on PATH" >&2; exit 2; }
    printf -v "$var" '%s' "$found"
}
tool jackd jack2 jackd
tool jack_capture jack_capture jack_capture
tool jack_lsp jack-example-tools jack_lsp
tool xvfb xvfb Xvfb
# REAPER and the JACK tools find libjack through this; devbox's library path would load
# libraries REAPER was not built with.
server=scythe-count-in-$$
jacklib=$(dirname "$(dirname "$(readlink -f "$jackd")")")/lib
jackenv=(env -u LD_LIBRARY_PATH LD_LIBRARY_PATH="$jacklib" JACK_DEFAULT_SERVER="$server" JACK_NO_START_SERVER=1
    JACK_NO_AUDIO_RESERVATION=1)

work=$(mktemp -d "${TMPDIR:-/tmp}/scythe-count-in.XXXXXX")
pids=()
case_pids=()
cleanup() {
    for pid in "${case_pids[@]}" "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    wait 2>/dev/null || true
    if [ "${KEEP:-0}" = 1 ]; then echo "kept $work"; else rm -rf "$work"; fi
}
trap cleanup EXIT

exec 3< <("$xvfb" -displayfd 1 -screen 0 1280x1024x24 -nolisten tcp 2>/dev/null)
pids+=($!)
read -r -t 20 display <&3 || { echo "Xvfb did not start" >&2; exit 1; }
export DISPLAY=:$display
"${jackenv[@]}" "$jackd" -n "$server" -d dummy -r 48000 -p "$block" -C 4 -P 4 \
    >"$work/jackd.log" 2>&1 &
pids+=($!)
jack_up=
for _ in $(seq 100); do
    "${jackenv[@]}" "$jack_lsp" >/dev/null 2>&1 && { jack_up=1; break; }
    sleep 0.1
done
[ -n "$jack_up" ] || { echo "the JACK dummy server did not start:" >&2; tail -n 5 "$work/jackd.log" >&2; exit 1; }

# A ramp whose value tells the project position of every frame the master plays.
"$python" -I - "$work/ramp.wav" <<'EOF'
import array, struct, sys
sr, secs = 48000, 30
x = array.array('f', ((i / sr + 1.0) / 64.0 for i in range(sr * secs)))
data = x.tobytes()
with open(sys.argv[1], 'wb') as f:
    f.write(b'RIFF' + struct.pack('<I', 36 + len(data)) + b'WAVE')
    f.write(b'fmt ' + struct.pack('<IHHIIHH', 16, 3, 1, sr, sr * 4, 4, 32))
    f.write(b'data' + struct.pack('<I', len(data)) + data)
EOF

cat >"$work/measure.lua" <<'EOF'
local dir = os.getenv("CASE_DIR")
local start_at = tonumber(os.getenv("CASE_AT"))
local rec = os.getenv("CASE_MODE") == "rec"
local secs = tonumber(os.getenv("CASE_SECS"))
local out = io.open(dir .. "/transport.tsv", "w")
reaper.Main_openProject("noprompt:" .. dir .. "/case.RPP")
if rec then
  reaper.InsertTrackAtIndex(1, false)
  local tr = reaper.GetTrack(0, 1)
  reaper.SetMediaTrackInfo_Value(tr, "I_RECINPUT", 0)
  reaper.SetMediaTrackInfo_Value(tr, "I_RECARM", 1)
end
local t0
local function go()
  local f = io.open(dir .. "/go", "r")
  if f then f:close() end
  return f ~= nil
end
local function tick()
  if not t0 then
    if go() then
      reaper.SetEditCurPos(start_at, false, false)
      t0 = reaper.time_precise()
      reaper.Main_OnCommand(rec and 1013 or 1007, 0)
    end
    reaper.defer(tick)
    return
  end
  local now = reaper.time_precise() - t0
  out:write(string.format("%.6f\t%d\t%.6f\t%.6f\n", now, reaper.GetPlayState(), reaper.GetPlayPosition(),
    reaper.GetPlayPosition2()))
  if now < secs then
    reaper.defer(tick)
    return
  end
  reaper.Main_OnCommand(rec and 40667 or 1016, 0)
  out:close()
  reaper.Main_SaveProject(0, false)
  reaper.Main_OnCommand(40004, 0)
end
reaper.defer(tick)
EOF

# name, metronome flags, reaper.ini preroll and prerollmeas, start, play or rec; then as
# measured: where the playhead starts, the wait before it without padding, the first click into
# it, that click's beat in its bar, the clicks before the start and the seconds that count in.
cases="
disabled           10 0  2    4.0   play 4.0  0.0  0.0   0 0 0.0
count-in           11 0  2    4.0   play 4.0  2.0  0.0   0 4 2.0
disabled-rec       16 0  2    4.0   rec  4.0  0.0  0.0   0 0 0.0
count-in-rec       21 0  2    4.0   rec  4.0  2.0  0.0   0 4 2.0
before-pre-roll    11 1  1    6.0   play 4.0  2.0  0.0   0 4 2.0
before-pre-roll-rec 21 2 1    6.0   rec  4.0  2.0  0.0   0 4 2.0
rec-pre-roll       5  2  1    7.0   rec  4.0  0.0  0.0   0 0 0.0
rec-pre-roll-mid-bar 5 6 1    7.3   rec  5.3  0.0  0.0   0 0 0.0
pre-roll-before-0  3  1  1    1.0   play 0.0  2.0  0.0   0 4 0.0
rec-mid-bar-before-0 21 6 1   0.3   rec  0.0  3.7  0.2   1 7 2.0
quiet-before-0     1  1  1    1.0   play 0.0  2.0  0.0   0 0 0.0
count-in-before-0  11 1  1    1.0   play 0.0  4.0  0.0   0 8 2.0
mid-bar-before-0   11 5  1    0.3   play 0.0  3.7  0.2   1 7 2.0
mid-bar            11 0  2    3.125 play 3.125 2.0 0.375 3 4 2.0
count-in-at-bar    11 8  2    3.125 play 3.125 3.125 0.0 0 7 3.125
both-at-bar        11 13 1    9.25  play 7.25 3.25 0.0   0 7 3.25
pre-roll-at-bar    3  1  1    9.0   play 6.0  0.0  0.0   0 0 0.0
pre-roll-mid-bar   3  5  1    9.0   play 7.0  0.0  0.0   0 0 0.0
pre-roll-1.75      3  5  1.75 9.0   play 5.5  0.0  0.0   0 0 0.0
pre-roll-1.75-at   3  1  1.75 9.0   play 4.0  0.0  0.0   0 0 0.0
count-in-0.95      11 8  2    3.9   play 3.9  1.9  0.0   0 4 1.9
count-in-0.9       11 8  2    3.8   play 3.8  1.8  0.0   0 4 1.8
count-in-0.875     11 8  2    3.75  play 3.75 3.75 0.0   0 8 3.75
pre-roll-0.95      3  1  1    7.9   play 6.0  0.0  0.0   0 0 0.0
pre-roll-0.9       3  1  1    7.8   play 6.0  0.0  0.0   0 0 0.0
pre-roll-0.875     3  1  1    7.75  play 4.0  0.0  0.0   0 0 0.0
"

cat >"$work/check.py" <<'EOF'
import array, math, struct, sys

path, transport, block, mode = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
at, start_exp, wait, first, first_in_bar, nclicks, counting = map(float, sys.argv[5:12])
sr, beat = 48000, 24000
# A recording's pre-roll inside the project is lengthened at its start to whole blocks.
pad = 0.0
if mode == "rec" and 0 < start_exp < at:
    pad = (-round((at - start_exp) * sr)) % block / sr
    if start_exp - pad < 0:
        pad = 0.0

def read(path):
    b = open(path, 'rb').read()
    i, ch, data = 12, 0, b''
    while i < len(b):
        cid, size = b[i:i + 4], struct.unpack('<I', b[i + 4:i + 8])[0]
        if cid == b'fmt ':
            ch = struct.unpack('<H', b[i + 10:i + 12])[0]
        elif cid == b'data':
            data = b[i + 8:i + 8 + size]
        i += 8 + size + (size & 1)
    x = array.array('f')
    x.frombytes(data)
    return [x[c::ch] for c in range(ch)]

ramp, _, click = read(path)
problems = []
start = next((i for i, v in enumerate(ramp) if abs(v) > 1e-7), None)
if start is None:
    print("  the master played nothing")
    sys.exit(1)
a, n = start + sr // 10, sr // 2
xs = range(n)
ys = ramp[a:a + n]
mx, my = (n - 1) / 2, sum(ys) / n
slope = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sum((x - mx) ** 2 for x in xs)
icpt = my - slope * mx
start_pos = icpt / slope / sr - 1.0 - (a - start) / sr
# REAPER fades the master in over the first block; after that the ramp is on the line unless
# the capture lost a block.
off = [abs(ramp[i] - icpt - slope * (i - a)) > 50 * abs(slope) for i in range(start, a + n)]
skips = sum(off[2 * block:])
onsets, last = [], -sr
for i, v in enumerate(click):
    if abs(v) > 1e-4:
        if i - last > sr // 20:
            onsets.append(i)
        last = i
before = [(i - start, max(abs(v) for v in click[i:i + 1440])) for i in onsets if i < start - 2]
gaps = [b[0] - a[0] for a, b in zip(before, before[1:])]
if skips or any(abs(g - beat) > 2 for g in gaps):
    print("  DROPOUT")
    sys.exit(3)
print(f"  playhead from {start_pos:.6f}", end="")
want_start = max(start_exp, 0.0) - pad
if abs(want_start - start_pos) > 2 / sr:
    problems.append(f"playhead from {start_pos:.6f}, not {want_start:.6f}")
rel = [round(r / sr, 6) for r, _ in before]
print(f", clicks before it at {rel}", end="")
want = [round(-(wait - first) + pad + k * 0.5, 6) for k in range(int(nclicks))]
if len(rel) != len(want) or any(abs(r - w) > 2 / sr for r, w in zip(rel, want)):
    problems.append(f"clicks at {rel}, not {want}")
peaks = [p for _, p in before]
if peaks:
    accents = [k for k, p in enumerate(peaks) if p > 1.5 * min(peaks)]
    want_acc = [k for k in range(int(nclicks)) if (int(first_in_bar) + k) % 4 == 0]
    if accents != want_acc:
        problems.append(f"accents on clicks {accents}, not {want_acc}")
rows = [l.split('\t') for l in open(transport) if l.strip()]
playing = [(float(r[2]), float(r[3])) for r in rows if int(r[1]) & 1]
if counting > 0 and playing:
    pos, pos2 = playing[0]
    told = round(pos2 - pos, 6)
    frames = round(counting * sr)
    padded = (frames + (-frames) % block) / sr
    print(f", count-in {told}", end="")
    if abs(told - padded) > 2 / sr:
        problems.append(f"count-in {told} s, not {padded:.6f}")
print()
for p in problems:
    print("  DIFFERS: " + p)
sys.exit(1 if problems else 0)
EOF

status=0
while read -r name flags preroll pm at mode from wait first first_in_bar nclicks counting; do
    [ -n "$name" ] || continue
    [[ $name =~ ${ONLY:-.} ]] || continue
    secs=$(awk -v w="$wait" 'BEGIN { printf "%d", w + 4 }')
    echo "== $name"
    for attempt in 1 2 3 4; do
        dir=$work/$name
        rm -rf "$dir"
        mkdir -p "$dir/home"
        printf '[REAPER]\nlinux_audio_mode=0\njack_options=1\nlinux_audio_nch_in=4\nlinux_audio_nch_out=4\nverchk=0\npreroll=%s\nprerollmeas=%s\n' \
            "$preroll" "$pm" >"$dir/reaper.ini"
        cat >"$dir/case.RPP" <<EOF
<REAPER_PROJECT 0.1 "7.73/linux-x86_64" 0
  SAMPLERATE 48000 0 0
  <METRONOME $flags 1
    VOL 0.25 0.125
  >
  METRONOMEOUT 1026
  TEMPO 120 4 4
  <TRACK
    NAME ramp
    <ITEM
      POSITION 0
      LENGTH 30
      <SOURCE WAVE
        FILE "$work/ramp.wav"
      >
    >
  >
>
EOF
        CASE_DIR=$dir CASE_AT=$at CASE_MODE=$mode CASE_SECS=$secs "${jackenv[@]}" HOME="$dir/home" timeout $((secs + 60)) \
            "$reaper" -cfgfile "$dir/reaper.ini" -nosplash -new "$work/measure.lua" >"$dir/reaper.log" 2>&1 &
        rpid=$!
        case_pids=($rpid)
        ports=
        for _ in $(seq 300); do
            ports=$("${jackenv[@]}" "$jack_lsp" 2>/dev/null | grep '^REAPER:out[123]$' || true)
            [ "$(grep -c . <<<"$ports")" = 3 ] && break
            sleep 0.1
        done
        [ "$(grep -c . <<<"$ports")" = 3 ] || { echo "  REAPER did not open the JACK device" >&2; kill $rpid; exit 1; }
        sleep 2
        "${jackenv[@]}" "$jack_capture" -c 3 -p REAPER:out1 -p REAPER:out2 -p REAPER:out3 -d $((secs + 3)) -f wav \
            "$dir/capture.wav" >"$dir/capture.log" 2>&1 &
        cpid=$!
        case_pids+=($cpid)
        sleep 1
        touch "$dir/go"
        wait $cpid || true
        wait $rpid || true
        set +e
        "$python" -I "$work/check.py" "$dir/capture.wav" "$dir/transport.tsv" "$block" "$mode" \
            "$at" "$from" "$wait" "$first" "$first_in_bar" "$nclicks" "$counting"
        result=$?
        set -e
        # The dummy driver runs without real-time priority: a cycle missed while nothing
        # sounds shows only as jack_capture's xrun count.
        if [ $result = 1 ] && grep -aq 'Xruns: [1-9]' "$dir/capture.log"; then
            echo "  (jack_capture saw an xrun)"
            result=3
        fi
        [ $result = 3 ] || break
    done
    [ $result = 0 ] || status=1
done <<<"$cases"
exit $status
