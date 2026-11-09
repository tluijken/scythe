# MCP server

Scythe speaks the [Model Context Protocol](https://modelcontextprotocol.io), so AI agents such
as Claude Code and Claude Desktop can inspect and change a running session: read the project,
set levels, route sends, add effects, move items, run the transport. Every tool is a
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
`--control 127.0.0.1:8770`.

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
| Effects | `fx_list`, `fx_catalog` (with `query` to search), `fx_add`, `fx_remove`, `fx_bypass`, `fx_params`, `fx_param` |
| Items | `item_list`, `item_move`, `item_split`, `item_delete`, `item_volume`, `item_mute` |
| Markers | `marker_list`, `marker_add`, `marker_edit`, `marker_delete`, `marker_goto` |
| Monitor mixes | `mix_list`, `mix_get`, `mix_create`, `mix_copy`, `mix_rename`, `mix_delete`, `mix_level`, `mix_pan`, `mix_mute`, `mix_own`, `mix_more_me`, `mix_volume`, `mix_output`, `mix_limiter`, `mix_add_source`, `mix_remove_source`, `mix_cue_bus` |
| Setlists | `setlist_songs`, `setlist_list`, `setlist_get`, `setlist_create`, `setlist_rename`, `setlist_delete`, `setlist_duplicate`, `setlist_set`, `setlist_move`, `setlist_import`, `setlist_export`, `setlist_select`, `setlist_cue`, `setlist_play`, `setlist_stop`, `setlist_next`, `setlist_previous`, `setlist_auto_advance`, `setlist_marker_actions`, `setlist_status` |
| Anything else | `scythe_command`: any command as JSON (`{"command": {"cmd": "mix.…", …}}`), so new API domains work before they get tools of their own |

Results are the command's JSON answer; answers that are a whole project (`project_open`,
`project_new`) come back as the `get_project` overview. A command that fails (no such track,
value out of range, nothing to undo) is a tool result with `isError` and a message starting
with its kind (`invalid`, `not_found`, `failed`), so the agent can correct itself.

Tool annotations tell clients what to confirm: read-only tools (`*_list`, `*_get`,
`*_status`, `transport_state`, `fx_catalog`, `fx_params`, `setlist_songs`, `setlist_export`,
`get_project`) say `readOnlyHint`; deleting and removing, `project_open` and `project_new` (which drop unsaved changes), `project_save` (which
overwrites a file) and `scythe_command` say `destructiveHint`. Toggles (mute, solo, arm,
monitor, bypass, own, limiter without a value) and steps such as `setlist_next` are not
idempotent.

## Resources and prompts

| URI | |
|---|---|
| `scythe://project/summary` | the `get_project` overview, items included |
| `scythe://project` | the full snapshot as JSON |
| `scythe://docs/control-api` | the control API reference |
| `scythe://docs/mcp` | this page |

Prompts: `mix_check` (optional `focus`) has the agent inspect levels, routing and effects and
propose changes without making them; `monitor_mix` (`musician`, `request`) has it change one
musician's in-ear mix in small steps and report what it changed; `record_setup` (optional
`tracks`) has it arm tracks and check inputs and monitoring.

## Safety

- Scythe's control API, and with it `/mcp`, listens on `127.0.0.1` unless told otherwise.
  Binding to another address without a token prints a warning.
- With a token (`--token`, `--control-token`, `SCYTHE_CONTROL_TOKEN`), `/mcp` requires it like
  every other endpoint, as `Authorization: Bearer <token>`; `scythe-mcp --token` sends it.
- Without a token, `/mcp` refuses requests whose `Origin` is a web page on another host, so a
  site in a browser cannot reach the session through DNS rebinding.
- Paths in `project_open` and `project_save` are on the machine running Scythe; an agent can
  open and overwrite any `.RPP` the Scythe process may write. Every project change can be
  undone with `project_undo`; saving and plugin parameter changes cannot.

## Limits

- 97 tools: clients that load every tool definition up front spend context on them.
- No server-to-client messages: `GET /mcp` answers 405, there are no resource subscriptions,
  and meter levels (a control API event) are not available to agents.
- Plugin parameters are not undo steps (as in the UI).

## Extending

A new control API domain works through `scythe_command` at once. To give it tools of its own,
derive `schemars::JsonSchema` on its command enum (with doc comments on variants and fields:
they become the tool and argument descriptions) and add it to `domains()` in
`crates/scythe-mcp/src/tools.rs`; then refresh the snapshot with
`UPDATE_SNAPSHOTS=1 devbox run -- scripts/cargo.sh test -p scythe-mcp --test schema`.
Annotations follow from the action's name (`list`, `get` read; `delete`, `remove`, `open`,
`new`, `save` destroy; `mute`, `solo`, `arm`, `bypass` toggle).
