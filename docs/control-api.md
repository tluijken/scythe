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

A token (`--token`, `--control-token` in the app, `token = "…"` in the config file, or
`SCYTHE_CONTROL_TOKEN`) makes every endpoint except `/api/health` require it, either as
`Authorization: Bearer <token>` or as a `?token=<token>` query parameter (for browsers opening
a WebSocket). Binding to anything other than a loopback address without a token prints a
warning: anyone on the network could then drive the session, including opening and saving
files. There is no TLS; put a reverse proxy in front for anything beyond a trusted LAN.

## Endpoints

| Method | Path | |
|---|---|---|
| `GET` | `/api/health` | `{"ok": true, "name": "scythe", "protocol": 1}`, no token needed |
| `POST` | `/api/command` | body: one command; answer: `{"ok": true, "result": …}` |
| `GET` | `/api/snapshot` | the same as the `snapshot` command |
| `GET` | `/api/ws` | WebSocket: commands in, replies and events out |

Errors answer `{"ok": false, "error": {"kind": …, "message": …}}` with HTTP status 400 for
`invalid` (malformed, unknown command, value out of range), 404 for `not_found` (a track, send,
effect, item or marker that does not exist), 409 for `failed` (understood but could not be
done: the file did not open, nothing to undo, no armed track to record) and 401 for
`unauthorized`.

### WebSocket

On connecting the server sends `{"type": "hello", "protocol": 1}`. A client sends commands as
JSON text messages, optionally with an `id` of any JSON type; each gets exactly one reply, in
the order the commands were sent:

```json
→ {"id": 7, "cmd": "track.volume", "track": "Vox", "db": -4.5}
← {"type": "reply", "id": 7, "ok": true, "result": {"id": 12, "name": "Vox", "volume_db": -4.5, …}}
← {"type": "reply", "id": 8, "ok": false, "error": {"kind": "not_found", "message": "no track with id 99"}}
```

Every connected client also receives every event (below) as `{"type": "event", "event": …}`. A
client that falls behind skips events rather than slowing the session down.

## Commands

A command is a JSON object whose `cmd` names `domain.action`; its other fields are its
arguments. Fields shown with `?` may be left out.

**Tracks** are named by `track`: the id from a snapshot (a number, stable for the session), the
track's exact name (a string), or `"master"` where the master makes sense (volume, pan, mute,
effects). **Levels** are given as `volume` (gain, 1.0 = 0 dB, capped at +24 dB) or `db`. **Pan**
runs from -1 (left) to 1 (right). Times are in seconds. Flags (`muted`, `solo`, `armed`,
`monitor`, `bypassed`) toggle when left out.

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
| `transport.metronome` | `enabled` | |
| `transport.state` | | |

### Project

