//! The tools, made from the control API's command types so their names, descriptions and
//! argument schemas follow the API: every command `domain.action` is a tool `domain_action`.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use scythe_session::access::AccessCommand;
use scythe_session::api::{
    analysis::AnalysisCommand, audio::AudioCommand, bay::BayCommand, customize::CustomizeCommand,
    envelope::EnvelopeCommand, explorer::ExplorerCommand, feed::EventsCommand, fx::FxCommand, groups::GroupCommand,
    items::ItemCommand, markers::MarkerCommand, media::MediaCommand, midi::MidiCommand, midi_edit::MidiEditCommand,
    mix::MixCommand, modulation::ModulationCommand, nam::NamCommand, project::ProjectCommand,
    recording::RecordingCommand, render::RenderCommand, rig::RigCommand, script::ScriptCommand, sends::SendCommand,
    setlist::SetlistCommand, surface::SurfaceCommand, tempo::TempoCommand, theme::ThemeCommand, timing::TimingCommand,
    tracks::TrackCommand, transport::TransportCommand,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// What calling a tool does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Runs this API command with the tool's arguments.
    Command(String),
    /// Answers with the project overview.
    Project,
    /// Runs the command given as the `command` argument.
    Passthrough,
    /// Shows or changes which tools are listed.
    Tools,
}

#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    /// The API domain (`track`, `mix`…), what `--tools` selects by; empty for the tools
    /// always listed.
    pub domain: String,
    pub action: Action,
    /// The tool as `tools/list` lists it.
    pub definition: Value,
}

/// Arguments of `get_project`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectArgs {
    /// List every item (media clip) under its track, with its id, position and length.
    #[serde(default)]
    pub items: bool,
    /// `summary` (default): a few lines per track. `json`: the full snapshot, which can be large.
    #[serde(default)]
    pub format: ProjectFormat,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProjectFormat {
    #[default]
    Summary,
    Json,
}

/// Arguments of `scythe_command`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandArgs {
    /// The command: `cmd` names it (`domain.action`), the other fields are its arguments.
    pub command: Map<String, Value>,
}

/// The schema of a tool's own arguments type, whose doc comment is not for clients.
fn arguments<T: JsonSchema>() -> Value {
    let mut value = schema::<T>();
    if let Some(object) = value.as_object_mut() {
        object.remove("description");
    }
    value
}

fn schema<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::draft2020_12().with(|s| s.inline_subschemas = true).into_generator();
    let mut value = generator.into_root_schema_for::<T>().to_value();
    if let Some(object) = value.as_object_mut() {
        object.remove("$schema");
        object.remove("title");
    }
    value
}

/// Each domain's command enum. A new domain whose commands derive `JsonSchema` appears as
/// tools once listed here; until then `scythe_command` reaches it.
fn domains() -> Vec<Value> {
    vec![
        schema::<TransportCommand>(),
        schema::<ProjectCommand>(),
        schema::<TrackCommand>(),
        schema::<SendCommand>(),
        schema::<FxCommand>(),
        schema::<GroupCommand>(),
        schema::<ItemCommand>(),
        schema::<MarkerCommand>(),
        schema::<MixCommand>(),
        schema::<SetlistCommand>(),
        schema::<TempoCommand>(),
        schema::<SurfaceCommand>(),
        schema::<MidiCommand>(),
        schema::<MidiEditCommand>(),
        schema::<TimingCommand>(),
        schema::<ModulationCommand>(),
        schema::<RenderCommand>(),
        schema::<RecordingCommand>(),
        schema::<CustomizeCommand>(),
        schema::<EnvelopeCommand>(),
        schema::<MediaCommand>(),
        schema::<ExplorerCommand>(),
        schema::<BayCommand>(),
        schema::<ThemeCommand>(),
        schema::<AnalysisCommand>(),
        schema::<EventsCommand>(),
        schema::<AccessCommand>(),
        schema::<ScriptCommand>(),
        schema::<RigCommand>(),
        schema::<NamCommand>(),
        schema::<AudioCommand>(),
    ]
}

/// Every tool, in the order `tools/list` gives them.
pub fn all() -> &'static [Tool] {
    static TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        let mut tools = vec![project_tool()];
        for domain in domains() {
            let notes = domain["description"].as_str().map(str::to_owned);
            let variants = domain["oneOf"].as_array().cloned().unwrap_or_default();
            tools.extend(variants.iter().filter_map(|v| command_tool(v, notes.as_deref())));
        }
        tools.push(passthrough_tool());
        tools.push(tools_tool());
        tools
    })
}

