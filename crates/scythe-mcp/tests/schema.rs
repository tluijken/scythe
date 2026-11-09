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
