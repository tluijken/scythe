# Control API

The control API is how other programs drive a Scythe session: the web UI, an MCP server for
AI agents, a tablet with a monitor mix or setlist. It is served by `scythe-server` (and
`scythe --headless`), and by the desktop app when started with `--control`. Every command goes
through the same undoable edits as the UI, so `project.undo` undoes a remote change just like a
local one.

Protocol version: **1** (reported by `/api/health` and the WebSocket `hello`).

## Serving

| Where | How |
|---|---|
| Headless | `scythe-server [project.RPP]` or `scythe --headless [project.RPP]`; listens on `127.0.0.1:8770` unless `--control ADDR` (or `control = "…"` in the config file) says otherwise; `--control off` disables it |
| Desktop app | `scythe --control 0.0.0.0:8770` or `SCYTHE_CONTROL=0.0.0.0:8770` |

An admin token (`--token`, `--control-token` in the app, `token = "…"` in the config file, or
`SCYTHE_CONTROL_TOKEN`) makes every endpoint except `/api/health` and the web UI's files
require a token, either as `Authorization: Bearer <token>` or as a `?token=<token>` query
parameter (for browsers opening a WebSocket). Binding to anything other than a loopback
address without a token prints a warning: anyone on the network could then drive the session,
including opening and saving files. There is no TLS; put a reverse proxy in front for anything
beyond a trusted LAN.

### Roles and access links

The admin token may do everything. Access links (see `access.*` below) carry tokens of their
own, each limited to a role:

| Role | Commands | Events |
|---|---|---|
| `admin` | all, MCP, `/api/files`, `/api/peaks` | all |
| `musician` (one musician's mix, found by name) | `mix.list` (their mix alone), `mix.get`, `mix.level`, `mix.pan`, `mix.mute`, `mix.more_me`, `mix.volume`, `mix.click` on their own mix; `transport.state`, `setlist.status`, `access.whoami`; `snapshot` answers `{"revision", "name", "transport", "access"}` | `meters` with only their mix in `mixes` (no tracks, no master), `project_changed` |
| `stage` | `setlist.songs`, `list`, `get`, `export`, `status`, `select`, `cue`, `play`, `stop`, `next`, `previous`, `auto_advance`, `count_in`; `transport.play`, `transport.stop`, `transport.state`, `transport.health`, `transport.performance`; `access.whoami`; the same short `snapshot` | `transport` (with the count-in), `setlist`, `health`, `project_changed` |

Anything else answers `forbidden` (HTTP 403). The check runs on the session's thread for every
command, from HTTP, the WebSocket or anything else, so a page cannot widen it. Without an admin
token a client presenting no token is admin, as before; a link token still narrows. Links are
kept in a JSON file of the server's own (`access_file` in the config, `--access-file`;
`$XDG_CONFIG_HOME/scythe/access.json` by default, mode 0600), not in the project, so they
survive restarts and opening another project, and do not travel with a copied .RPP.

## Endpoints

