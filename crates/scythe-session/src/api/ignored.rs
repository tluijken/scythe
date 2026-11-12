//! Which fields of a command its type never reads. Commands keep accepting fields they do not
//! know (clients send extra keys, and structs that `flatten` cannot deny them), so instead the
//! answer lists them, and a toggle refuses them when its value is missing.

use serde_json::{Map, Value};

use super::{ApiError, Command};

/// Commands that toggle (or cycle) when all of their value fields are left out, with those
/// fields.
pub const TOGGLES: &[(&str, &[&str])] = &[
    ("track.mute", &["muted"]),
    ("track.solo", &["solo"]),
    ("track.solo_defeat", &["on"]),
    ("track.phase", &["phase"]),
    ("track.arm", &["armed"]),
    ("track.monitor", &["monitor"]),
    ("track.main_send", &["on"]),
    ("track.collapse", &["compact", "mixer"]),
    ("fx.bypass", &["bypassed"]),
    ("fx.offline", &["offline"]),
    ("item.mute", &["muted"]),
    ("item.lock", &["locked"]),
    ("send.mute", &["muted"]),
    ("mix.mute", &["muted"]),
    ("mix.own", &["own"]),
    ("mix.limiter", &["on"]),
    ("setlist.auto_advance", &["enabled"]),
    ("setlist.count_in", &["enabled"]),
    ("setlist.marker_actions", &["enabled"]),
];

/// No field type reads this string as it read whatever the field held before.
const PROBE: &str = "\u{1}scythe: is this field read?";

/// The top-level fields of `input` that parsing it into `command` ignored, in key order.
///
/// A field the command's type reads shows up when the command is serialised again, under its
/// own name even when an alias was given. Any other key is put to a test: if the command parses
/// to the same thing with the key's value replaced by [`PROBE`], nothing read it. That keeps
/// aliases and fields that are not serialised back from being reported, and needs no list of
/// fields per command. Keys inside nested objects are not checked.
pub(super) fn ignored(input: &Value, command: &Command) -> Result<Vec<String>, ApiError> {
    let Some(fields) = input.as_object() else { return Ok(Vec::new()) };
    let read = serde_json::to_value(command).ok();
    let read = read.as_ref().and_then(Value::as_object);
    let mut ignored = Vec::new();
    for key in fields.keys() {
        if read.is_some_and(|r| r.contains_key(key)) {
            continue;
        }
        let mut probe = fields.clone();
        probe.insert(key.clone(), PROBE.into());
        if serde_json::from_value::<Command>(Value::Object(probe)).is_ok_and(|p| &p == command) {
            ignored.push(key.clone());
        }
    }
    if let Some(run) = agents_run(command) {
        ignored.extend(run?);
    }
    if let Some(first) = ignored.first() {
        refuse_toggle(fields, read, first)?;
    }
    Ok(ignored)
}

/// The ignored fields of the command an agent's `agents.run` runs.
fn agents_run(command: &Command) -> Option<Result<Vec<String>, ApiError>> {
    let Command::Agents(super::agents::AgentsCommand::Run { command: inner, .. }) = command else { return None };
    let parsed = serde_json::from_value::<Command>(inner.clone()).ok()?;
    Some(ignored(inner, &parsed))
}

/// A toggle given an unknown field and not its value would do the opposite of what was meant
/// half the time (`"invert": false` for `phase`), so that is refused.
fn refuse_toggle(
    fields: &Map<String, Value>,
    read: Option<&Map<String, Value>>,
    unknown: &str,
) -> Result<(), ApiError> {
    let Some(cmd) = read.and_then(|r| r.get("cmd")).or_else(|| fields.get("cmd")).and_then(Value::as_str) else {
        return Ok(());
    };
    let Some((_, values)) = TOGGLES.iter().find(|(name, _)| *name == cmd) else { return Ok(()) };
    let given = |field: &&str| match read {
        Some(read) => read.get(*field).is_some_and(|v| !v.is_null()),
        None => fields.get(*field).is_some_and(|v| !v.is_null()),
    };
    if values.iter().any(given) {
        return Ok(());
    }
    let expected = values.iter().map(|v| format!("\"{v}\"")).collect::<Vec<_>>().join(" or ");
    Err(ApiError::invalid(format!(
        "{cmd} has no field \"{unknown}\" and toggles without {expected}: give {expected}, or leave \"{unknown}\" out to toggle"
    )))
}
