# MCP server

Scythe speaks the [Model Context Protocol](https://modelcontextprotocol.io), so AI agents such
as Claude Code and Claude Desktop can inspect and change a running session: read the project,
set levels, route sends, add effects, move items, run the transport, import media, measure a
mix against a reference and match it. Every tool is a
[control API](control-api.md) command underneath, so a change made by an agent is an undo step
in Scythe like any other and shows up live in its window.

Protocol revisions: `2025-11-25` (newest), `2025-06-18`, `2025-03-26` and `2024-11-05`, picked
by the client in `initialize`.

## Two ways in

| Transport | How |
|---|---|
| stdio | the `scythe-mcp` binary, started by the agent. It talks to a running Scythe's control API (`--url`, default `http://127.0.0.1:8770`, and `--token`), or with `--headless [project.RPP]` runs a session itself without a window (`--null-audio` opens no audio device) |
| Streamable HTTP | `http://ADDR/mcp` on every Scythe that serves the control API (`scythe-server`, `scythe --headless`, `scythe --control ADDR`) |

Build the binary with `devbox run -- scripts/cargo.sh build --release -p scythe-mcp`; it lands
in `target/release/scythe-mcp`. Scythe has to serve its control API for `--url` and for `/mcp`:
the headless server does by default on `127.0.0.1:8770`, the desktop app with
`--control 127.0.0.1:8770` or with "Start the web UI and MCP server with Scythe" in the audio
device window (Preferences), whose "Claude Code command" button copies the `claude mcp add` line
with its token.

### Claude Code

```sh
# a running Scythe (window or headless), through the binary
claude mcp add scythe -- /path/to/scythe-mcp --url http://127.0.0.1:8770

# the same with a token
claude mcp add scythe --env SCYTHE_CONTROL_TOKEN=secret -- /path/to/scythe-mcp

# or straight to Scythe's own endpoint, no binary needed
claude mcp add --transport http scythe http://127.0.0.1:8770/mcp
claude mcp add --transport http scythe http://studio.lan:8770/mcp --header "Authorization: Bearer secret"

# a session of its own, without a window
claude mcp add scythe -- /path/to/scythe-mcp --headless /home/me/songs/demo.RPP
```

### Claude Desktop

In `claude_desktop_config.json` (Settings, Developer, Edit Config):

```json
{
  "mcpServers": {
    "scythe": {
      "command": "/path/to/scythe-mcp",
      "args": ["--url", "http://127.0.0.1:8770"],
      "env": {"SCYTHE_CONTROL_TOKEN": "secret"}
    }
  }
}
```

Leave out `env` when Scythe runs without a token. For a session of its own use
`"args": ["--headless", "/home/me/songs/demo.RPP"]`.

## Tools

Every control API command `domain.action` is a tool `domain_action`, with its description and
argument schema generated from the command's Rust type (`schemars`), so the tools cannot drift
from the API. `crates/scythe-mcp/tests/tools.json` is a snapshot of the whole list.

| Group | Tools |
|---|---|
| Overview | `get_project`: a few lines per track (id, name, level, pan, flags, effects, sends, hardware outputs, items) and per monitor mix instead of the full JSON snapshot; `items: true` lists every item, `format: "json"` returns the snapshot |
| Transport | `transport_play`, `transport_pause`, `transport_stop`, `transport_record`, `transport_seek`, `transport_loop`, `transport_metronome`, `transport_state` |
| Project | `project_open`, `project_save`, `project_new`, `project_undo`, `project_redo` |
| Tracks | `track_list`, `track_get`, `track_add`, `track_delete`, `track_rename`, `track_move`, `track_volume`, `track_pan`, `track_mute`, `track_solo`, `track_arm`, `track_monitor`, `track_input`, `track_color`, `track_folder` |
| Sends | `send_list`, `send_add`, `send_remove`, `send_volume`, `send_pan`, `send_mute`, `send_mode`; hardware outputs `send_hw_list`, `send_hw_add`, `send_hw_remove`, `send_hw_set` |
| Effects | `fx_list`, `fx_catalog` (with `query` to search; answers at once with `scanning` and `progress` while the background plugin scan runs, so poll it until `scanning` is false; `failed` lists plugins that hung or crashed while scanned; `rescan: true` checks every plugin file anew), `fx_add`, `fx_remove`, `fx_bypass`, `fx_params`, `fx_param` |
| Guitar rigs | `rig_list`, `rig_get`, `rig_create`, `rig_convert`, `rig_set_amp`, `rig_set_cab`, `rig_stage`, `rig_set`, `rig_snapshot`, `rig_presets`, `rig_save_preset`, `rig_load_preset`, `rig_delete_preset` |
| Items | `item_list`, `item_move`, `item_split`, `item_delete`, `item_volume`, `item_mute` |
| Markers | `marker_list`, `marker_add`, `marker_edit`, `marker_delete`, `marker_goto` |
| Monitor mixes | `mix_list`, `mix_get`, `mix_create`, `mix_copy`, `mix_rename`, `mix_delete`, `mix_level`, `mix_pan`, `mix_mute`, `mix_own`, `mix_more_me`, `mix_volume`, `mix_output`, `mix_limiter`, `mix_add_source`, `mix_remove_source`, `mix_cue_bus` |
| Setlists | `setlist_songs`, `setlist_list`, `setlist_get`, `setlist_create`, `setlist_rename`, `setlist_delete`, `setlist_duplicate`, `setlist_set`, `setlist_move`, `setlist_import`, `setlist_export`, `setlist_select`, `setlist_cue`, `setlist_play`, `setlist_stop`, `setlist_next`, `setlist_previous`, `setlist_auto_advance`, `setlist_marker_actions`, `setlist_status` |
| Media | `media_import` (audio and MIDI files as items: onto a track, new tracks, or in sequence with gaps), `media_probe` (format, rate, channels, length, loudness, true peak) |
| Analysis | `analysis_file`, `analysis_mix` (the master, a track or bus, or a track group, rendered offline), `analysis_compare`, `analysis_match_eq` (and `apply` it as a ScyEQ), `analysis_apply_eq`, `analysis_masking` (which instrument groups bury each other, in which region and how often, each group rendered offline through its buses), `analysis_segments` (songs in a backing-track or click file, proposed song and `!1016` stop markers), `analysis_job` |
| Events | `events_since` (what happened since a number: project changes, notices, transport, setlist, device health), `events_meters` (peak meters of the master, tracks and monitor mixes, and the device's health) |
| Access links | `access_whoami`, `access_list`, `access_create`, `access_update`, `access_renew`, `access_delete`: links for musicians' mixes and the stage tablet, with `urls`, their QR code payloads (served by `scythe-server` and `scythe --control`) |
| Scripts | `script_run`: ReaScript text (Lua or EEL) in the desktop app, one undo step |
| Tool choice | `scythe_tools`: which domains are listed, and listing more or fewer |
| Status | `set_status`: what the agent is doing, shown in Scythe beside the agent light (see [Watching what agents do](#watching-what-agents-do)) |
| Anything else | `scythe_command`: any command as JSON (`{"command": {"cmd": "mix.…", …}}`), so new API domains work before they get tools of their own |

Results are the command's JSON answer; answers that are a whole project (`project_open`,
`project_new`) come back as the `get_project` overview. A command that fails (no such track,
value out of range, nothing to undo) is a tool result with `isError` and a message starting
with its kind (`invalid`, `not_found`, `failed`), so the agent can correct itself. A
command given fields it does not read still runs, and its result ends with a line naming them
(`ignored: "width" (not fields of this command, so they did nothing)`); a toggle given such a
field instead of its value (`track_mute` with `"mute"` rather than `"muted"`) fails as
`invalid`. The `get_project` overview tells the project's sample rate, the rate renders run at
and the audio device's apart.

Measuring tools (`analysis_*`, `media_probe`) start a job in Scythe and wait for it (up to 15
minutes), then answer with the finished job: `{"job", "status": "done", "result", "seconds"}`.
The job number names the measurement later (`{"job": n}` as a subject of `analysis_compare` or
`analysis_match_eq`), so a reference is measured once. Nothing is played: mixes are rendered
offline while the session carries on.

Tool annotations tell clients what to confirm: read-only tools (`*_list`, `*_get`,
`*_status`, `transport_state`, `fx_catalog`, `fx_params`, `setlist_songs`, `setlist_export`,
`get_project`, the measuring tools, `events_*`, `access_whoami`) say `readOnlyHint`; deleting
and removing, `project_open` and `project_new` (which drop unsaved changes), `project_save`
(which overwrites a file), `access_renew` (which locks out the phone holding the link),
`script_run` and `scythe_command` say `destructiveHint`. Toggles (mute, solo, arm, monitor,
bypass, own, limiter without a value) and steps such as `setlist_next` are not idempotent.

## Choosing the tools listed

Listing all of Scythe's 200 tools costs a model context it would rather spend on the session,
so by default a compact set is listed: about 40 tools for building a live session, mixing and
matching a reference (transport, save and undo, adding and naming tracks, levels and inputs,
sends and hardware outputs, effects, markers, monitor mixes and setlists in brief, media,
analysis, events), plus `get_project`, `scythe_command`, `scythe_tools` and `set_status`, which are
always there. Any tool can still be called by name, listed or not.

| Where | How |
|---|---|
| stdio | `scythe-mcp --tools default,mix,setlist`; `--tools all` for every tool |
| HTTP | `http://ADDR/mcp?tools=default,mix,setlist`, `?tools=all` |
| at run time | the agent calls `scythe_tools` with `enable: ["mix"]` or `disable: ["default"]`; clients get `notifications/tools/list_changed` and list again |

Domains: `transport`, `project`, `track`, `send`, `fx`, `group`, `item`, `marker`, `mix`,
`setlist`, `tempo`, `surface`, `midi`, `sync`, `take`, `lanes`, `record`, `render`, `action`,
`media`, `analysis`, `events`, `access`, `script` (`scythe_tools` without arguments lists them
with their sizes). `default` is the compact set, `all` everything.

## Resources, notifications and prompts

| URI | |
|---|---|
| `scythe://project/summary` | the `get_project` overview, items included |
| `scythe://project` | the full snapshot as JSON |
| `scythe://docs/control-api` | the control API reference |
| `scythe://docs/mcp` | this page |
| `scythe://docs/mixing` | [field notes on mixing](mixing.md): measuring against a reference, low end, drums, NAM guitars, vocals, character effects, pitfalls |

The server sends notifications on stdio between answers, and over HTTP on `GET /mcp` (a stream
of server-sent events for as long as the client holds it open):

- `notifications/resources/updated` for `scythe://project` and `scythe://project/summary`
  after `resources/subscribe`, when the project changes (from the agent, the window, a tablet);
- `notifications/tools/list_changed` when `scythe_tools` changes the list;
- `notifications/message` after `logging/setLevel`: notices (saved, could not load…),
  transport changes, setlist changes and the audio device's health (xruns, device lost, at
  `warning`), with the event itself in `data.event`.

Agents whose client shows no notifications poll `events_since` (pass the `latest` it answered
as `since` next time) and read `events_meters` for levels and the device's health.

Prompts:

| Prompt | Arguments | What the agent does |
|---|---|---|
| `mix_check` | `focus?` | inspects levels, routing and effects and proposes changes without making them |
| `monitor_mix` | `musician`, `request` | changes one musician's in-ear mix in steps of at most 3 dB and reports what it changed |
| `record_setup` | `tracks?` | arms tracks and checks inputs and monitoring |
| `build_live_session` | `band`, `outputs?`, `backing?`, `setlist?` | builds a live session step by step, checking each: input tracks, NAM and dynamics, folders and groups, hardware outputs, monitor mixes with limiters, backing tracks and click, song and stop markers, setlists, access links with their QR payloads, a footswitch |
| `setlist_from_backing_tracks` | `files`, `songs?` | probes and imports backing-track and click files, finds the songs in them, places song and `!1016` stop markers and makes the setlist |
| `match_reference` | `reference`, `target?`, `range?` | measures the reference and the mix, compares, applies small explained EQ and dynamics changes, measures again, and stops within tolerances (1 LU, -1 dBTP, 1.5 dB per region) or after three rounds |

## Walkthrough: a live session

With Scythe running (`scythe-server --null-audio` while preparing, the real interface on the
night), an agent given `build_live_session` with "Vox on input 1, guitar DI on 2, bass DI on
3, drummer on in-ears at outputs 3/4, click only to the drummer, backing tracks in
/gigs/backing" goes like this:

1. `get_project`, `events_meters`: an empty project, the device healthy.
2. `track_add` Vox, Gtr, Bass; `track_input` 0, 1, 2; arming and input monitoring through
   `scythe_command` (`track.arm`, `track.monitor`).
3. `fx_catalog {"query": "nam"}`, `fx_add` the Neural Amp Modeler on Gtr and Bass,
   `builtin:compressor` on Vox and Bass, `builtin:equalizer` for high-passes; `fx_param` to set them.
4. `media_probe` each backing file, `media_import` them onto "Backing" and the click onto
   "Click" (`sequence: true`, a gap of a few seconds); the click kept off the master (heard only in the monitor mixes).
5. `analysis_segments` on a long backing file finds the songs (with each click's tempo);
   `markers: true` places "Song n" and `!1016` stop markers; `marker_edit` names them.
6. `mix_create {"musician": "Drummer", "own": ["Click"]}`, `mix_level` per source,
   `mix_output` to outputs 3/4, the limiter on.
7. `setlist_create` with the songs in order, `setlist_status`.
8. `access_create {"role": "musician", "musician": "Drummer"}` and `{"role": "stage"}`: the
   answers' `urls` are what the QR codes hold.
9. `events_since` to see nothing failed, and a summary of what to check by ear.

Every step is an undo step, visible live in Scythe's window.

## Walkthrough: matching a reference

An example of what an agent does given `match_reference` with `/refs/album/03.flac` and the chorus at 60-120 s:

1. `analysis_file {"path": "/refs/album/03.flac"}` answers job 1: -9.2 LUFS, -0.8 dBTP, a
   loudness range of 5 LU, tilt -4.1 dB per octave.
2. `analysis_compare {"reference": {"job": 1}, "mix": {"mix": "master", "start": 60, "end":
   120}}`: "The mix is 5.3 LU quieter", "Tonal balance: 2.4 dB less in the low end (60-250
   Hz)", "1.6 dB more in the presence (3-8 kHz)".
3. `analysis_match_eq` with the same subjects, `max_gain_db: 4`, `max_bands: 5`, `apply:
   "master"`: a +2.2 dB low shelf at 100 Hz and a -1.5 dB peak at 4 kHz go onto the master as
   one ScyEQ (one undo step, `Match EQ`); `dynamics` suggests 5.3 dB of limiter drive with a
   -1 dBTP ceiling.
4. `fx_add` `builtin:limiter` on the master, `fx_param` its threshold and ceiling.
5. `analysis_compare` again: within 0.6 LU, every region within 1 dB. The agent stops and lists
   the changes with the numbers before and after.

The reference file is decoded and measured on the machine running Scythe; its measurement is
cached in `$XDG_DATA_HOME/scythe/analysis-cache` and nothing is uploaded anywhere.

## Watching what agents do

Whoever sits at Scythe sees when an agent is at work. A small light in the top bar, beside the
project's name (a badge on the logo when the name leaves no room), shows up while an MCP client
is connected: dim while it is idle, a soft orange pulse while its tool calls run and for two
seconds after, pink for a moment after a call failed, with a ring around it while agents are
paused. Its tooltip names the agent and what it says it is doing; a click opens the Agents
window:

- the connected agents: the name and version from `initialize`'s `clientInfo` (`title`, else
  `name`), the transport (`http` for `/mcp`, `stdio` for `scythe-mcp`), since when, and the
  status text the agent set;
- the activity: each tool call with when, what it did in words ("Set Vocals volume to -3 dB",
  else the command and its arguments), whether it worked (the error when not), how long it took
  and the undo steps it made; the last 200 calls are kept;
- **Undo last agent change**, enabled while the last undo step is an agent's;
- **Pause agents**: until resumed, tool calls that would change anything are refused with an
  error saying the user paused agents; reading (`get_project`, `*_list`, measuring) still works;
- **Disconnect** (one agent, or all): its calls are refused from then on and it is no longer
  listed; it comes back by opening a new MCP session (restarting the MCP connection).

The web editor's top bar shows the same light and panel to the admin.

### `set_status`

`set_status {"text": "Balancing drums against the reference"}` shows what the agent is doing
beside the light until it sets another text or clears it with `""`. The server instructions and
every prompt ask agents to call it before each step of a longer task.

### How clients are told apart

Both transports report to the session with `agents.*` control commands (see
[control-api.md](control-api.md#agents)), so the window, the web UI and the API see one picture:

- `/mcp` gives every `initialize` an MCP session (`Mcp-Session-Id`); requests naming it count
  for that client, `DELETE /mcp` with it ends it, and a request naming an unknown session gets
  404 so the client starts a new one (after Scythe restarted, say). Requests naming no session
  count for the client that initialized last. A client holding `GET /mcp` open stays listed
  while it does and 30 s after; one that never does, 30 minutes after its last request.
- `scythe-mcp` (stdio, `--url` or `--headless`) is one client: it says hello to Scythe every 5 s,
  so it is listed within seconds of Scythe starting and dropped 20 s after it quits; closing its
  stdin ends it at once. Over `--url` its requests go to `/api/command` with the user agent
  `scythe-mcp/<version>` and are not counted among the web clients of `/api/health`.

Every tool call is bracketed by `agents.call` and `agents.done`, and its commands run through
`agents.run`: the session refuses them while paused, and marks the undo steps they record with
the agent's name. The labels stay REAPER's ("Track volume"); the window, the undo toast
("Undo: Track volume (Claude Code)") and `agents.activity` show who made them. Undo history
is not saved in the project, so RPP files are unchanged.

## Safety

- Scythe's control API, and with it `/mcp`, listens on `127.0.0.1` unless told otherwise.
  Binding to another address without a token prints a warning.
- With a token (`--token`, `--control-token`, `SCYTHE_CONTROL_TOKEN`), `/mcp` requires it like
  every other endpoint, as `Authorization: Bearer <token>`; `scythe-mcp --token` sends it.
  It must be the admin token: a musician's or the stage's access link is refused (403).
- Without a token, `/mcp` refuses requests whose `Origin` is a web page on another host, so a
  site in a browser cannot reach the session through DNS rebinding.
- Paths in `project_open`, `project_save`, `media_import`, `media_probe` and the analysis tools
  are on the machine running Scythe; an agent can read any file and overwrite any `.RPP` the
  Scythe process may. These are admin-only: musicians' and stage links cannot reach them, and
  the web UI's file browser stays inside its folders (`--folder`). Every project change can be
  undone with `project_undo`; saving and plugin parameter changes cannot.
- `script_run` runs code that can change anything in the project; clients should confirm it.

## Limits

- MCP sessions over HTTP only tell clients apart for the agents window: every client of one
  address shares its tool selection, subscriptions and log level, and `GET /mcp` streams every
  notification to every listener.
- The pause judges a command by its action's words (`list`, `get`, `probe`… read), as the
  tools' read-only hints do; MCP progress notifications are not shown, `set_status` is.
- Notifications are found by polling the session's events every quarter second, not pushed.
- `script_run` needs the desktop app; the headless server answers `failed`.
- Plugin parameters are not undo steps (as in the UI).

## Extending

A new control API domain works through `scythe_command` at once. To give it tools of its own,
derive `schemars::JsonSchema` on its command enum (with doc comments on variants and fields:
they become the tool and argument descriptions) and add it to `domains()` in
`crates/scythe-mcp/src/tools.rs`; then refresh the snapshot with
`UPDATE_SNAPSHOTS=1 devbox run -- scripts/cargo.sh test -p scythe-mcp --test schema`.
Annotations follow from the action's name (`list`, `get`, `probe`, `compare` read; `delete`,
`remove`, `open`, `new`, `save`, `renew` destroy; `mute`, `solo`, `arm`, `bypass` toggle). Add a
tool to `CURATED` there to list it by default. A command answering `{"job", "status":
"running"}` is waited for through `analysis.job`.