pub fn find(name: &str) -> Option<&'static Tool> {
    all().iter().find(|t| t.name == name)
}

/// The `cmd` a variant's schema pins.
fn command_name(variant: &Value) -> Option<&str> {
    let cmd = &variant["properties"]["cmd"];
    cmd["const"].as_str().or_else(|| cmd["enum"][0].as_str())
}

fn command_tool(variant: &Value, notes: Option<&str>) -> Option<Tool> {
    let cmd = command_name(variant)?.to_owned();
    if cmd == "snapshot" {
        return None;
    }
    let mut input = variant.as_object()?.clone();
    let summary = input.remove("description").and_then(|d| d.as_str().map(str::to_owned));
    let mut properties = input.remove("properties").and_then(|p| p.as_object().cloned()).unwrap_or_default();
    properties.remove("cmd");
    let required: Vec<Value> = input
        .remove("required")
        .and_then(|r| r.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r != "cmd")
        .collect();
    input.insert("type".into(), "object".into());
    input.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        input.insert("required".into(), Value::Array(required));
    }

    let mut description = summary.unwrap_or_else(|| format!("Runs `{cmd}`."));
    if let Some(notes) = notes {
        description = format!("{description}\n\n{notes}");
    }
    description.push_str(&format!("\n\nControl API command `{cmd}`."));
    let name = cmd.replace('.', "_");
    let domain = cmd.split('.').next().unwrap_or_default().to_owned();
    let definition = json!({
        "name": name,
        "title": title(&cmd),
        "description": description,
        "inputSchema": input,
        "annotations": annotations(&cmd),
    });
    Some(Tool { name, domain, action: Action::Command(cmd), definition })
}

fn project_tool() -> Tool {
    let description = "A compact overview of the open project: file, tempo, transport, undo state, the \
        master and every track (id, name, level, pan, mute/solo/arm, effects, sends, items) and the \
        markers. Start here: tools name tracks by the ids and names it shows. Times are in seconds.";
    Tool {
        name: "get_project".into(),
        domain: String::new(),
        action: Action::Project,
        definition: json!({
            "name": "get_project",
            "title": "Project overview",
            "description": description,
            "inputSchema": arguments::<ProjectArgs>(),
            "annotations": {"readOnlyHint": true, "openWorldHint": false},
        }),
    }
}

fn passthrough_tool() -> Tool {
    let description = "Runs any control API command and answers with its JSON result, for commands no \
        other tool covers (new domains show up here first). Example: {\"command\": {\"cmd\": \
        \"track.volume\", \"track\": \"Vox\", \"db\": -6}}. The resource scythe://docs/control-api \
        lists every command.";
    Tool {
        name: "scythe_command".into(),
        domain: String::new(),
        action: Action::Passthrough,
        definition: json!({
            "name": "scythe_command",
            "title": "Run a control API command",
            "description": description,
            "inputSchema": arguments::<CommandArgs>(),
            "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false},
        }),
    }
}

/// Arguments of `scythe_tools`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolsArgs {
    /// Domains to list the tools of from now on, besides those listed (`mix`, `setlist`,
    /// `fx`…; `all` for every tool).
    #[serde(default)]
    pub enable: Vec<String>,
    /// Domains to stop listing (`default` for the curated set).
    #[serde(default)]
    pub disable: Vec<String>,
}

fn tools_tool() -> Tool {
    let description = "Shows which tool domains are listed and which exist, or lists more (`enable`) or \
        fewer (`disable`). The default is a compact set for building and mixing a session; every \
        domain's tools can be added here, and any command runs through scythe_command meanwhile. \
        Clients are told of the change (tools/list_changed).";
    Tool {
        name: "scythe_tools".into(),
        domain: String::new(),
        action: Action::Tools,
        definition: json!({
            "name": "scythe_tools",
            "title": "Choose the tools listed",
            "description": description,
            "inputSchema": arguments::<ToolsArgs>(),
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        }),
    }
}

/// The compact set listed unless `--tools` says otherwise: enough to build a live session,
/// mix it and match it to a reference, with scythe_command for the rest.
pub const CURATED: &[&str] = &[
    "transport_play",
    "transport_stop",
    "transport_seek",
    "transport_state",
    "transport_metronome",
    "project_save",
    "project_undo",
    "project_redo",
    "track_add",
    "track_rename",
    "track_volume",
    "track_pan",
    "track_mute",
    "track_input",
    "track_folder",
    "send_add",
    "send_hw_add",
    "fx_catalog",
    "fx_add",
    "fx_params",
    "fx_param",
    "marker_list",
    "marker_add",
    "mix_create",
    "mix_level",
    "mix_output",
    "setlist_songs",
    "setlist_create",
    "media_import",
    "media_probe",
    "analysis_file",
    "analysis_mix",
    "analysis_compare",
    "analysis_match_eq",
    "analysis_segments",
    "events_since",
    "events_meters",
];

