use std::path::Path;

use serde_json::Value;

/// The tool list clients see. A change to a command type changes it; when that is intended,
/// run with `UPDATE_SNAPSHOTS=1` and commit the new `tests/tools.json`.
#[test]
fn tool_definitions_match_the_snapshot() {
    let tools: Vec<&Value> = scythe_mcp::tools::all().iter().map(|t| &t.definition).collect();
    let current = serde_json::to_string_pretty(&tools).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tools.json");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &current).unwrap();
        return;
    }
    let saved = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        saved == current,
        "the tool definitions changed; run `UPDATE_SNAPSHOTS=1 cargo test -p scythe-mcp --test schema` if that is intended"
    );
}

/// Every command documented as toggling when its value is left out is in `api::TOGGLES`, which
/// refuses it an unknown field in place of that value, and every entry names fields the command
/// has.
#[test]
fn every_toggle_is_known_to_the_api() {
    let toggles = scythe_session::api::TOGGLES;
    for tool in scythe_mcp::tools::all() {
        let definition = &tool.definition;
        let description = definition["description"].as_str().unwrap_or_default().to_lowercase();
        let Some((_, cmd)) = description.split_once("control api command `") else { continue };
        let cmd = cmd.split('`').next().unwrap();
        let known = toggles.iter().find(|(name, _)| *name == cmd);
        if description.contains("toggles") && description.contains("left out") {
            assert!(known.is_some(), "{cmd} toggles but is not in api::TOGGLES");
        }
        if let Some((_, fields)) = known {
            for field in *fields {
                assert!(definition["inputSchema"]["properties"].get(field).is_some(), "{cmd} has no field {field}");
            }
        }
    }
    for (cmd, _) in toggles {
        let parsed = scythe_session::api::parse(serde_json::json!({"cmd": cmd}));
        assert!(parsed.as_ref().err().is_none_or(|e| !e.message.contains("unknown variant")), "{cmd} is no command");
    }
}