| `cmd` | Arguments | Answer |
|---|---|---|
| `snapshot` (or `project.snapshot`) | | the whole project, below |
| `project.open` | `path` (on the machine running Scythe) | snapshot |
| `project.save` | `path?` (the project's own file when left out) | history state |
| `project.new` | | snapshot |
| `project.undo`, `project.redo` | | `{"revision", "dirty", "label", "undo", "redo"}`; `failed` when there is nothing to undo or redo |

### Tracks

| `cmd` | Arguments | Answer |
|---|---|---|
| `track.list` | | every track |
| `track.get` | `track` | the track |
| `track.add` | `name?`, `index?` (end when left out) | the new track |
| `track.delete` | `track` | `{}` |
| `track.rename` | `track`, `name` | the track |
| `track.move` | `track`, `index` | the track |
| `track.volume` | `track`, `volume` or `db` | the track |
| `track.pan` | `track`, `pan` | the track |
| `track.mute` | `track`, `muted?` | the track |
| `track.solo` | `track`, `solo?` | the track |
| `track.arm` | `track`, `armed?` | the track |
| `track.monitor` | `track`, `monitor?` | input monitoring: an armed track with monitoring on plays its input through its FX |
| `track.input` | `track`, `input` | record input in REAPER's `I_RECINPUT` numbering, inputs counted from 0: mono input *n* is *n*, the stereo pair from *n* is 1024 + *n*, MIDI is 4096 + device × 32 + channel (device 63 = all, channel 0 = all), -1 is none |
| `track.color` (or `track.colour`) | `track`, `color` (`"#rrggbb"` or `null`) | the track |
| `track.folder` | `track`, `depth` | REAPER's `I_FOLDERDEPTH`: 1 starts a folder holding the tracks below, -*n* closes *n* folders after this track, 0 is a plain track |

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
| `send.hw_list` | `track` (or `"master"`) | the hardware outputs |
| `send.hw_add` | `track`, `output?` (device output from 0; the next free one when left out), `mono?` | the new output |
| `send.hw_remove` | `track`, `hw` | the remaining outputs |
| `send.hw_set` | `track`, `hw`, any of `output`, `mono`, `mode`, `volume` or `db`, `pan`, `muted`, `phase` | the output |

`track.main_send` (`track`, `on?`) turns a track's feed to its parent or the master on or off;
a bus that only plays through hardware outputs has it off.

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
| `mix.add_source` | `mix`, `source`, `mode?` | the mix |
| `mix.remove_source` | `mix`, `source` | the mix |
| `mix.cue_bus` | `sources`, `name?`, `mode?`, `volumes_from_tracks?`, `pans_from_tracks?`, `output?`, `mono?` | the new bus as a track: SWS's "Create cue buss from track selection" |

A mix:

```json
{"id": 20, "musician": "Drummer", "name": "MON Drummer", "color": "#a6e22e",
 "volume": 1.0, "volume_db": 0.0, "more_me": 6.0,
 "output": {"output": 2, "mono": false, "label": "Out 3/4"}, "limiter": true,
 "sources": [{"track": 12, "name": "Kick", "color": "#66d9ef", "volume": 1.41,
              "volume_db": 3.0, "pan": 0.0, "muted": false, "mode": "post_fx", "own": true}]}
```

### Effects

Effects are numbered from 0 per chain; `track` may be `"master"`.

| `cmd` | Arguments | Answer |
|---|---|---|
| `fx.list` | `track` | `[{"index", "name", "enabled", "running"}]` (`running`: processing audio, not passed through) |
| `fx.catalog` | | `[{"id", "name", "format"}]`: built-in effects and installed CLAP, LV2 and VST 3 plugins (the first call scans for plugins) |
| `fx.add` | `track`, `plugin` (a catalog `id` such as `builtin:compressor`, or exact name), `index?` | the chain |
| `fx.remove` | `track`, `index` | the chain |
| `fx.bypass` | `track`, `index`, `bypassed?` | the chain |
| `fx.params` | `track`, `index` | `[{"id", "name", "value", "text", "min", "max", "default"}]` of a hosted plugin |
| `fx.param` | `track`, `index`, `param` (an id from `fx.params`), `value` (clamped to the range) | the parameter |

Plugin parameters live in the running plugin, as with the inspector's sliders: they mark the
project unsaved and are stored on save, but are not undo steps.

### Items

Items are named by their `id` from a snapshot or `item.list`.

| `cmd` | Arguments | Answer |
|---|---|---|
| `item.list` | `track?` | the items of the track, or of all tracks |
| `item.move` | `item`, `position?`, `track?` | the item (overlaps crossfade, as after a move in the UI) |
| `item.split` | `item`, `at` | both halves; `invalid` unless `at` lies inside the item |
| `item.delete` | `item` | `{}` |
| `item.volume` | `item`, `volume` or `db` | the item |
| `item.mute` | `item`, `muted?` | the item |

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
| `setlist.get` | `setlist?` | `{"index", "name", "entries": [{"name", "guid", "song", "duration"}], "duration"}` (`song` is `null` when the project has no such song) |
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
| `setlist.status` | | `{"setlist", "name", "playing", "current", "next", "elapsed", "remaining", "auto_advance", "marker_actions"}`; `current` and `next` are `{"entry", "name", "start", "duration"}`, `elapsed` and `remaining` are set while the current song plays |

## The snapshot

```json
{
  "revision": 42, "name": "monitors", "path": "/home/band/live/monitors.RPP", "dirty": false,
  "sample_rate": 48000, "length": 213.5, "bpm": 120.0, "time_signature": [4, 4],
  "undo": "Track volume", "redo": null,
  "transport": {"playing": false, "recording": false, "position": 0.0, "cursor": 0.0,
                "loop_enabled": false, "loop_range": null, "metronome": false},
  "master": { "id": 0, "index": null, …same fields as a track… },
  "tracks": [{
    "id": 12, "index": 0, "guid": "{…}", "name": "Vox", "color": "#66d9ef",
    "volume": 0.6, "volume_db": -4.4, "pan": 0.0, "muted": false, "solo": false,
    "armed": true, "monitor": true, "input": 1024, "folder_depth": 0, "level": 0, "channels": 2,
    "fx": [{"index": 0, "name": "VST: ReaComp (Cockos)", "enabled": true, "running": true}],
    "sends": [{"index": 0, "dest": 15, "volume": 1.0, "volume_db": 0.0, "pan": 0.0, "muted": false,
               "mode": "post_fader", "mono": false, "phase": false}],
    "main_send": true,
    "hw_outs": [{"index": 0, "output": 2, "mono": false, "label": "Out 3/4", "mode": "post_fader",
                 "volume": 1.0, "volume_db": 0.0, "pan": 0.0, "muted": false, "phase": false}],
    "items": [{"id": 31, "track": 12, "name": "take.wav", "position": 1.0, "length": 4.0,
               "volume": 1.0, "muted": false, "takes": 1, "active_take": 0, "file": "…/take.wav"}]
  }],
  "mixes": […monitor mixes, as `mix.list`…],
  "markers": [{"number": 1, "region": true, "name": "Song 1", "position": 10.0, "end": 200.0,
               "color": null}]
}
```

`level` is how deep in folders a track sits (0 at the top). Ids are stable while the session
runs (also across undo) but not across opening the project again; names and GUIDs are.

## Events

| `event` | Fields | When |
|---|---|---|
| `transport` | the transport state | about 20 times a second while playing or recording, and whenever play state, cursor, loop or metronome change |
| `meters` | `master: [l, r]`, `tracks: [{"id", "peak": [l, r]}]`, `mixes` (the monitor mixes' own meters, same shape) | about 20 times a second; peaks as gain, falling back like the UI's meters |
| `project_changed` | `revision` | after any change to the project, from any client or the UI; fetch a new snapshot when yours is older |
| `notice` | `message` | what the UI shows as a toast: saved, could not open, recording started… |
| `setlist` | the setlist status | when the setlist played, its current or next song, play state or options change (not as the song plays on; use `transport` for that) |

## Extending

The API lives in `crates/scythe-session/src/api/`, one module per domain, each with its own
command enum (`#[serde(tag = "cmd")]`, variants renamed `domain.action`) and an `execute`
function. A new domain needs a module, a variant of `api::Command` and one line in each of
its two `match`es (parsing by domain prefix and dispatching). Commands change the project with
`Session::edit` (one undo step) or `Session::edit_merging` (continuous controls), so the UI,
undo and `project_changed` events follow without more work.

The server side is `crates/scythe-server`: `ControlServer::start` binds, runs axum on a thread
and tokio runtime of its own, and returns a `ControlPump` that the thread owning the session
calls (`pump` every frame, or `wait` in a loop) to run commands and publish events.