/// Which tools `tools/list` gives: the curated set and/or whole domains, or all of them.
/// `get_project`, `scythe_command` and `scythe_tools` are always listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub all: bool,
    pub curated: bool,
    pub domains: BTreeSet<String>,
}

impl Default for Selection {
    fn default() -> Self {
        Self { all: false, curated: true, domains: BTreeSet::new() }
    }
}

/// Every domain that has tools, in the order they are listed.
pub fn domain_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for t in all() {
        if !t.domain.is_empty() && !names.contains(&t.domain.as_str()) {
            names.push(t.domain.as_str());
        }
    }
    names
}

impl Selection {
    pub fn everything() -> Self {
        Self { all: true, curated: true, domains: BTreeSet::new() }
    }

    /// `--tools` and `?tools=`: a comma-separated list of domains, `default` for the curated
    /// set, `all` for every tool.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut s = Self { all: false, curated: false, domains: BTreeSet::new() };
        s.enable(spec.split(',').map(str::trim).filter(|d| !d.is_empty()))?;
        if !s.all && !s.curated && s.domains.is_empty() {
            s.curated = true;
        }
        Ok(s)
    }

    pub fn enable<'a>(&mut self, names: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
        let known = domain_names();
        for name in names {
            match name {
                "all" => self.all = true,
                "default" => self.curated = true,
                d if known.contains(&d) => {
                    self.domains.insert(d.to_owned());
                }
                d => return Err(format!("no tool domain \"{d}\"; there are default, all, {}", known.join(", "))),
            }
        }
        Ok(())
    }

    pub fn disable<'a>(&mut self, names: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
        let known = domain_names();
        for name in names {
            match name {
                "all" => {
                    self.all = false;
                    self.curated = false;
                    self.domains.clear();
                }
                "default" => self.curated = false,
                d if known.contains(&d) => {
                    if self.all {
                        self.all = false;
                        self.curated = false;
                        self.domains = known.iter().map(|k| (*k).to_owned()).collect();
                    }
                    self.domains.remove(d);
                }
                d => return Err(format!("no tool domain \"{d}\"")),
            }
        }
        Ok(())
    }

    pub fn includes(&self, tool: &Tool) -> bool {
        self.all
            || tool.domain.is_empty()
            || self.domains.contains(&tool.domain)
            || (self.curated && CURATED.contains(&tool.name.as_str()))
    }

    /// The tools listed, in `tools/list` order.
    pub fn tools(&self) -> Vec<&'static Tool> {
        all().iter().filter(|t| self.includes(t)).collect()
    }

    /// How it reads in `--tools` form.
    pub fn describe(&self) -> String {
        if self.all {
            return "all".into();
        }
        let mut parts: Vec<String> = Vec::new();
        if self.curated {
            parts.push("default".into());
        }
        parts.extend(self.domains.iter().cloned());
        if parts.is_empty() { "none".into() } else { parts.join(",") }
    }
}

fn title(cmd: &str) -> String {
    let (domain, action) = cmd.split_once('.').unwrap_or((cmd, ""));
    let domain = match domain {
        "fx" => "FX".to_owned(),
        d => d[..1].to_uppercase() + &d[1..],
    };
    format!("{domain} {}", action.replace('_', " ")).trim().to_owned()
}