| Method | Path | |
|---|---|---|
| `GET` | `/api/health` | `{"ok": true, "name": "scythe", "protocol": 1, "token": true, "clients": 2}` (`token`: whether the others need one; `clients`: WebSockets connected), no token needed |
| `POST` | `/api/command` | body: one command; answer: `{"ok": true, "result": …}` |
| `GET` | `/api/snapshot` | the same as the `snapshot` command |
| `GET` | `/api/peaks?item=…&width=…&from=…&to=…` | the same as `item.peaks` (`from` and `to` optional), for waveforms a browser can cache by address |
| `GET` | `/api/files?path=…` | admin only: the folders (`--folder`, `folders = […]`; home by default) and, given a `path` inside one, its sub-folders and `.RPP` files: `{"roots", "path", "parent", "entries": [{"name", "path", "dir", "size", "modified"}]}`; a path resolving outside every folder (through `..` or a link) is `forbidden` |
| `GET` | `/api/ws` | WebSocket: commands in, replies and events out |
| `POST` | `/mcp` | the same commands as MCP tools for AI agents, see [mcp.md](mcp.md); `?tools=` picks the tools listed |
| `GET` | `/mcp` | the MCP server's notifications as server-sent events |
| `GET` | `/`, `/monitor`, `/setlist`, … | the web UI (see [features.md](features.md#web-ui-browser-tablet-phone)), no token needed to load it |

Errors answer `{"ok": false, "error": {"kind": …, "message": …}}` with HTTP status 400 for
`invalid` (malformed, unknown command, value out of range), 404 for `not_found` (a track, send,
effect, item or marker that does not exist), 409 for `failed` (understood but could not be
done: the file did not open, nothing to undo, no armed track to record), 403 for `forbidden`
(the token's role may not do it) and 401 for `unauthorized`.

### WebSocket

On connecting the server sends `{"type": "hello", "protocol": 1, "access": {"role": "admin"}}`
(`access` is the token's role, with `musician` for a musician's link). A client sends commands as
JSON text messages, optionally with an `id` of any JSON type; each gets exactly one reply, in
the order the commands were sent:

```json
→ {"id": 7, "cmd": "track.volume", "track": "Vox", "db": -4.5}
← {"type": "reply", "id": 7, "ok": true, "result": {"id": 12, "name": "Vox", "volume_db": -4.5, …}}
← {"type": "reply", "id": 8, "ok": false, "error": {"kind": "not_found", "message": "no track with id 99"}}
```

Every connected client also receives the events (below) its role may hear as `{"type":
"event", "event": "project_changed", "revision": 42}`: the event's name with its fields beside it.
A client that falls behind skips events rather than slowing the session
down. `access.*` commands are answered by the server itself, at once.

## Commands

A command is a JSON object whose `cmd` names `domain.action`; its other fields are its
arguments. Fields shown with `?` may be left out.

**Tracks** are named by `track`: the id from a snapshot (a number, stable for the session), the
track's exact name (a string), or `"master"` where the master makes sense (volume, pan, mute,
effects). **Levels** are given as `volume` (gain, 1.0 = 0 dB, capped at +24 dB) or `db`. **Pan**
runs from -1 (left) to 1 (right). Times are in seconds. Flags (`muted`, `solo`, `armed`,
`monitor`, `bypassed`) toggle when left out. Fields a command does not know are ignored, so a
misspelt flag toggles: read the answer to see the state.

Continuous controls (track and send volume and pan, item volume) fold repeated changes of the
same control within a second into one undo step, so a fader dragged on a tablet is undone in
one go.

### Transport

Every transport command answers with the transport state (see `transport` events).

| `cmd` | Arguments | |
|---|---|---|
| `transport.play` | | play from the edit cursor; nothing while playing |
| `transport.pause` | | stop and leave the edit cursor where playback was (ends a recording) |
| `transport.stop` | | stop (ending and keeping a recording) and return to the edit cursor |
| `transport.record` | | record the armed tracks; `failed` when nothing is armed |
| `transport.seek` | `position` | move the edit cursor, and the playhead while playing |
| `transport.loop` | `enabled?`, `start?`, `end?` | repeat on/off and/or the range it repeats (both ends) |
| `transport.metronome` | `enabled?`, `volume` or `db` (its level at its outputs; REAPER's default 0.25) | on or off, and its level; saved with the project, not an undo step |
| `transport.metronome_output` | `output` (device output from 0, or `null`), `mono?`, `default?` | where the metronome plays: point it at a drummer's in-ear output and the audience hears no click. With `default` it sets the preference for projects that name no output (`null`: the master) |
| `transport.count_in` | `playback?`, `record?`, `measures?` (above 0, up to 16), `measure_start?` | count in before playback and/or recording; `measure_start` (a preference) counts in from the bar line before the start |
| `transport.preroll` | `playback?`, `record?`, `measures?` (above 0, up to 16), `measure_start?` | pre-roll before playback and/or recording (preferences) |
| `transport.health` | | the health state (below), not the transport state |
| `transport.performance` | `reset?` | the performance meter: `rt_cpu` (percent of the block's time the last callback took), `rt_longest_block_ms` of the last few seconds against `block_ms`, `xruns`, `overloads` (late blocks), `tracks_cpu`, and `tracks`: per track in project order (`index` from 1, `name`, `cpu` in percent of one core, `longest_block_ms`, `pdc` in frames, `fx` with each effect's `name` and `cpu`), then the master's effects (`index` 0) when it has any; a new reading about once a second. Also `rt_blocks` and `rt_block_p50_ms`, `rt_block_p99_ms`, `rt_block_p999_ms`, `rt_block_max_ms`: the callbacks rendered since the window started and their render time at those percentiles; `reset: true` starts a new window after the reading; `ahead_ms`, `ahead_misses` and `live_tracks` for anticipative FX |
| `transport.state` | | |

A transport state:

```json
{"playing": true, "recording": false, "position": 120.0, "bar_beat": "61.1.00", "cursor": 120.0,
 "loop_enabled": false, "loop_range": null, "metronome": false, "metronome_volume": 0.25,
 "metronome_outputs": [{"output": 4, "mono": false}], "metronome_default_output": null,
 "count_in": {"playback": true, "record": false, "measures": 1.0, "measure_start": false,
              "position": -1.25, "beat": 2, "beats": 4},
 "preroll": {"playback": false, "record": false, "measures": 2.0, "measure_start": true}}
```

**Count-in.** With `playback` (or `record`) on and the metronome on, starting playback (or
recording) first plays `measures` bars of metronome at the tempo and time signature where it
starts, while the playhead waits at the start: `playing` is true, `position` stays put and
`count_in.position` counts up from minus the count-in's length (in seconds) to 0, with `beat`
of `beats` clicking. The clicks fall on the project's beats carried back from the start, so a
song starting on beat 3 counts in with the accents where its bar lines are; with
`measure_start` the count-in starts on the bar line before (or counts in to the next bar line
when the start is within a tenth of a bar of it). As in REAPER, a disabled metronome means no
count-in at all; input monitoring and effects run on meanwhile, items wait. A setlist with
`count_in` (see `setlist.count_in`) counts in every song it plays, clicking even with the
metronome off. The settings are REAPER's: `<METRONOME flags measures` in the project, flags 8
and 16 being "Count-in before playback" and "before recording".

**Pre-roll.** With `preroll.playback` (or `record`) on, playback starts `measures` bars before
the position asked for, on the bar line unless `measure_start` is off; a recording keeps only
what comes after the start. A count-in comes before the pre-roll. Before the project start the
playhead waits as during a count-in (`count_in.position` counts the wait, clicking only if the
metronome runs during playback or recording).

**Preferences.** Pre-roll, `count_in.measure_start` and `metronome_default_output` are what
REAPER keeps in reaper.ini rather than the project (`preroll`, `prerollmeas`,
`metronome_defout`); Scythe keeps them under the same keys in
`$XDG_CONFIG_HOME/scythe/preferences.ini`. Changing them does not mark the project changed.

**Where the click goes.** `metronome_outputs` are REAPER's metronome hardware outputs
(`METRONOMEOUT` lines after the `<METRONOME` chunk, coded like a hardware output's `I_DSTCHAN`);
empty, the click plays to `metronome_default_output`, and with the master when that is `null`. A monitor mix can also take the click as a source of
its own (`mix.click`), so each musician sets their own click level; that is Scythe's own
(REAPER ignores it).

A health state, also sent as the `health` event:

```json
{"device": "Scarlett 18i20 USB", "sample_rate": 48000, "block_size": 128, "buffer_ms": 2.67,
 "output_latency_ms": 5.33, "cpu": 14.2, "cpu_peak": 31.0, "xruns": 0, "overloads": 0,
 "status": "running", "reason": null, "reconnects": 0,
 "realtime": {"policy": "SCHED_RR", "priority": 20, "via": "rtkit", "reason": null}}
```

`cpu` is the time spent rendering over the time the buffer lasts, in percent (at 100 the audio
drops out); `cpu_peak` the highest over the last seconds. `xruns` counts the underruns the
device reported, `overloads` the callbacks that took longer than their audio. `status` is
`silent` when no device plays: none was asked for, it was missing at the start or it went away
(unplugged); `reason` says which. The session then plays on silently, so the transport,
setlist and clients carry on, and looks for the device every five seconds; when it is back
(`reconnects` goes up) playback continues through it and the inputs open again. Both changes
also come as `notice` events. `realtime` says how the thread rendering the audio is scheduled
once it has run: `via` is `direct` (`SCHED_FIFO` within the user's rtprio limit), `rtkit`
(RealtimeKit granted it over D-Bus), `host` (the audio system's thread already was real-time)
or `none`, with `reason` saying why.

### Project

| `cmd` | Arguments | Answer |
|---|---|---|
| `snapshot` (or `project.snapshot`) | | the whole project, below |
| `project.open` | `path` (on the machine running Scythe) | snapshot |
| `project.save` | `path?` (the project's own file when left out) | history state |
| `project.new` | | snapshot |
| `project.undo`, `project.redo` | | `{"revision", "dirty", "label", "undo", "redo"}`; `failed` when there is nothing to undo or redo |
| `project.tempo` | `bpm` (20 to 400), `at?` (the edit cursor) | `{"bpm"}`: sets the tempo in effect at `at`, one undo step while it keeps changing |
| `project.tabs` | | the open tabs: `index`, `name`, `path`, `dirty`, `active` |
| `project.tab_new` | `path?` (an empty project when left out) | the tabs; the new one is active |
| `project.tab_switch` | `index` | the tabs; the tab left stops and keeps its unsaved changes |
| `project.tab_close` | `index?` (the active tab) | the tabs; closing the last one leaves an empty project |

### Tracks

| `cmd` | Arguments | Answer |
|---|---|---|
| `track.list` | | every track |
| `track.get` | `track` | the track |
| `track.add` | `name?`, `index?` (end when left out) | the new track |
| `track.delete` | `track` | `{}`; only that track goes and the folders around it stay as they were: a folder's children move up a level in its place, and a track that closed folders hands that on to the track before it |
| `track.rename` | `track`, `name` | the track |
| `track.freeze` | `track` | `{"file", "levels"}`: renders the items through the effects (pre-fader, one second of tail) beside the saved project and plays that instead; again stacks a level |
| `track.unfreeze` | `track` | `{"levels"}`: the last level's effects and items are back |
| `track.move` | `track`, `index?`, `parent?`, `also?` (more tracks moved along after it, as a selection drags) | the track, a folder with its children, every folder kept closed: to position `index` in the track list (keeping its depth where the tracks around allow), or with `parent` into that folder (`"master"` for the top level) at `index` among its other children (the end when left out); a plain parent becomes a folder |
| `track.volume` | `track`, `volume` or `db` | the track; volume groups follow (a dB offset, reversed followers the other way) |
| `track.pan` | `track`, `pan` | the track; pan groups follow (an offset) |
| `track.mute` | `track`, `muted?` | the track; mute groups follow |
| `track.solo` | `track`, `solo?`, `mode?` (`"in_place"`, the default, or `"ignore_routing"`) | the track; solo groups follow |
| `track.solo_defeat` | `track`, `on?` | solo defeat: the track stays heard while others are soloed (monitor mixes have it) |
| `track.phase` | `track`, `phase?` (or `invert?`; toggles when both are left out) | polarity, inverted at the fader (post-fader sends follow, pre-fader and pre-FX sends do not); polarity groups follow |
| `track.pan_mode` | `track`, `mode` (`"balance"`, `"classic"`, `"stereo"`, `"dual"` or `null` for the project default) | the track |
| `track.width` | `track`, `width` (-1 to 1) | the track; width groups follow |
| `track.dual_pan` | `track`, `left`, `right` (-1 to 1) | the track: where dual pan puts each channel |
| `track.arm` | `track`, `armed?` | the track; record-arm groups follow |
| `track.monitor` | `track`, `monitor?` | input monitoring: an armed track with monitoring on plays its input through its FX |
| `track.input` | `track`, `input` | record input in REAPER's `I_RECINPUT` numbering, inputs counted from 0: mono input *n* is *n*, the stereo pair from *n* is 1024 + *n*, MIDI is 4096 + device × 32 + channel (device 63 = all, 62 = the virtual keyboard, channel 0 = all), -1 is none |
| `track.midi_input` | `track`, `device?` (`"all"`, `"keyboard"` or a device number from `midi.devices`; all when left out), `channel?` (1-16, 0 or left out for all) | the track, its input set to that MIDI device and channel |
| `track.color` (or `track.colour`) | `track`, `color` (`"#rrggbb"` or `null`) | the track |
| `track.folder` | `track`, `depth` | REAPER's `I_FOLDERDEPTH`: 1 starts a folder holding the tracks below, -*n* closes *n* folders after this track, 0 is a plain track |
| `track.new_folder` | `tracks`, `name?` | the new folder: REAPER's "Move tracks to new folder" puts it where the first track was and the tracks (with their children) inside |
| `track.collapse` | `track`, `compact?` (0 normal, 1 small, 2 collapsed: `I_FOLDERCOMPACT`), `mixer?` (hide the children in the mixer) | the track; with neither it cycles 0, 1, 2 as "Track: Cycle folder collapsed state". Not an undo step, as in REAPER |
| `track.channels` | `track` (or `"master"`), `channels` (even, 2-128) | the track: its channel count, REAPER's `NCHAN`. Channels 3/4 are the sidechain (auxiliary) input of its effects |

### Recording and lanes

| `cmd` | Arguments | Answer |
|---|---|---|
| `track.record_mode` | `track`, `mode?`, `tap?`, `auto_arm?` | `{"track", "armed", "mode", "tap", "monitor", "monitor_items", "auto_arm"}`. `mode` is REAPER's per-track record mode: `"input"`, `"input_mono"`, `"input_stereo"`, `"input_multichannel"`, `"input_midi"`, `"output_stereo"`, `"output_mono"`, `"output_multichannel"` (each also `…_compensated`), `"midi_output"`, `"midi_overdub"`, `"midi_replace"`, `"midi_touch_replace"`, `"midi_latch_replace"` or `"disabled"` (monitor only); `tap` is where output recording listens: `"post_fader"`, `"pre_fx"` or `"post_fx"` |
| `track.monitor_mode` | `track`, `mode?` (`"off"`, `"on"`, `"auto"`: tape style, not while playing back), `items?` (play the track's own items while it records) | as `track.record_mode` |
| `record.options` | `mode?` (`"normal"`, `"time_selection_punch"`, `"item_punch"`: the selected items get the takes), `new_lanes?`, `loop_takes?`, `lanes_exclusive?` | the project record mode and the lane preferences (REAPER's `RECMODE` and `autoxfade` bits) |
| `lanes.list` | `track` | `{"track", "fixed", "names", "playing", "comp_lane", "areas": [{"start", "end", "lane"}], "items"}` (items per lane) |
| `lanes.fixed` | `track`, `on` | fixed item lanes on or off; the lanes as `lanes.list` |
| `lanes.add` | `track` | an empty lane at the bottom |
| `lanes.play` | `track`, `lanes` (from 0 at the top; empty for all) | which lanes play (`LANESOLO`) |
| `lanes.comp` / `lanes.comp_off` | `track` | comp into a new empty lane at the top / stop comping (the lane stays) |
| `lanes.swipe` | `track`, `lane`, `start`, `end` (seconds) | a comp area: the comp lane plays that lane there, overlapped areas cross-fade into it |
| `lanes.remove_area` | `track`, `at` (seconds) | removes the comp area there |

### Track groups

REAPER's 64 track groups. In each a track can lead or follow volume, pan, width, mute, solo,
record arm, polarity, automation mode and media edits, with reverse flags; a lead's edits
(through the API, the UI or scripts) carry over to the followers. A VCA lead (`VOLUME_VCA_LEAD`)
scales its followers' level as they play instead of moving their faders.

| `cmd` | Arguments | Answer |
|---|---|---|
| `group.list` | | every group in use: `group` (1-64), `name`, `members` (`track`, `name`, `params`) |
| `group.name` | `group`, `name` (`null` clears it) | the groups |
| `group.set` | `track`, `group`, `params` (REAPER's names: `VOLUME_LEAD`, `VOLUME_FOLLOW`, `VOLUME_VCA_LEAD`, `VOLUME_VCA_FOLLOW`, `VOLUME_VCA_FOLLOW_ISPREFX`, `PAN_LEAD`, …, case-insensitive), `on?` (default true) | the groups |
| `group.clear` | `group` | the groups, without it |

Track snapshots carry `groups` (`group`, `params`), `phase`, `pan_mode`, `width`, `dual_pan`,
`solo_mode` and `solo_defeat`. The group commands are for admin tokens only.

### Sends

Sends are numbered from 0 per sending track, in the order `send.list` gives.

| `cmd` | Arguments | Answer |
|---|---|---|
| `send.list` | `track` | the track's sends |
| `send.add` | `track`, `dest` | the new send |
| `send.remove` | `track`, `send` | the remaining sends |
| `send.volume` | `track`, `send`, `volume` or `db` | the send |
| `send.pan` | `track`, `send`, `pan` | the send |
| `send.mute` | `track`, `send`, `muted?` | the send |
| `send.mode` | `track`, `send`, `mode?` (`post_fader`, `pre_fx`, `post_fx`), `mono?`, `phase?` | the send |
| `send.midi` | `track`, `send`, any of `enabled`, `audio` (false makes a MIDI-only send), `src_channel`, `dst_channel`, `src_bus`, `dst_bus` (1-16; 0 is all for sources and unchanged for destinations), `faders` | the send; its `midi` field (null without MIDI) and `audio` show the same, stored as REAPER's `I_MIDIFLAGS` |
| `send.channels` | `track`, `send`, any of `src` (first source channel, from 1), `src_count` (1, 2 or an even number up to 64), `dst` (first destination channel on the receiving track, from 1), `dst_mono` (mix down to the one channel `dst`) | the send; stored as REAPER's `I_SRCCHAN` / `I_DSTCHAN`. Both ends must fit the tracks' channel counts (`track.channels` adds more) |
| `send.hw_list` | `track` (or `"master"`) | the hardware outputs |
| `send.hw_add` | `track`, `output?` (device output from 0; the next free one when left out), `mono?` | the new output |
| `send.hw_remove` | `track`, `hw` | the remaining outputs |
| `send.hw_set` | `track`, `hw`, any of `output`, `mono`, `mode`, `volume` or `db`, `pan`, `muted`, `phase` | the output |

`track.main_send` (`track`, `on?`) turns a track's feed to its parent or the master on or off;
a bus that only plays through hardware outputs has it off.

Send entries (in `send.list` and track snapshots) carry the audio routing as `src`, `src_count`, `dst`
and `dst_mono` (channels from 1; `src` is null for a MIDI-only send). A sidechain: give the
receiving track four channels and send into 3/4, where its effects (a Scythe EQ band with
`external_sidechain`, a compressor's detector) listen without the key reaching the track's own
1/2:

```json
{"cmd": "track.channels", "track": "Guitar bus", "channels": 4}
{"cmd": "send.add", "track": "Piano", "dest": "Guitar bus"}
{"cmd": "send.channels", "track": "Piano", "send": 0, "dst": 3}
```

### Monitor mixes

A monitor mix ("cue bus") is a track named "MON *musician*" that receives its sources
pre-fader (post-FX), does not feed the master and plays through a hardware output. `mix` and
`source` are track references like `track`. "More me" changes the receive levels themselves,
so REAPER plays the mix the same.

| `cmd` | Arguments | Answer |
|---|---|---|
| `mix.list` | | every mix |
| `mix.get` | `mix` | the mix |
| `mix.create` | `musician`, `sources?` (every track that is not a mix or folder parent), `own?`, `output?`, `mono?`, `mode?`, `levels_from_tracks?`, `pans_from_tracks?`, `limiter?` (on) | the new mix |
| `mix.copy` | `mix`, `musician` | the copy, on the next free output |
| `mix.rename` | `mix`, `musician` | the mix |
| `mix.delete` | `mix` | the remaining mixes |
| `mix.level` | `mix`, `source`, `volume` or `db` | the mix |
| `mix.pan` | `mix`, `source`, `pan` | the mix |
| `mix.mute` | `mix`, `source`, `muted?` | the mix |
| `mix.own` | `mix`, `source`, `own?` | the mix; own sources are what "more me" raises |
| `mix.more_me` | `mix`, `db` (-24 to 24) | the mix: own sources `db`/2 up, the rest `db`/2 down |
| `mix.volume` | `mix`, `volume` or `db` | the mix's own level |
| `mix.output` | `mix`, `output` (from 0, or `null`), `mono?` | the mix |
| `mix.limiter` | `mix`, `on?` | the mix |
| `mix.click` | `mix`, `volume` or `db` (0 takes it out) | the mix; the metronome and every count-in as a source of the mix at its own level, before the mix's effects (Scythe only) |
| `mix.add_source` | `mix`, `source`, `mode?` | the mix |
| `mix.remove_source` | `mix`, `source` | the mix |
| `mix.cue_bus` | `sources`, `name?`, `mode?`, `volumes_from_tracks?`, `pans_from_tracks?`, `output?`, `mono?` | the new bus as a track: SWS's "Create cue buss from track selection" |

A mix:

```json
{"id": 20, "musician": "Drummer", "name": "MON Drummer", "color": "#a6e22e",
 "volume": 1.0, "volume_db": 0.0, "more_me": 6.0,
 "output": {"output": 2, "mono": false, "label": "Out 3/4"}, "limiter": true, "click": 0.5,
 "sources": [{"track": 12, "name": "Kick", "color": "#66d9ef", "volume": 1.41,
              "volume_db": 3.0, "pan": 0.0, "muted": false, "mode": "post_fx", "own": true}]}
```

### Effects

Effects are numbered from 0 per chain; `track` may be `"master"`. Every `fx.*` command that
names a chain also takes `chain` (`track`, the default; `input`, the track's record input chain
on the live input it monitors; `take`, with `item` and `take?`, the active take when left out;
`monitor`, the monitoring chain after the master, heard on the device only, never rendered, and
saved as `reaper-hwoutfx.ini` beside the preferences) and `inside`, the positions of the
container the effect sits in (each one inside the container before it; left out for the chain
itself).

| `cmd` | Arguments | Answer |
|---|---|---|
| `fx.list` | `track` | `[{"index", "name", "enabled", "running", "offline", "wet", "delta", "parallel", "container"}]` (`running`: processing audio, not passed through; `parallel`: `serial`, `parallel` or `merge_midi`; `container`: `{"channels", "inputs", "outputs", "effects"}` for an FX container) |
| `fx.catalog` | `query?` (only entries whose name, id or format contain it, ignoring case), `rescan?` (check every plugin file anew, failed ones too) | `{"plugins": [{"id", "name", "format"}], "scanning", "progress": {"done", "total", "current"}, "failed": [{"format", "path", "reason"}]}`: built-in effects and the installed CLAP, LV2 and VST 3 plugins known so far. It answers at once: plugins are scanned in the background (from the cache when unchanged), and while `scanning` is true more may follow, so ask again; `fx.add` of a plugin not found yet says the scan is still running |
| `fx.add` | `track`, `plugin` (a catalog `id` such as `builtin:compressor`; the built-ins are `equalizer`, `gate`, `compressor`, `delay`, `convolution-reverb`, `limiter`, `multiband-compressor`, `algorithmic-reverb`, `synth` and `midi-control`; or an exact name), `index?` | the chain |
| `fx.remove` | `track`, `index` | the chain |
| `fx.bypass` | `track`, `index`, `bypassed?` | the chain |
| `fx.params` | `track`, `index` | `[{"id", "name", "value", "text", "min", "max", "default"}]` of a hosted plugin |
| `fx.param` | `track`, `index`, `param` (an id from `fx.params`), `value` (clamped to the range) | the parameter |
| `fx.meters` | `track`, `index` | `{"input_db", "output_db", "reduction_db", "detector_db", "gate_open", "band_reduction_db"}`; measuring runs for about two seconds after each call, so the first can read nothing |
| `fx.eq_band` | `track`, `index`, `action` (`add`, `remove`, `set`), `band`, `kind` (`low_shelf`, `high_shelf`, `band`, `notch`, `low_pass`, `high_pass`, `band_pass`, `all_pass`), `freq`, `gain_db`, `width` | `{"bands"}`; one undo step |
| `eq.band` | `track`, `index` (a Scythe EQ), `band?` (0-23), `remove?`, `shape?`, `freq?`, `gain_db?`, `q?`, `slope?`, `stereo?`, `enabled?`, `dynamic?`, `range_db?`, `threshold_db?`, `auto_threshold?`, `attack_ms?`, `release_ms?`, `external_sidechain?` | the band; one undo step. Bands are 24 fixed slots: without `band` a new band goes into the first free slot, a `band` that is free is added there (from the defaults, then the fields given), a `band` in use is changed. `remove` of a band not in use answers `invalid` naming the bands in use |
| `fx.delay_tap` | `track`, `index`, `action`, `tap`, `time_ms`, `musical`, `feedback_db`, `volume_db`, `pan`, `lowpass`, `highpass`, `enabled` | `{"taps"}`; one undo step |
| `fx.fir_points` | `track`, `index`, `points` (`[[hz, db], …]`) | `{"points"}`; one undo step |
| `fx.wet` | `track`, `index`, `wet?` (0-1), `delta?` (play only the difference the effect makes) | the chain |
| `fx.parallel` | `track`, `index`, `mode` (`serial`, `parallel`: on the same input as the effect before, outputs summed; `merge_midi`: also merging its MIDI output) | the chain |
| `fx.offline` | `track`, `index`, `offline?` (toggles when left out) | the chain |
| `fx.container.move_in` | `track`, `index` (the effect), `container` (the container's index), `position?` | the chain |
| `fx.container.move_out` | `track`, `index` (the container), `child` | the chain, the effect just after the container |
| `fx.container.config` | `track`, `index`, `channels?` (2-64), `inputs?`, `outputs?` (pins to the chain around it) | the chain |
| `fx.container.pins` | `track`, `index`, `output?` (output pins rather than input pins), `pins` (per pin, the outer channels from 1) | the chain |
| `fx.container.map` | `track`, `index`, `child`, `param` (position in the child's parameters) | `{"id"}` of the container parameter |
| `fx.take_channels` | `item`, `take?`, `channels?` (2-64; `null` follows the source) | `{"channels"}` |

Plugin parameters live in the running plugin, as with the inspector's sliders: they mark the
project unsaved and are stored on save, but are not undo steps.

### Guitar rigs

A rig is one FX container named "Rig: <amp> + <cab>" holding a ReaGate, the Neural Amp Modeler
slot, the cab (a fully wet ReaVerb with the impulse response, and a parallel second ReaVerb when
two responses blend), a ReaEQ with low cut, bass, mid, treble and high cut, and a ReaEQ whose
only band is off for the output level. REAPER opens it as the container it is. `track` names
the track, `index?` the rig's position in its chain (the track's first rig when left out). Every
answer but `rig.presets` and `rig.delete_preset` is the rig: `{"track", "track_name", "index",
"name", "settings", "snapshots", "preset"}`, `settings` being
`{"gate": {"enabled", "threshold_db"}, "amp": {"enabled", "model", "input_db", "output_db"},
"cab": {"enabled", "ir", "ir2", "blend", "level_db", "auto"}, "eq": {"enabled", "low_cut",
"high_cut", "bass_db", "mid_db", "treble_db"}, "output_db"}` (a cut of `null` is off).

| `cmd` | Arguments | Answer |
|---|---|---|
| `rig.list` | | `{"rigs", "presets"}` |
| `rig.get` | `track`, `index?` | the rig |
| `rig.create` | `track`, `preset?` | a new rig at the end of the chain |
| `rig.convert` | `track`, `index` (a NAM slot) | the NAM slot with a ReaGate right before it and the ReaVerb(s) and ReaEQ right after it as a rig; stages it lacks are added switched off, so it sounds the same |
| `rig.set_amp` | `track`, `index?`, `model?`, `gear?` (when the file does not say), `input_db?`, `output_db?` | the rig; a capture with a cabinet (`amp_cab`, `full_rig`, TONE3000's `amp-cab`) switches the cab off, an amp-only one on, while `cab.auto` is set |
| `rig.set_cab` | `track`, `index?`, `ir?`, `ir2?` (`""` removes it), `blend?` (0-1), `level_db?`, `enabled?` (by hand, which clears `auto`), `auto?` | the rig |
| `rig.stage` | `track`, `index?`, `stage` (`gate`, `amp`, `cab`, `eq`), `enabled` | the rig |
| `rig.set` | `track`, `index?`, `settings` (merged into the settings above) | the rig |
| `rig.snapshot` | `track?`, `index?` (the selected track's rig when left out), `snapshot` (`A`-`D`), `store?` | the rig, switched to (or stored as) the snapshot |
| `rig.presets` | | the preset names (`$XDG_DATA_HOME/scythe/rigs/*.json`) |
| `rig.save_preset` | `track`, `index?`, `name` | `{"saved", "presets"}` |
| `rig.load_preset` | `track?`, `index?`, `name` (or `next`, `previous`) | the rig (a new one when the track has none) |
| `rig.delete_preset` | `name` | the preset names |

Switching is click-free: a new model warms up beside the old one and the two crossfade, a new
impulse response crossfades over 20 ms, and gate, EQ and level changes reach the running
effects without restarting them. The actions `_SCYTHE_RIG_SNAPSHOT_A` … `_D`,
`_SCYTHE_RIG_PRESET_NEXT` and `_SCYTHE_RIG_PRESET_PREVIOUS` do the same for the selected track's
rig from a footswitch, a learned MIDI control, a surface or `action.run`.

### Items

Items are named by their `id` from a snapshot or `item.list`.

| `cmd` | Arguments | Answer |
|---|---|---|
| `item.list` | `track?` | the items of the track, or of all tracks |
| `item.move` | `item`, `position?`, `track?` | the item (its group moves along, later items ripple as the ripple mode says, overlaps crossfade while auto-crossfade is on; a locked item stays) |
| `item.split` | `item`, `at` | both halves; `invalid` unless `at` lies inside the item (the item's group splits too) |
| `item.delete` | `item` | `{}` (the gap closes as the ripple mode says; a locked item stays) |
| `item.volume` | `item`, `volume` or `db` | the item |
| `item.mute` | `item`, `muted?` | the item |
| `item.peaks` | `item`, `width` (columns, 1 to 8192), `from?`, `to?` (seconds into the item; all of it when left out) | `{"item", "from", "to", "state", "min", "max"}`: per column the lowest and highest sample (channels summed, item and take gain applied, -1 to 1) from the engine's peak cache; `state` is `ready`, `loading` (ask again shortly), `offline` (the file is missing) or `none` (no audio file) |

### Editing

These follow the project's editing settings as REAPER's actions do: ripple mode, item groups,
locks and auto-crossfade. Items are lists of ids.

| `cmd` | Arguments | Answer |
|---|---|---|
| `edit.ripple` | `mode?`: `off`, `per_track`, `all_tracks` | `{"ripple", "auto_crossfade", "grouping"}` |
| `project.editing` | `auto_crossfade?`, `grouping?` | the same |
| `project.grid` | `division?` (whole notes, as `GetSetProjectGrid`), `kind?` (`normal`, `swing`, `measure`), `swing?` (-1 to 1), `snap?`, `lines?` | `{"division", "kind", "swing_mode", "swing", "snap", "lines", "label"}` |
| `project.snap` | `time` | `{"time"}` snapped to the grid (`SnapToGrid`; unchanged while snapping is off) |
| `project.lock` | `enabled?`, `modes?` (names: "time selection", "full item", "left/right item", "up/down item", "track envelope", "region", "marker", "time signature marker", "item edges", "item fade/volume handles", "loop points") | `{"enabled", "modes", "flags"}` |
| `item.lock` | `items`, `locked?` (toggles when left out) | the items |
| `item.group` / `item.ungroup` | `items` | `{"group"}` / the items |
| `item.select_groups` | `items` | the ids with their group mates |
| `item.nudge` | `items`, `value`, `what?` (`position`, `left_trim`, `left_edge`, `right_edge`, `contents`, `duplicate`, `edit_cursor`), `unit?` (REAPER's nudge units 0-21: ms, seconds, grid, notes 1/256 to whole, measures.beats, samples, frames, pixels, item lengths, item selections), `set?`, `snap?`, `reverse?`, `copies?` | the items to select afterwards (`{"cursor"}` for the edit cursor) |
| `item.fade` | `item`, `fade` (`in`/`out`), `length?`, `shape?` (0-7, 7 equal power), `curve?`, `kind?` (REAPER's fade types 1-7) | the fade |
| `item.crossfade` | `items`, `start?` and `end?` (crossfade neighbours over that range, extending them), `kind?` (crossfade type 1-7 for items that already crossfade) | the items |
| `item.transients` | `item`, `sensitivity?` (0-1, 0.5), `threshold_db?` (-24) | `{"transients"}` in project time |
| `item.dynamic_split` | `items`, `sensitivity?`, `threshold_db?`, `min_length?` | the pieces |
| `take.explode` | `items`, `mode?` (`across_tracks`, `in_place`, `in_order`) | the new items |
| `take.implode` | `items`, `mode?` (`across_tracks`, `same_track`) | the resulting items |
| `take.crop` / `take.delete` | `items` | the items |
| `take.active` | `item`, `take` | the item |
| `edit.razor` | `areas` (`track`, `start`, `end`, `envelope?` naming an envelope lane), `op` (`delete`, `split`) | `{}` |

### Markers and regions

Markers and regions are named by their number (`marker`). REAPER numbers markers and regions
separately, so `region: true` or `false` picks one when both use a number; left out, the first
match wins.

| `cmd` | Arguments | Answer |
|---|---|---|
| `marker.list` | | all markers and regions, by position |
| `marker.add` | `position?` (the edit cursor), `end?` (makes a region), `name?`, `color?` | the marker |
| `marker.edit` | `marker`, `region?`, `position?`, `end?` (regions only), `name?`, `color?` | the marker |
| `marker.delete` | `marker`, `region?` | `{}` |
| `marker.goto` | `marker`, `region?` | the transport state |

### Rendering

Renders use the project's render settings (`RENDER_FILE`, `RENDER_PATTERN`, `RENDER_RANGE`,
`RENDER_STEMS`, `<RENDER_CFG`); the selected track stands for "selected tracks". All are
admin-only.

| `cmd` | Arguments | Answer |
|---|---|---|
| `render.jobs` | | the files a render writes now: `path`, `start`, `end`, `source` (`kind`: `master`, `stem`, `track_via_master`, `item`, `item_via_master`; `track`) |
| `render.settings` | `source?` (`master`, `master_and_stems`, `stems`, `tracks_via_master`, `items`, `items_via_master`, `matrix`), `bounds?` (`project`, `time_selection`, `regions`, `selected_items`), `pattern?`, `file?` | the jobs, as `render.jobs` |
| `render.matrix` | `region`, `track` (id, name or `"master"`), `on` | `{}`: ticks the track for the region in the region render matrix |
| `render.project` | | `{}`; renders in the background, a notice says when it is done |
| `render.queue` | `dir?` (REAPER's `QueuedRenders`) | `{"queued"}`: the queued copy |
| `render.queue_list` | `dir?` | the queued projects, oldest first |
| `render.queue_run` | `dir?` | `{}`; renders every queued project in the background and removes each one that renders |

### Tempo map

Tempo and time signature markers (REAPER's tempo envelope) are numbered from 0 in time order.
A time signature always starts a bar, so a marker given one moves to the next bar line.

| `cmd` | Arguments | Answer |
|---|---|---|
| `tempo.list` | | `{"bpm", "time_signature", "markers": [{"index", "time", "bpm", "linear", "time_signature", "bar_beat"}]}`; `time_signature` is `[num, denom]` where a marker starts one, else `null` |
| `tempo.add` | `time`, `bpm?` (the tempo there), `time_signature?`, `linear?` | the tempo map; a map without markers first gets one at the start |
| `tempo.edit` | `index`, any of `time`, `bpm`, `time_signature` (`null` removes it), `linear` | the tempo map; changes of one marker within a second are one undo step |
| `tempo.delete` | `index` | the tempo map |

Tempo edits from any client move what follows the beat, as REAPER does: tempo markers after the
changed one keep their beat when `tempo_follows_beats` is on, and items, track and send envelope
points and markers move by their timebase (`project.timebase`, a track's `BEAT`, `item.timebase`):
`beats` keeps an item's start and end in quarter notes and changes its play rate to fit (or
spreads its stretch markers), `beats_position` moves only its start, `time` leaves it.

### Item timing

| `cmd` | Arguments | Answer |
|---|---|---|
| `item.stretch_markers` | `item` | `[{"index", "position", "source", "slope"}]` of the active take; `position` in seconds from the item start, `source` in seconds into the source |
| `item.stretch_markers.add` | `item`, `position`, `source?` (where the take plays there) | the markers |
| `item.stretch_markers.move` | `item`, `index`, `position?` (the audio stretches, the source position stays), `source?` (the marker pins another source position, REAPER's Shift-drag) | the markers; a marker stays between its neighbours |
| `item.stretch_markers.slope` | `item`, `index`, `slope` (-0.99 to 0.99) | the markers |
| `item.stretch_markers.delete` | `item`, `index?` (all when left out) | the markers |
| `item.timebase` | `item`, `timebase?`: `project`, `time`, `beats`, `beats_position` | `{"item", "timebase", "effective"}` |
| `take.channel_mode` | `item`, `take?` (the active one), `mode?`: `normal`, `reverse_stereo`, `downmix`, `mono`, `stereo`, `channel?` (first channel for mono and stereo, from 1) | `{"item", "take", "code", "label"}` with REAPER's `I_CHANMODE` code |
| `project.timebase` | `timebase?`: `time`, `beats`, `beats_position`; `tempo_follows_beats?` | `{"timebase", "tempo_follows_beats"}` |
| `project.playrate` | `rate?` (0.25 to 4), `preserve_pitch?` | `{"rate", "preserve_pitch"}`; playback and renders run at the rate |

These are admin commands.

### Envelopes and automation items

Track, send and FX parameter envelopes, their points and shapes, automation items and their
pools, automation modes and the global override. An envelope is named by `track`, `envelope`
(`volume`, `volume_pre_fx`, `trim`, `pan`, `mute`, `width`, `dual_pan_left`,
`dual_pan_right`, or `fx:<effect>:<parameter>` with REAPER's parameter order) and, for a send's
`volume`, `pan` or `mute`, `send` (its index in `send.list`); `item` names a take envelope
(`volume`, `pan`, `mute` or `pitch` of the item's active take, points in project seconds) and
`envelope: "tempo"` the tempo map (a point per tempo change; `square` jumps, `linear` ramps; any
track). Editing an envelope that does not exist creates it (an FX parameter envelope over the parameter's own range). Points are
`{"time", "value", "shape"?, "tension"?}` in seconds and the envelope's own units (volume as
gain, pan with REAPER's sign: positive is left); shapes are `linear`, `square`,
`slow_start_end`, `fast_start`, `fast_end` and `bezier`. Pool points are in quarter notes from
the pool's start with values 0..1 of the envelope's range. Edits are undo steps. Admin only.

| `cmd` | Arguments | Answer |
|---|---|---|
| `envelope.list` | `track` | every envelope of the track, its sends' and its effects': `[{"envelope", "send"?, "label", "active", "visible", "in_lane", "armed", "range", "fader_scaled", "points", "items"}]` |
| `envelope.get` | envelope | the envelope as in `envelope.list` |
| `envelope.set` | envelope, `active?`, `visible?`, `in_lane?`, `armed?` | the envelope |
| `envelope.remove` | envelope | `{}` |
| `envelope.point_add` | envelope, point | the envelope |
| `envelope.point_edit` | envelope, `index`, `time?`, `value?`, `shape?`, `tension?` | the envelope |
| `envelope.point_delete` | envelope, `index` | the envelope |
| `envelope.shape` | envelope, `shape`, `tension?`, `from?`, `to?` (seconds; all points when left out) | the envelope |
| `envelope.evaluate` | envelope, `time` | `{"value"}`, automation items included |
| `envelope.item_add` | envelope, `position`, `length`, `pool?` (a pooled copy), `lfo?` `{shape: sine\|triangle\|square\|saw_up\|saw_down, cycles, phase?}` | the envelope; without `pool` or `lfo` the new pool holds the envelope as it is there |
| `envelope.item_edit` | envelope, `index`, `position?`, `length?`, `start_offset?`, `rate?`, `baseline?` (0..1), `amplitude?` (-1..1), `looped?`, `transition?` (seconds), `selected?` | the envelope |
| `envelope.item_delete` | envelope, `index`, `keep_points?` | the envelope |
| `envelope.item_duplicate` | envelope, `index`, `pooled?` | the envelope, the copy right after the item |
| `envelope.item_unpool` | envelope, `index` | the envelope |
| `envelope.pool` | `pool`, `points?`, `lfo?`, `length_qn?`, `name?` | `{"pool", "name", "length_qn", "lfo", "points", "items"}`; every pooled item follows |
| `envelope.mode` | `track?` (every track when left out), `mode?` (`trim_read`, `read`, `touch`, `write`, `latch`, `latch_preview`), `send?` (with `track`: that send's own mode, `track` to follow the track again; REAPER's send `I_AUTOMODE`) | `[{"track", "mode", "effective"}]`, or `{"track", "send", "mode", "effective"}` |
| `envelope.latch_preview_apply` | `span` (`time_selection`, `to_end`, `to_start`) | `{"written"}`: the values the last latch preview pass remembered |
| `envelope.global_override` | `mode?` (`none`, the modes above, or `bypass`) | `{"mode"}` |

### Parameter modulation and learn

Parameter modulation, links and MIDI/OSC learn of effect parameters, stored beside the plugin as
REAPER stores them (`<PROGRAMENV`, `PARMLEARN`/`LFOLEARN`), and MIDI/OSC shortcuts to actions,
kept in `reaper-kb.ini` beside the preferences (`KEY <status> <data1> <action> 0` as REAPER
writes MIDI shortcuts; OSC ones as `SCYTHE_OSC <address> <action>`). `param` counts in REAPER's
order from 0 (a JSFX's sliders in order). Edits are undo steps. Admin only.

| `cmd` | Arguments | Answer |
|---|---|---|
| `fx.modulation.list` | `track`, `index` | `[{"param", "name", "modulation"?, "learn"?}]` of the modulated or learned parameters |
| `fx.modulation.set` | `track`, `index`, `param`, `enabled?`, `base?` (the parameter's own units), `lfo?` `{active, shape, strength, direction, hz \| period_qn, tempo_sync, free, phase}`, `audio?` `{active, channel, stereo, attack_ms, release_ms, min_db, max_db, strength, direction, curve_x, curve_y}`, `link?` `{active, fx, param \| midi, scale, offset}`; fields left out keep their value | the modulation |
| `fx.modulation.remove` | `track`, `index`, `param` | `{}` |
| `fx.learn.set` | `track`, `index`, `param`, `midi?` `{"type": "cc"\|"note"\|"pitch"\|…, "number", "channel"}`, `osc?`, `heard?`, `mode?` (`absolute`, `relative1`-`3`, `toggle`), `soft_takeover?`, `selected_track_only?`, `focused_fx_only?`, `visible_fx_only?`, `lfo_retrigger?` | the binding |
| `fx.learn.remove` | `track`, `index`, `param` | `{}` |
| `fx.learn.listen` | `on?` (default true) | `{"listening", "heard"}`: waits for the next MIDI message from an input enabled for control (`midiins_cs`; all enabled inputs when the key is absent) or OSC message from a surface with `learn` |
| `fx.learn.heard` | | `{"listening", "heard", "describe"}` |
| `action.learn.list` | | `[{"action", "midi"?, "osc"?, "describe"}]` |
| `action.learn.add` | `action`, `midi?` \| `osc?` \| `heard: true` | the binding |
| `action.learn.remove` | `action?`, `midi?`, `osc?` | `{"removed"}` |

A value is `baseline + LFO + audio follower + scale * (source + offset)` in the parameter's
0..1 range, clamped; a parameter envelope stands in for the baseline.

### Actions, shortcuts, toolbars and mouse modifiers

One action registry serves the palette, keyboard shortcuts, toolbars, mouse modifiers,
surfaces, learned controls, marker actions and ReaScript's `Main_OnCommand`. Actions are
REAPER command ids (named as REAPER 7.73's action list names them) or named commands:
Scythe's own (`_SCYTHE_SETLIST_NEXT`), custom actions (`_<id>`) and scripts registered as
actions (`_RS…`). Shortcuts, custom actions and scripts live in `reaper-kb.ini`, toolbars in
`reaper-menu.ini`, mouse modifiers in `reaper-mouse.ini`, beside the preferences and in
REAPER's formats. Actions that need the window answer an error on a headless server.

| Command | Fields | Answer |
|---|---|---|
| `action.list` | `query?` | `[{"command", "id", "name", "section", "runs": "session"\|"window", "kind": "builtin"\|"custom"\|"script", "shortcuts", "toggle"}]` |
| `action.run` | `action` | `{"ok"}` |
| `action.shortcut.add` | `key` ("Ctrl+Shift+A", "F5", "NumPad +"), `action` | `{"key", "action"}` |
| `action.shortcut.remove` | `key`, `disable_default?` | `{"removed"}` |
| `action.custom.set` | `id?`, `name`, `actions`, `consolidate_undo?` (default true) | `{"command", "id"}` |
| `action.script.add` | `path` | `{"command", "id"}` |
| `action.remove` | `action` (a custom or script action) | `{"ok"}` |
| `action.keymap.export` | | `{"text"}`: the shortcut file as a `.ReaperKeyMap` |
| `action.keymap.import` | `text` | `{"lines"}` |
| `toolbar.get` | `toolbar` (`main` or 1-32) | `{"toolbar", "title", "customized", "buttons": [{"action", "text", "icon", "name", "toggle"}]}` |
| `toolbar.set` | `toolbar`, `buttons: [{"action", "text?", "icon?"}]`, `title?` | as `toolbar.get`; a button without `text` gets the main toolbar's label for the action, else its name without the category |
| `toolbar.reset` | `toolbar` | `{"reset"}` |
| `toolbar.import` | `text` (a `.ReaperMenu` or `.ReaperMenuSet`) | `{"sections"}` |
| `mouse.get` | `context?` | `[{"context", "modifiers", "assignment"}]`: what differs from the defaults |
| `mouse.set` | `context` (`MM_CTX_ITEM_DBLCLK`…), `modifiers` (1 Shift, 2 Ctrl, 4 Alt, 8 Logo), `action?` | the assignment; no `action` resets it |

### Control surfaces

OSC devices, Mackie Control (and extenders) and HUI, kept in the preferences as REAPER keeps
them in reaper.ini (`csurf_cnt`, `csurf_<i>=<TYPE> <config>`); changes apply at once and the
running session opens, closes and reopens the devices. Admin only.

| `cmd` | Arguments | Answer |
|---|---|---|
| `surface.list` | | `[{"index", "line", "description", "surface", "running", "error"?, "port"?}]`; `line` as in reaper.ini, `port` the UDP port an OSC surface listens on |
| `surface.add` | `surface` | the list |
| `surface.set` | `index`, `surface` | the list |
| `surface.remove` | `index` | the list |
| `surface.devices` | | `{"inputs", "outputs"}`: MIDI port names indexed by the stable device numbers `input` and `output` take (as `midi.devices` numbers them; empty where no device is present) |
| `surface.action_list` | | `[{"id", "name", "runs"}]`: every action of the main section (see `action.list`) |
| `surface.run_action` | `action` (`"40364"`, `"_SCYTHE_SETLIST_NEXT"`) | `{}` |

A `surface` is one of:

- `{"type": "osc", "name", "local_port", "device_ip"?, "device_port", "receive"?, "send"?, "learn"?, "pattern"?, "max_packet"?, "wait_ms"?}`:
  `pattern` names a `.ReaperOSC` file in `$XDG_CONFIG_HOME/scythe/OSC` or REAPER's
  `~/.config/REAPER/OSC` (or a path); empty for Scythe's default, which has REAPER's addresses.
- `{"type": "midi", "kind", "input", "output", "offset"?}` with `kind` `mcu`, `mcu_extender`,
  `hui`, `faderport` or `faderport2` (the FaderPorts are kept but not driven yet); `offset`
  moves the first strip on by that many tracks (8 for an extender right of an MCU).
- `{"type": "line", "line": "MCU 0 0 2 3 0"}`: a line copied from REAPER's reaper.ini.

### Audio device

Admin only. As the desktop app's audio device window (REAPER's Preferences > Audio > Device).

| `cmd` | Arguments | Answer |
|---|---|---|
| `audio.devices` | | `[{"system", "devices": [{"id", "name", "input", "output"}]}]`: every audio system (`ALSA`, `JACK`, `Dummy`) and its devices |
| `audio.device` | | `{"system", "output", "output_id", "input", "sample_rate", "block_size", "output_latency", "silent", "reason", "inputs", "outputs", "requested"}`: what plays, the channels by the names tracks show (aliases included) and what was asked for |
| `audio.set_device` | any of `system`, `output`, `input` (an id from `audio.devices` or a name; empty for the default), `sample_rate`, `block_size` (0 for the device's own), `output_channels`, `input_channels` (0 for all of an interface, else a pair), `bits` (ALSA, PulseAudio: 16, 24, 32), `periods` (ALSA, 2-8: the blocks the device ring holds), `save` (default true: kept in the preferences) | the new `audio.device`; the transport stops and the project stays open. When the device cannot be opened the previous one plays on and the error says why |
| `audio.channel_name` | `kind` (`input`, `output`), `channel` (from 1), `name` (empty clears it) | `audio.device`; the alias is kept as REAPER keeps it and aliasing turns on for that side |
| `audio.buffering` | | REAPER's Buffering settings and how anticipative FX is doing: `anticipative`, `render_ahead_ms`, `threads` (0: one per performance core), `live_multiprocessing`, `running`, `ahead_ms`, `misses` (blocks that found nothing rendered ahead), `switches` (changes of the live tracks played through), `live_tracks` |
| `audio.set_buffering` | any of `anticipative`, `render_ahead_ms` (10-2000), `threads`, `live_multiprocessing` | the new `audio.buffering`; kept in the preferences |
| `audio.channel_map` | `kind`, `channels`: `[{"hardware": 3, "name"?}, {"hardware": null}, ...]` (hardware channels from 1, `null` for not connected; an empty list resets) | `audio.device`; the channels tracks see, each playing its hardware channel (REAPER's channel remapping), kept as REAPER keeps it; aliasing/remapping turns on for that side |

### Agents

Admin only. The AI agents connected over MCP and what they do (see [mcp.md](mcp.md#watching-what-agents-do)).
Times are milliseconds since the Unix epoch.

| `cmd` | Arguments | Answer |
|---|---|---|
| `agents.activity` | `calls` (how many of the newest; all kept, up to 200, when left out) | `{"clients": [{"id", "name", "version", "transport", "role", "connected_ms", "active_ms", "status", "status_ms", "running"}], "calls": [{"id", "client", "agent", "tool", "summary", "started_ms", "duration_ms", "ok", "error", "undo": [labels], "changed"}], "paused", "busy", "status", "undo": {"label", "agent"}}`; `undo` is there while the last undo step is an agent's |
| `agents.pause` | `paused` (default true) | `{"paused"}`: while paused, agents' commands that change anything are refused |
| `agents.undo` | | `{"label", "undo"}`: undoes the last step if an agent made it, else fails |
| `agents.disconnect` | `client` (all when left out) | `{"disconnected"}`: the client's calls are refused until it opens a new MCP session |

The MCP servers report with the rest; other clients have no need to send them:

| `cmd` | Arguments | Answer |
|---|---|---|
| `agents.hello` | `client`, `name`, `version`, `transport` (`http`, `stdio`), `ttl` (seconds it stays listed) | `{"paused"}`; refused for a disconnected client |
| `agents.bye` | `client` | `{}` |
| `agents.status` | `client`, `name`, `version`, `transport`, `text` (empty clears) | `{"status"}` |
| `agents.call` | `client`, `name`, `version`, `transport`, `tool`, `command` (the `cmd` it runs, if any), `arguments` | `{"call", "paused"}`: a call starts |
| `agents.run` | `call`, `command` | the command's answer; refused while paused unless it only reads; its undo steps are marked as the agent's. `agents.*` but `agents.activity` are refused |
| `agents.done` | `call`, `error` | `{}` |

### MIDI devices and synchronization

Devices keep stable numbers, as REAPER's `reaper-midihw-linux.ini` keeps them beside
reaper.ini; tracks name their MIDI inputs and outputs by these numbers. Enabling a device and
the clock settings are reaper.ini's `midiins`, `midiouts`, `midiouts_clock`,
`midiouts_clock_nospp` and `midiouts_noreset` masks; aliases are the cache's `ia`/`oa` lines.

| `cmd` | Arguments | Answer |
|---|---|---|
| `midi.devices` | | `{"inputs": [...], "outputs": [...]}`, each `{"number", "name", "alias", "present", "enabled"}`, outputs also `clock`, `spp`, `reset` |
| `midi.rescan` | | the devices, after looking for ports again |
| `midi.device` | `kind` (`input`, `output`), `number`, any of `enabled`, `alias` (empty clears it), and for outputs `clock`, `spp`, `reset` | the devices |
| `midi.keyboard` | `note`, `velocity` (0 releases), `channel?` (1-16) | `null`; plays the virtual MIDI keyboard (device 62) |
| `sync.get` | | `{"enabled", "source" ("mtc", "ltc", "clock"), "device", "channel", "offset", "resync_ms", "freewheel_ms", "frame_rate", "clock_outputs"}` |
| `sync.set` | any of `enabled`, `source`, `device` (63 for all inputs), `channel` (LTC audio input from 0), `offset` (seconds subtracted from the timecode), `resync_ms`, `freewheel_ms`, `frame_rate` (24, 25, 29.97, 30) | the synchronization; chasing MTC or LTC is the project's `SMPTESYNC` line, chasing MIDI clock (Scythe's own) a project extension state |

### MIDI editing

Times are quarter notes from the start of the item's active take; the note and lane commands
work on the selected notes (all when none is selected) and answer with the item's notes or
lane events.

| Command | Fields | Answer |
|---|---|---|
| `midi.notes.list` | `item` | `[{"index", "start", "end", "key", "velocity", "channel", "selected", "muted", "name"?, "notation"?}]` |
| `midi.notes.add` | `item`, `notes`: `[{"start", "end"? or "length"?, "key", "velocity"?, "channel"?, "muted"?}]` | the notes |
| `midi.notes.set` | `item`, `indexes`, any of `start`, `end`, `key`, `velocity`, `channel`, `muted`, `selected` | the notes |
| `midi.notes.delete`, `midi.notes.select` | `item`, `indexes` | the notes |
| `midi.quantize` | `item`, `grid?` (the take's), `swing?` (-1 to 1), `strength?` (%), `what?` (`position`, `position_and_end`, `end`, `length`), `window_min?`, `window_max?` (% of the grid) | the notes |
| `midi.humanize` | `item`, `timing?` (% of the grid), `velocity?` (%), `grid?`, `seed?` | the notes |
| `midi.transpose` | `item`, `semitones`, `scale?`, `root?` (scale steps when a scale is given) | the notes |
| `midi.edit` | `item`, `op` (`legato`, `legato_relative`, `split_grid`, `split`, `join`, `length`, `lengthen`, `move`, `velocity`, `channel`, `mute`, `unmute`, `toggle_mute`, `duplicate`, `force_key`, `select_all`, `select_none`), `value?`, `relative?`, `grid?`, `scale?` | the notes |
| `midi.action` | `item`, `command` (a REAPER MIDI editor command id, such as 40469), `scale?`, `root?` (the project's key when left out) | the notes |
| `midi.key` | `root?` (0-11), `scale?` | the project's key signature, `{"root", "scale", "pattern"}`; set when given (REAPER's `<KEYSIG`) |
| `midi.actions`, `midi.scales` | (`files?` for scales: `.reascale` paths) | the actions with REAPER's names; the scales |
| `midi.cc.list` | `item`, `lane?` (`cc0`-`cc127`, `cc14:0`-`cc14:31`, `pitch`, `program`, `pressure`, `poly`) | `[{"index", "lane", "at", "value", "channel", "key"?, "shape", "tension"?, "selected", "muted"}]` |
| `midi.cc.set` | `item`, `lane`, `points`: `[{"at", "value", "channel"?, "key"?, "shape"?, "tension"?}]`, `from?`, `to?` | the lane |
| `midi.cc.line` | `item`, `lane`, `from`, `to`, `start_value`, `end_value`, `shape?`, `tension?`, `channel?` | the lane: two events joined by a CC shape |
| `midi.cc.shape`, `midi.cc.delete` | `item`, `lane`, `indexes` (`shape`, `tension?` for the first) | the lane |
| `midi.sysex.list`, `midi.sysex.add`, `midi.sysex.delete` | `item`; `at` and `hex` or `text` with `kind` (1-15); `indexes` | `[{"index", "at", "kind", "text"?, "hex"}]` |
| `midi.groove` | `items`, `file` or `groove` (`.rgt` text), `strength?`, `velocity_strength?`, `target?` (`notes`, `items`) | the notes, or the items' positions |
| `midi.groove.extract` | `item`, `beats?`, `file?` | `{"groove"}`: the `.rgt` text |
| `midi.editor.get`, `midi.editor.set` | `item`; `mode?` (`piano_roll`, `named_notes`, `event_list`), `grid?`, `swing?`, `key_snap?`, `channel?`, `show_channels?`, `lanes?` | the settings, stored in the take as REAPER stores them |
| `midi.note_names.get`, `midi.note_names.set` | `track`; `names?` (`[{"key", "name", "channel"?}]`), `file?` (a note name file) | the track's note names |

### Access links

Answered by the server, not the session; all but `access.whoami` need the admin token.

| `cmd` | Arguments | Answer |
|---|---|---|
| `access.whoami` | | the caller's role: `{"role": "musician", "musician": "Drummer"}` |
| `access.list` | | `{"links": [link…], "admin_token", "stored", "origins"}`; `origins` are addresses phones can reach this server at (empty when it listens on loopback only) |
| `access.create` | `role` (`musician` or `stage`), `musician` (for a musician), `name?` | the link |
| `access.update` | `id`, `name?`, `musician?` | the link |
| `access.renew` | `id` | the link with a new token; the old one stops working |
| `access.delete` | `id` | `{"links"}` left |

A link: `{"id", "name", "role", "musician"?, "token", "created", "path", "urls"}`; `path` is the
page it opens with its token, such as `/monitor?token=…`, and `urls` that page at each address
in `origins`: the payload of the link's QR code.

### Media

Admin only: paths name files on the machine running Scythe, anything its process may read.

| `cmd` | Arguments | Answer |
|---|---|---|
| `media.import` | `paths` (audio files, `.mid` files), `track?` (else a new track per file, named after it), `position?` (seconds, 0), `sequence?` (all on one track, one after another), `gap?` (seconds between them), `name?` (the new track's) | `{"items": [{"track", "track_name", "item", "path", "position", "length", "kind": "audio"\|"midi"}], "end"}`; each file is one undo step (`Import media`, `Import MIDI`); MIDI becomes in-project MIDI items as REAPER imports them |
| `media.probe` | `path` | a job (see below) whose result is `{"kind", "format", "sample_rate", "channels", "frames", "duration", "exact", "integrated_lufs", "true_peak_dbtp", "loudness_range_lu", "sample_peak_dbfs"}` (for MIDI: `{"kind": "midi", "length_qn"}`) |

An item's length comes from the file's header for WAV, AIFF, CAF and FLAC, and from decoding it
for codecs with encoder delay (MP3, AAC, Vorbis, Opus), so it matches what REAPER gives the item.

### Media explorer

The media explorer's folders and REAPER media databases. The shortcut list is kept in the
`[reaper_sexplorer]` section of `preferences.ini` (`NbShortcuts`, `ShortcutN`, `ShortcutTN`) and
the databases in `MediaDB/NN.ReaperFileList` beside it, in REAPER's own formats, so REAPER's can
be copied over. A place is `folder` (a path) or `database` (a file name such as
`00.ReaperFileList`, or the title in the shortcut list).

| `cmd` | Arguments | Answer |
|---|---|---|
| `media.shortcuts` | | `{"shortcuts": [{"index", "name", "target", "database"}], "preview": {settings}}` |
| `media.add_shortcut` / `media.remove_shortcut` | `folder` / `index` | `{"index"}` / `{}` |
| `media.create_database` | `name`, `folders` | `{"database", "files"}`: the folders and their subfolders scanned |
| `media.scan_database` | `database`, `rescan?` (read every file again), `remove_missing?` | `{"database", "added", "removed", "files"}` |
| `media.browse` | `folder` or `database` | `{"entries": [{"path", "name", "folder", "size", "modified", "favorite", "metadata": {"length", "sample_rate", "channels", "bits", "bitrate_kbps", "title", "artist", "album", "date", "genre", "comment", "description", "bpm", "key"}}]}` |
| `media.search` | `folder` or `database`, `query`, `subfolders?` (folder searches, default on) | as `media.browse`: every word in the file name or metadata, `-word` excluded, `"two words"` together |
| `media.favorite` | `path`, `on?` (default true), `database?` | `{}`; in a database the `FILE` line's favourite flag, else Scythe's list of favourites |
| `media.preview_settings` | `tempo_match?` (`off`, `on`, `half`, `double`), `pitch?` (semitones), `volume_db?`, `repeat?`, `preserve_pitch?` | the settings |
| `media.preview` | `path`, `start?` (seconds) | `{"path", "rate"}`: plays through the preview player, straight to the device after the master (never with `--null-audio`); tempo matched at `rate`, the project tempo at the edit cursor over the file's BPM |
| `media.preview_stop` / `media.preview_state` | | `{}` / `{"path", "playing", "position", "length"}` |
| `media.insert` | `path`, `track?` or `new_track?` | `{"track", "item", "position", "length", "playrate"}`: at the edit cursor, on the selected track (else a new one); tempo matched like the preview (the take's rate, pitch kept) |
| `media.action` | `command` (a media explorer action, keymap section 32063), `path?` (the selected file), `folder?`/`database?` | the preview settings |

The explorer section's actions: 1004 go to parent folder, 1008 preview play, 1009 stop, 1010
pause, 1068 toggle repeat, 40018 refresh, 40019-40023 tempo match off/on/half/double/toggle,
40024 play/stop, 40068 preserve pitch, 41000 insert into project, 41001 insert on a new track,
42006 scan database for new files, 42009 remove missing files, 42084 set as favorite.

### Project bay

| `cmd` | Arguments | Answer |
|---|---|---|
| `bay.media` | | `{"media": [{"file", "path", "exists", "takes", "items", "tracks": [{"id", "name"}]}]}` |
| `bay.items` | | `{"items": [{"item", "track", "track_name", "position", "length", "take", "file"}]}` |
| `bay.fx` | | `{"fx": [{"name", "instances", "tracks"}]}` |
| `bay.retarget` | `from`, `to` | `{"takes"}`: every take playing `from` plays `to` (one undo step); a file in the project folder is stored relative |
| `bay.unused` | | `{"folder", "total", "unused"}`: REAPER's "Clean current project directory" list |
| `bay.clean` | `files?` (default every unused file), `trash?` (default true: the freedesktop trash) | `{"removed", "trash"}` |

"Clean current project directory" looks in the folder recordings go to (`RECORD_PATH`, the
project folder when empty), not its subfolders, and lists media and peaks files there the project
does not use, as REAPER 7.73 does: `wav bwf w64 aif aiff caf sd2 raw pcm flac mp3 mp2 mpg ogg opus
wv m4a mka mkv mov mp4 m4v qt avi webm wma gif mid midi kar rmi syx rpp-prox reapindex reapeaks`
(a `.reapeaks` file only when the file it belongs to is unused); projects, backups, text and
images are left alone.

### Theme

| `cmd` | Arguments | Answer |
|---|---|---|
| `theme.get` | | `{"colors": [{"key", "label", "color", "default", "reaper_key"}], "default"}` |
| `theme.set` | `colors` (`{"accent": "#ff8800", ...}`) | as `theme.get` |
| `theme.reset` | | Scythe's colours |
| `theme.load` | `path`: `.ScytheTheme`, `.ReaperTheme` or `.ReaperThemeZip` | as `theme.get`, with `loaded` (the keys taken from the file) |
| `theme.save` | `path` | `{"path"}` |

The theme is kept in `theme.ScytheTheme` beside the preferences (`[scythe theme]`, `key=#rrggbb`).
From a REAPER theme only colours are taken: `col_arrangebg` (background), `col_main_bg2` (panels),
`col_tr1_bg` and `selcol_tr1_bg` (track lanes), `col_main_text2`, `col_tl_fg`, `col_tl_fg2`
(text), and the envelope colours `col_env1` (volume, green), `env_trim_vol` (trim, yellow),
`col_env3` (pan, the orange accent), `env_track_mute` (mute, pink); button shades are derived from
the panel and text colours. REAPER stores colours as `COLORREF` (red in the low byte; bits above
24 are flags), which REAPER 7.73's `GetThemeColor` confirms.

### Analysis

Measuring takes seconds, so these commands start a job on a thread of its own and answer at
once with `{"job", "kind", "status": "running"}`, or `"done"` with the `result` when it was
measured before. `analysis.job` answers `running` (with `elapsed`), `done` (with `result`,
`seconds`) or `failed` (with `error`). MCP tools wait for the job and answer with the finished
one. Mixes are rendered offline through the render path (effects, sends, automation, the
project's play rate), never played; the session goes on meanwhile. Files' measurements are
kept in `$XDG_DATA_HOME/scythe/analysis-cache`, keyed by path, size, modification time and
range, so a reference is measured once; nothing leaves the machine.

| `cmd` | Arguments | Result |
|---|---|---|
| `analysis.file` | `path`, `start?`, `end?` (seconds into the file) | a measurement |
| `analysis.mix` | `target?` (`"master"`, a track id or name, a folder renders as its bus, or `{"group": n}` for a track group soloed through the master), `start?`, `end?` (project time; the project's end by default) | a measurement |
| `analysis.job` | `job` | the job's state; a finished `match_eq` with `apply` applies its EQ now (once) and reports it as `applied` |
| `analysis.compare` | `reference`, `mix`: each a subject | `{"reference", "mix"}` (the measurements without bands) and `comparison` |
| `analysis.match_eq` | `reference`, `mix`, `apply?` (track or `"master"`), `max_bands?` (6, at most 12), `max_gain_db?` (6, at most 12), `min_hz?`, `max_hz?` (30 to 16000) | `{"bands": [{"kind", "freq", "gain_db", "bandwidth"}], "max_error_before_db", "max_error_after_db", "rms_error_before_db", "rms_error_after_db", "curve": [{"hz", "target_db", "eq_db"}], "dynamics", "loudness_lufs", "true_peak_dbtp"}` |
| `analysis.apply_eq` | `track`, `bands` (as `match_eq` gives them; gains within ±12 dB) | not a job: adds a ReaEQ with these bands at the end of the chain, one undo step (`Match EQ`): `{"track", "fx_index", "bands"}` |
| `analysis.segments` | `path` or `item` (its active take's file, in project time), `threshold_db?` (-50), `min_silence?` (2 s), `min_length?` (5 s), `markers?`, `name?` ("Song") | `{"duration", "segments": [{"start", "end", "length", "peak_db", "loudness_lufs", "onsets", "bpm"}], "ignored", "markers": [{"position", "name"}], "added"}`; the proposed markers are a song marker at each start and a `!1016` stop marker just after each end; with `markers` they are added as one undo step (`Song markers`) at once, without a job |

A subject is `{"job": n}` (a finished `analysis.file` or `analysis.mix`), `{"file": path,
"start"?, "end"?}` or `{"mix": target, "start"?, "end"?}`.

A measurement: `integrated_lufs`, `short_term_max_lufs`, `momentary_max_lufs` (BS.1770-4 with
gating), `loudness_range_lu` (EBU Tech 3342), `true_peak_dbtp` (4x oversampled),
`sample_peak_dbfs`, `rms_dbfs`, `crest_db` (sample peak over RMS), `plr_db` (true peak over
integrated loudness), `correlation` and `side_db` (side over mid), `centroid_hz`,
`brightness_pct` (power above 4 kHz), `tilt_db_per_octave` (0 for pink noise, +3 for white),
`transients_per_second`, `low_end` (`{"below_hz": 120, "correlation", "side_db",
"mono_safe"}`) and `bands`: 31 third octaves from 20 Hz to 20 kHz with `level_db` (mean power of
the mid signal), `tonal_db` (smoothed over about an octave and less the integrated loudness, so
mixes of different loudness compare by shape), `side_db` and `correlation`. Silence reads -150.

A comparison (the mix less the reference): `loudness_diff_lu`, `true_peak_diff_db`,
`crest_diff_db`, `plr_diff_db`, `loudness_range_diff_lu`, `side_diff_db`,
`tilt_diff_db_per_octave`, `centroid_ratio`, `transients_diff_per_second`, `regions` (sub, low
end, low mids, mids, presence, air: average tonal difference), `bands` (`diff_db`,
`side_diff_db`), `max_tonal_diff_db` (40 Hz to 16 kHz) and `summary`: what stands out, in
plain sentences.

`match_eq` fits low and high shelves and peaks (ReaEQ's filters, the response computed from the
same design) greedily, then refines their gains together, stopping when what is left is under
0.25 dB or a band no longer helps; the level is left to the fader. `dynamics` advises:
`loudness_gap_lu`, `limiter_drive_db` (at most 6 dB per step), `limiter_ceiling_db` (-1 to -2
dBTP), a `compressor` (`ratio`, `attack_ms`, `release_ms`, `target_reduction_db`) when the mix's
peak-to-loudness ratio is more than 3 dB above the reference's, and `notes`.

### Events for polling

| `cmd` | Arguments | Answer |
|---|---|---|
| `events.since` | `since?` | `{"latest", "missed", "revision", "transport", "events": [{"event", …, "seq", "time_ms"}]}`: the events numbered after `since` (the last 256 are kept): `project_changed` (consecutive ones folded into the last), `notice`, `setlist`, `transport` (only changes, not the moving position), `health` (when it changes). Pass `latest` as `since` next time; `missed` says some were dropped in between |
| `events.meters` | | `{"master": {"peak_db": [l, r]}, "tracks": [{"id", "name", "peak_db"}], "mixes": [{"id", "musician", "peak_db"}], "health", "transport"}`, peaks in dBFS decaying like the UI's meters |

### Scripts

| `cmd` | Arguments | Answer |
|---|---|---|
| `script.run` | `code`, `language?` (`lua`, `eel`) | `{"console", "changed", "undo"}`: runs ReaScript text over the project (the `reaper.*` functions Scythe implements); its changes are one undo step named by `Undo_EndBlock`; a runtime error answers `failed` but keeps what the script changed before it, as REAPER does. Admin only. Only the desktop app runs scripts (`failed` from the headless server); message boxes answer OK, user inputs are cancelled, and a script that defers is stopped after its first pass |

### Setlists

A live project holds every song on one timeline: a song starts at a marker and runs to the
next stop marker (`!1016`, a marker action that stops playback when crossed). With stop
markers present, the first plain marker after a stop begins a song and plain markers inside it
are its cues; without any, every plain marker begins a song running to the next. Setlists are
stored in the project (`<EXTSTATE`, section `SCYTHE_SETLISTS`), so REAPER keeps them.

`setlist` names a setlist by its index from `setlist.list` or its exact name; left out, the
setlist being played. Songs in `songs` are an index from `setlist.songs`, or the start marker's
`{GUID}` or name (any case). `entry` is a position in the setlist, from 0.

| `cmd` | Arguments | Answer |
|---|---|---|
| `setlist.songs` | | `[{"index", "number", "guid", "name", "start", "end", "duration", "ends_at", "sections": [{"name", "position"}]}]`; `ends_at` is `stop_marker`, `next_song` or `project_end` |
| `setlist.list` | | `{"setlists": [setlist…], "current", "auto_advance", "marker_actions"}` |
| `setlist.get` | `setlist?` | `{"index", "name", "entries": [{"name", "guid", "song", "duration"}], "duration", "count_in"}` (`song` is `null` when the project has no such song) |
| `setlist.create` | `name?`, `songs?` | the setlist, which becomes the one played |
| `setlist.rename` | `setlist?`, `name` | the setlist (a name already used gets a number) |
| `setlist.duplicate` | `setlist?`, `name?` | the copy, placed after it |
| `setlist.delete` | `setlist?` | `{}` |
| `setlist.set` | `setlist?`, `songs` | the setlist with these songs |
| `setlist.move` | `setlist?`, `from`, `to` | the setlist |
| `setlist.import` | `text`: a title line, then one song per line (numbering and bullets optional) | the new setlist, songs matched by name |
| `setlist.export` | `setlist?` | `{"text"}` in the same form |
| `setlist.select` | `setlist` | status; makes it the setlist played, its first song current (nothing starts playing) |
| `setlist.cue` | `entry` | status; that song plays next (the edit cursor moves to it when stopped) |
| `setlist.play` | `entry?` (the current one) | status; plays the song from its marker; it stops at its stop marker |
| `setlist.stop` | | status |
| `setlist.next`, `setlist.previous` | | status; cues the song when stopped, plays it when playing |
| `setlist.auto_advance` | `enabled?` | status; after a song stops at its stop marker the next one is cued (not played) |
| `setlist.marker_actions` | `enabled?` | status; whether crossing a `!` marker runs its actions |
| `setlist.count_in` | `setlist?`, `enabled?` | the setlist; whether playing its songs counts in first (the project's count-in length, at the song's tempo), for bands whose drummer does not count |
| `setlist.status` | | `{"setlist", "name", "playing", "current", "next", "elapsed", "remaining", "auto_advance", "marker_actions", "count_in"}`; `current` and `next` are `{"entry", "name", "start", "duration"}`, `elapsed` and `remaining` are set while the current song plays |

## The snapshot

```json
{
  "revision": 42, "name": "monitors", "path": "/home/band/live/monitors.RPP", "dirty": false,
  "sample_rate": 48000, "length": 213.5, "bpm": 120.0, "time_signature": [4, 4],
  "bars": [0.0, 2.0, 4.0, …],
  "undo": "Track volume", "redo": null,
  "transport": {"playing": false, "recording": false, "position": 0.0, "bar_beat": "1.1.00", "cursor": 0.0,
                "loop_enabled": false, "loop_range": null, "metronome": false, …},
  "master": { "id": 0, "index": null, …same fields as a track… },
  "tracks": [{
    "id": 12, "index": 0, "guid": "{…}", "name": "Vox", "color": "#66d9ef",
    "volume": 0.6, "volume_db": -4.4, "pan": 0.0, "muted": false, "solo": false,
    "armed": true, "monitor": true, "record_mode": "input", "monitor_mode": "on", "input": 1024, "folder_depth": 0, "level": 0, "folder_compact": 0, "mixer_collapsed": false, "channels": 2,
    "fx": [{"index": 0, "name": "VST: ReaComp (Cockos)", "enabled": true, "running": true}],
    "sends": [{"index": 0, "dest": 15, "volume": 1.0, "volume_db": 0.0, "pan": 0.0, "muted": false,
               "mode": "post_fader", "mono": false, "phase": false, "audio": true,
               "src": 1, "src_count": 2, "dst": 1, "dst_mono": false, "midi": null}],
    "main_send": true,
    "hw_outs": [{"index": 0, "output": 2, "mono": false, "label": "Out 3/4", "mode": "post_fader",
                 "volume": 1.0, "volume_db": 0.0, "pan": 0.0, "muted": false, "phase": false}],
    "items": [{"id": 31, "track": 12, "name": "take.wav", "position": 1.0, "length": 4.0,
               "volume": 1.0, "muted": false, "takes": 1, "active_take": 0, "file": "…/take.wav"}]
  }],
  "mixes": […monitor mixes, as `mix.list`…],
  "markers": [{"number": 1, "region": true, "name": "Song 1", "position": 10.0, "end": 200.0,
               "color": null}],
  "tempo": […tempo markers, as `tempo.list`…]
}
```

`bars` holds where each bar starts (bar 1 first), following tempo and time signature changes, to
a minute past the end of the project; `bar_beat` is the position as REAPER shows it. `level` is
how deep in folders a track sits (0 at the top). Ids are stable while the session
runs (also across undo) but not across opening the project again; names and GUIDs are.

## Events

| `event` | Fields | When |
|---|---|---|
| `transport` | the transport state | about 20 times a second while playing or recording, and whenever play state, cursor, loop, metronome or count-in settings change, and each count-in beat |
| `meters` | `master: [l, r]`, `tracks: [{"id", "peak": [l, r]}]`, `mixes` (the monitor mixes' own meters, same shape plus `musician`) | about 20 times a second; peaks as gain, falling back like the UI's meters |
| `project_changed` | `revision` | after any change to the project, from any client or the UI; fetch a new snapshot when yours is older |
| `notice` | `message` | what the UI shows as a toast: saved, could not open, recording started… |
| `setlist` | the setlist status | when the setlist played, its current or next song, play state or options change (not as the song plays on; use `transport` for that) |
| `health` | the health state | every second, and at once when xruns are counted or the device goes or comes back |
| `agent_activity` | the `agents.activity` answer with the newest 30 calls | admin only; when an MCP agent connects, goes, says what it does or makes a call, and when the agent undo step changes (at most 10 a second) |

## Extending

The API lives in `crates/scythe-session/src/api/`, one module per domain, each with its own
command enum (`#[serde(tag = "cmd")]`, variants renamed `domain.action`) and an `execute`
function. A new domain needs a module, a variant of `api::Command` and one line in each of
its two `match`es (parsing by domain prefix and dispatching). Commands change the project with
`Session::edit` (one undo step) or `Session::edit_merging` (continuous controls), so the UI,
undo and `project_changed` events follow without more work. MCP clients reach a new domain
through `scythe_command` at once; deriving `JsonSchema` on its command enum and listing it in
`crates/scythe-mcp/src/tools.rs` turns its commands into tools of their own.

Who may run what is `scythe_session::access` (`Scope`, checked in `ControlPump` before a
command runs); a new command is admin-only until it is listed there. The server side is
`crates/scythe-server`: `ControlServer::start` binds, runs axum on a thread
and tokio runtime of its own, and returns a `ControlPump` that the thread owning the session
calls (`pump` every frame, or `wait` in a loop) to run commands and publish events. The
desktop app pumps it on a thread of its own (`crates/scythe-app/src/pump.rs`), taking turns
with the window for the session, so commands do not wait for a frame the compositor may never
give a hidden window. The server sets `TCP_NODELAY`: a reply written behind an event the client
has not acknowledged otherwise waits out its delayed ACK. The same
server answers MCP at `/mcp` with `scythe-mcp`'s `McpServer`.

The web UI is in `crates/scythe-server/web` (plain ES modules, no build step), embedded by
`crates/scythe-server/src/web.rs`, whose file list a test keeps in step with the directory.
Its client for the monitor-mix and setlist domains keeps their command and field names in
`web/js/domains.js`.
