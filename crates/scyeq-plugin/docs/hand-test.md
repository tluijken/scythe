# ScyEQ-Pro hand test

Run per platform before a release, with the plugin installed from the release zip (see the
plugin's README). Automated checks (`scripts/reaper-compare.sh`, `docs/testing.md`) cover the
sound; this list covers what only a person at the screen can judge.

## Checklist

1. Install: copy `ScyEQ-Pro.clap` and `ScyEQ-Pro.vst3` from the zip; REAPER finds both after a
   rescan (FX browser lists "ScyEQ-Pro (Scythe)" under CLAP and VST3).
2. Song 2: open it in REAPER; every ScyEQ-Pro instance shows its curve and plays.
3. Edit: drag a band handle, change shape, Q and gain with mouse and wheel; the sound follows,
   the readouts match the handles.
4. Automate: write automation on a band's gain and frequency (touch or latch); it plays back.
5. Undo: REAPER's undo reverts an edit made in the editor, and redo brings it back.
6. Sidechain: route another track into channels 3/4 and pick it as a dynamic band's key; the band
   reacts to that track.
7. EQ Match: match one instance against another track's instance; the result curve is sensible.
8. Instance list: lists the other ScyEQ-Pro instances by track name and colour; opening one edits
   that instance; closing a track removes it from the list.
9. Save and exchange: save in REAPER, open the project in Scythe (settings and automation equal),
   save in Scythe, open again in REAPER (still equal).
10. Window: resize the editor (it keeps its layout down to the minimum size); on a HiDPI screen
    the editor is sharp and sized for the scale.
11. Closing: close the FX window while the reference-file picker is open; nothing hangs or
    crashes.

## Results

| # | Linux (REAPER 7.73) | Windows VM (REAPER 7.85) | Notes |
|---|---|---|---|
| 1 | | | |
| 2 | | | |
| 3 | | | |
| 4 | | | |
| 5 | | | |
| 6 | | | |
| 7 | | | |
| 8 | | | |
| 9 | | | |
| 10 | | | |
| 11 | | | |

macOS: not tested by hand yet (beta; CI runs the host tests and validators).