/// Hints for clients deciding what to confirm with the user, judged by the action's name so
/// that new domains get sensible ones too.
fn annotations(cmd: &str) -> Value {
    const READS: [&str; 24] = [
        "list", "get", "state", "status", "catalog", "params", "snapshot", "search", "info", "songs", "export", "show",
        "peaks", "health", "devices", "evaluate", "since", "meters", "probe", "job", "compare", "file", "mix",
        "whoami",
    ];
    // Opening or starting a project drops unsaved changes; saving overwrites a file; renewing
    // a link locks out the phone holding it.
    const DESTROYS: [&str; 7] = ["delete", "remove", "clear", "open", "new", "save", "renew"];
    // Toggles, and actions that add or step on each time.
    const REPEATABLE: [&str; 22] = [
        "mute",
        "solo",
        "arm",
        "monitor",
        "bypass",
        "toggle",
        "own",
        "limiter",
        "advance",
        "actions",
        "add",
        "split",
        "undo",
        "redo",
        "duplicate",
        "insert",
        "create",
        "copy",
        "import",
        "bus",
        "next",
        "previous",
    ];
    if cmd == "script.run" {
        return json!({"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false});
    }
    let action = cmd.rsplit('.').next().unwrap_or(cmd);
    let has = |list: &[&str]| action.split('_').any(|word| list.contains(&word));
    if has(&READS) {
        return json!({"readOnlyHint": true, "openWorldHint": false});
    }
    json!({"readOnlyHint": false, "destructiveHint": has(&DESTROYS), "idempotentHint": !has(&REPEATABLE), "openWorldHint": false})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_api_command_but_the_snapshot_is_a_tool() {
        let names: Vec<&str> = all().iter().map(|t| t.name.as_str()).collect();
        for name in ["get_project", "transport_play", "track_volume", "send_add", "fx_catalog", "item_split"] {
            assert!(names.contains(&name), "{name} missing from {names:?}");
        }
        assert!(names.contains(&"marker_goto") && names.contains(&"scythe_command"));
        assert!(!names.contains(&"snapshot"));
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len());
    }

    #[test]
    fn schemas_come_from_the_command_types() {
        let volume = &find("track_volume").unwrap().definition;
        let input = &volume["inputSchema"];
        assert_eq!(input["required"], json!(["track"]));
        assert!(input["properties"]["cmd"].is_null());
        assert!(input["properties"]["db"]["description"].as_str().unwrap().contains("dB"));
        assert!(input["properties"]["track"]["description"].as_str().unwrap().contains("master"));
        assert!(volume["description"].as_str().unwrap().contains("`track.volume`"));
        let marker = &find("marker_delete").unwrap().definition;
        assert!(marker["description"].as_str().unwrap().contains("REAPER numbers them separately"));
    }

    #[test]
    fn destructive_tools_say_so() {
        for name in ["track_delete", "project_open", "project_new", "project_save", "fx_remove", "item_delete"] {
            assert_eq!(find(name).unwrap().definition["annotations"]["destructiveHint"], true, "{name}");
        }
        for name in ["track_list", "fx_catalog", "transport_state", "get_project"] {
            assert_eq!(find(name).unwrap().definition["annotations"]["readOnlyHint"], true, "{name}");
        }
        for name in ["mix_delete", "mix_remove_source", "send_hw_remove", "setlist_delete"] {
            assert_eq!(find(name).unwrap().definition["annotations"]["destructiveHint"], true, "{name}");
        }
        for name in ["mix_list", "send_hw_list", "setlist_status", "setlist_songs", "setlist_export"] {
            assert_eq!(find(name).unwrap().definition["annotations"]["readOnlyHint"], true, "{name}");
        }
        for name in ["mix_more_me", "mix_level", "setlist_cue"] {
            assert_eq!(find(name).unwrap().definition["annotations"]["idempotentHint"], true, "{name}");
        }
        assert_eq!(find("setlist_next").unwrap().definition["annotations"]["idempotentHint"], false);
        assert_eq!(find("track_volume").unwrap().definition["annotations"]["destructiveHint"], false);
        assert_eq!(find("track_mute").unwrap().definition["annotations"]["idempotentHint"], false);
    }

    #[test]
    fn selections_pick_domains() {
        let default = Selection::default().tools();
        assert!(default.len() < all().len() / 3);
        assert!(CURATED.iter().all(|name| find(name).is_some()), "every curated tool exists");
        let mixes = Selection::parse("mix").unwrap();
        assert!(mixes.tools().iter().all(|t| t.domain.is_empty() || t.domain == "mix"));
        assert_eq!(Selection::parse("all").unwrap().tools().len(), all().len());
        assert_eq!(Selection::parse("default,setlist").unwrap().describe(), "default,setlist");
        assert!(Selection::parse("nope").is_err());
        let mut s = Selection::everything();
        s.disable(["mix"]).unwrap();
        assert!(!s.tools().iter().any(|t| t.domain == "mix") && s.tools().iter().any(|t| t.domain == "track"));
        for name in ["script_run", "access_renew"] {
            assert_eq!(find(name).unwrap().definition["annotations"]["destructiveHint"], true, "{name}");
        }
        for name in ["analysis_compare", "media_probe", "events_since", "analysis_file"] {
            assert_eq!(find(name).unwrap().definition["annotations"]["readOnlyHint"], true, "{name}");
        }
    }

    #[test]
    fn tool_names_suit_every_client() {
        for tool in all() {
            let ok = tool.name.len() <= 64 && tool.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            assert!(ok, "{}", tool.name);
        }
    }
}
