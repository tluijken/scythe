use std::path::{Path, PathBuf};

use super::{Chunk, Node, ParseError, line_key, parse, quote_always, tokenize, write};
use crate::envelope::{EnvPoint, Envelope, EnvelopeKind, PointShape};
use crate::fade::Fade;
use crate::model::{
    Color, FxSlot, Item, LineMemo, Marker, Project, Section, Send, Source, Take, TimeRange, Track, TrackId, new_guid,
};
use crate::tempo::TempoPoint;
use crate::units::PanTaper;

const PROJECT_VERSION: &str = "0.1";
/// Set on stored colors that are user-chosen; without it REAPER uses the theme default.
const CUSTOM_COLOR_FLAG: i64 = 0x0100_0000;

#[derive(Debug, thiserror::Error)]
pub enum ProjectIoError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Parse(#[from] ParseError),
}

pub fn read_project(path: &Path) -> Result<Project, ProjectIoError> {
    let text = std::fs::read_to_string(path)?;
    let mut project = project_from_rpp(&text)?;
    project.path = Some(path.to_owned());
    Ok(project)
}

/// Like `write_project`, but first keeps the previous file as `<name>.RPP-bak`, as REAPER does.
pub fn save_project(project: &Project, path: &Path) -> Result<(), ProjectIoError> {
    if path.exists() {
        let mut bak = path.as_os_str().to_owned();
        bak.push("-bak");
        std::fs::copy(path, PathBuf::from(bak))?;
    }
    write_project(project, path)
}

/// Path for a timestamped auto-save: `<project dir>/Backups/<name>-YYYY-MM-DD_HHMMSS.rpp-bak`
/// (UTC).
pub fn autosave_path(path: &Path, unix_seconds: u64) -> PathBuf {
    let days = (unix_seconds / 86_400) as i64;
    let secs = unix_seconds % 86_400;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    let name =
        format!("{stem}-{year:04}-{month:02}-{day:02}_{:02}{:02}{:02}.rpp-bak", secs / 3600, secs / 60 % 60, secs % 60);
    path.parent().unwrap_or(Path::new(".")).join("Backups").join(name)
}

pub fn write_project(project: &Project, path: &Path) -> Result<(), ProjectIoError> {
    let text = project_to_rpp(project);
    let tmp = path.with_extension("rpp.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn num(toks: &[String], i: usize, default: f64) -> f64 {
    toks.get(i).and_then(|t| t.parse().ok()).unwrap_or(default)
}

fn flag(toks: &[String], i: usize) -> bool {
    num(toks, i, 0.0) != 0.0
}

/// Shortest representation that round-trips, which is also what REAPER writes.
pub fn fmt_num(v: f64) -> String {
    if v == 0.0 { "0".to_owned() } else { v.to_string() }
}

fn remember(lines: &mut LineMemo, t: &[String]) {
    lines.insert(t[0].clone(), t.to_vec());
}

/// The remembered tokens for `key` (or `default`) with the modelled fields overwritten.
fn line_from(lines: &LineMemo, key: &str, default: &[&str], set: &[(usize, String)]) -> Vec<String> {
    let mut toks: Vec<String> = match lines.get(key) {
        Some(t) => t.clone(),
        None => default.iter().map(|s| (*s).to_owned()).collect(),
    };
    for (i, v) in set {
        if toks.len() <= *i {
            toks.resize(i + 1, "0".to_owned());
        }
        toks[*i] = v.clone();
    }
    toks
}

fn bit(b: bool) -> String {
    if b { "1" } else { "0" }.to_owned()
}

fn color_from_native(v: i64) -> Option<Color> {
    (v & CUSTOM_COLOR_FLAG != 0).then(|| Color::from_hex((v & 0xff_ffff) as u32))
}

fn color_to_native(c: Option<Color>) -> i64 {
    c.map_or(0, |c| c.to_hex() as i64 | CUSTOM_COLOR_FLAG)
}

pub fn project_from_rpp(text: &str) -> Result<Project, ParseError> {
    let root = parse(text)?;
    if root.name() != "REAPER_PROJECT" {
        return Err(ParseError::WrongRoot { expected: "REAPER_PROJECT", found: root.name().to_owned() });
    }

    let mut p = Project::default();
    let mut selection = None;
    // AUXRECV lives on the receiving track and names its source by index.
    let mut receives: Vec<(usize, usize, Send)> = Vec::new();
    let mut region_starts: Vec<Marker> = Vec::new();

    for node in &root.children {
        match node {
            Node::Line(line) => {
                let t = tokenize(line);
                if matches!(
                    t[0].as_str(),
                    "TEMPO" | "LOOP" | "SELECTION" | "CURSOR" | "SAMPLERATE" | "MASTER_VOLUME" | "MASTERMUTESOLO"
                ) {
                    remember(&mut p.lines, &t);
                }
                match t[0].as_str() {
                    "TEMPO" => {
                        p.tempo.bpm = num(&t, 1, 120.0);
                        p.tempo.beats_per_bar = num(&t, 2, 4.0) as u32;
                        p.tempo.beat_unit = num(&t, 3, 4.0) as u32;
                    }
                    "LOOP" => p.loop_enabled = flag(&t, 1),
                    "SELECTION" => {
                        let (start, end) = (num(&t, 1, 0.0), num(&t, 2, 0.0));
                        selection = (start < end).then_some(TimeRange { start, end });
                    }
                    "CURSOR" => p.cursor = num(&t, 1, 0.0),
                    "SAMPLERATE" => p.sample_rate = Some(num(&t, 1, 44100.0) as u32),
                    "MASTER_VOLUME" => {
                        p.master.volume = num(&t, 1, 1.0);
                        p.master.pan = num(&t, 2, 0.0);
                    }
                    "MASTERMUTESOLO" => {
                        let v = num(&t, 1, 0.0) as i64;
                        p.master.muted = v & 1 != 0;
                    }
                    "MARKER" => parse_marker(&t, &mut p.markers, &mut region_starts),
                    "PANLAW" => {
                        p.pan_law.law = num(&t, 1, 1.0);
                        p.extra.push(node.clone());
                    }
                    "PANLAWFLAGS" => {
                        p.pan_law.taper = PanTaper::from_flags(num(&t, 1, 3.0) as i64);
                        p.extra.push(node.clone());
                    }
                    _ => p.extra.push(node.clone()),
                }
            }
            Node::Chunk(c) if c.name() == "TRACK" => {
                let idx = p.tracks.len();
                let (track, recvs) = parse_track(c);
                receives.extend(recvs.into_iter().map(|(src, send)| (src, idx, send)));
                p.tracks.push(track);
            }
            Node::Chunk(c) if c.name() == "TEMPOENVEX" => {
                p.tempo.points = parse_tempo_points(c);
                p.extra.push(node.clone());
            }
            Node::Chunk(c) if c.name() == "MASTERFXLIST" => {
                let (fx, prelude) = parse_fx_chain(c);
                p.master.fx = fx;
                p.master.fx_chain_prelude = prelude;
            }
            Node::Chunk(_) => p.extra.push(node.clone()),
        }
    }

    for (src, dest, mut send) in receives {
        if let (Some(dest_id), Some(src_track)) = (p.tracks.get(dest).map(|t| t.id), p.tracks.get(src)) {
            send.dest = dest_id;
            let src_id = src_track.id;
            p.track_mut(src_id).unwrap().sends.push(send);
        }
    }
    p.markers.extend(region_starts);
    p.markers.sort_by(|a, b| a.position.total_cmp(&b.position).then(a.index.cmp(&b.index)));
    p.time_selection = selection;
    Ok(p)
}

/// `PT <time> <bpm> <shape> [<time signature: num | denom << 16>] …`; shape 0 ramps linearly.
fn parse_tempo_points(c: &Chunk) -> Vec<TempoPoint> {
    c.children
        .iter()
        .filter_map(|n| match n {
            Node::Line(l) if line_key(l) == "PT" => Some(tokenize(l)),
            _ => None,
        })
        .map(|t| {
            let sig = num(&t, 4, 0.0) as i64;
            TempoPoint {
                time: num(&t, 1, 0.0),
                bpm: num(&t, 2, 120.0),
                linear: num(&t, 3, 1.0) == 0.0,
                time_sig: (sig > 0).then_some(((sig & 0xffff) as u32, ((sig >> 16) & 0xffff) as u32)),
                raw: t,
            }
        })
        .collect()
}

fn tempo_point_line(pt: &TempoPoint) -> Vec<String> {
    let mut set = vec![(1, fmt_num(pt.time)), (2, fmt_num(pt.bpm)), (3, if pt.linear { "0" } else { "1" }.to_owned())];
    let old_sig = num(&pt.raw, 4, 0.0) as i64;
    let sig = pt.time_sig.map_or(0, |(n, d)| n as i64 | (d as i64) << 16);
    if sig != (old_sig & 0xffff_ffff) {
        set.push((4, sig.to_string()));
    }
    line_from(&LineMemo::from([("PT".to_owned(), pt.raw.clone())]), "PT", &["PT"], &set)
}

fn write_tempo_env(original: Option<&Chunk>, points: &[TempoPoint]) -> Chunk {
    let mut c = original.cloned().unwrap_or_else(|| {
        let mut c = Chunk::new("TEMPOENVEX", &[]);
        c.line(["EGUID", &new_guid()]);
        c.line(["ACT", "1", "-1"]);
        c.line(["VIS", "1", "0", "1"]);
        c.line(["LANEHEIGHT", "0", "0"]);
        c.line(["ARM", "0"]);
        c.line(["DEFSHAPE", "1", "-1", "-1"]);
        c
    });
    let at =
        c.children.iter().position(|n| matches!(n, Node::Line(l) if line_key(l) == "PT")).unwrap_or(c.children.len());
    c.children.retain(|n| !matches!(n, Node::Line(l) if line_key(l) == "PT"));
    let lines = points.iter().map(|pt| Node::Line(super::join_tokens(tempo_point_line(pt))));
    c.children.splice(at..at, lines);
    c
}

/// Regions are written as two MARKER lines with the same index; the second carries the end.
fn parse_marker(t: &[String], markers: &mut Vec<Marker>, open_regions: &mut Vec<Marker>) {
    let index = num(t, 1, 0.0) as u32;
    let position = num(t, 2, 0.0);
    let name = t.get(3).cloned().unwrap_or_default();
    let flags = num(t, 4, 0.0) as i64;
    let color = color_from_native(num(t, 5, 0.0) as i64);
    let mut lines = LineMemo::new();
    lines.insert("MARKER".into(), t.to_vec());
    if flags & 1 == 0 {
        markers.push(Marker { index, position, name, color, region_end: None, lines });
    } else if let Some(pos) = open_regions.iter().position(|m| m.index == index && m.region_end.is_none()) {
        let mut region = open_regions.remove(pos);
        region.region_end = Some(position);
        region.lines.insert("MARKER_END".into(), t.to_vec());
        markers.push(region);
    } else {
        open_regions.push(Marker { index, position, name, color, region_end: None, lines });
    }
}

fn parse_track(c: &Chunk) -> (Track, Vec<(usize, Send)>) {
    let mut track = Track::new("");
    if let Some(guid) = c.args().into_iter().next() {
        track.guid = guid;
    }
    let mut receives = Vec::new();
    for node in &c.children {
        match node {
            Node::Line(line) => {
                let t = tokenize(line);
                if matches!(t[0].as_str(), "NAME" | "PEAKCOL" | "VOLPAN" | "ISBUS" | "MUTESOLO") {
                    remember(&mut track.lines, &t);
                }
                match t[0].as_str() {
                    "NAME" => track.name = t.get(1).cloned().unwrap_or_default(),
                    "PEAKCOL" => track.color = color_from_native(num(&t, 1, 0.0) as i64),
                    "VOLPAN" => {
                        track.volume = num(&t, 1, 1.0);
                        track.pan = num(&t, 2, 0.0);
                        track.pan_law = num(&t, 3, -1.0);
                    }
                    "ISBUS" => track.folder_depth = num(&t, 2, 0.0) as i32,
                    "MAINSEND" => {
                        track.main_send = flag(&t, 1);
                        track.extra.push(node.clone());
                    }
                    "MUTESOLO" => {
                        track.muted = flag(&t, 1);
                        track.solo = flag(&t, 2);
                    }
                    "REC" => {
                        track.armed = flag(&t, 1);
                        track.rec_input = num(&t, 2, 0.0) as i64;
                        track.monitor = flag(&t, 3);
                        track.extra.push(node.clone());
                    }
                    "AUXRECV" => receives.push((
                        num(&t, 1, 0.0) as usize,
                        Send {
                            dest: TrackId(0),
                            volume: num(&t, 3, 1.0),
                            pan: num(&t, 4, 0.0),
                            muted: flag(&t, 5),
                            raw: Some(t.clone()),
                        },
                    )),
                    _ => track.extra.push(node.clone()),
                }
            }
            Node::Chunk(ch) if ch.name() == "ITEM" => track.items.push(parse_item(ch)),
            Node::Chunk(ch) if let Some(kind) = EnvelopeKind::from_chunk_name(ch.name()) => {
                track.envelopes.push(parse_envelope(kind, ch));
                track.extra.push(node.clone());
            }
            Node::Chunk(ch) if ch.name() == "FXCHAIN" => {
                let (fx, prelude) = parse_fx_chain(ch);
                track.fx = fx;
                track.fx_chain_prelude = prelude;
            }
            Node::Chunk(_) => track.extra.push(node.clone()),
        }
    }
    (track, receives)
}

/// `PT <time> <value> <shape> <?> <selected> <?> <bezier tension>`.
fn parse_envelope(kind: EnvelopeKind, c: &Chunk) -> Envelope {
    let mut env = Envelope::new(kind);
    for node in &c.children {
        let Node::Line(l) = node else { continue };
        let t = tokenize(l);
        match t[0].as_str() {
            "ACT" => env.active = flag(&t, 1),
            "VIS" => env.visible = flag(&t, 1),
            "PT" => env.points.push(EnvPoint {
                time: num(&t, 1, 0.0),
                value: num(&t, 2, 0.0),
                shape: PointShape::from_rpp(num(&t, 3, 0.0) as i64, num(&t, 7, 0.0)),
                raw: t,
            }),
            _ => {}
        }
    }
    env.sort();
    env.raw = Some(c.clone());
    env
}

fn write_envelope(env: &Envelope) -> Chunk {
    let mut c = env.raw.clone().unwrap_or_else(|| {
        let mut c = Chunk::new(env.kind.chunk_name(), &[]);
        c.line(["EGUID", &new_guid()]);
        c.line(["ACT", "1", "-1"]);
        c.line(["VIS", "1", "1", "1"]);
        c.line(["LANEHEIGHT", "0", "0"]);
        c.line(["ARM", "0"]);
        c.line(["DEFSHAPE", "0", "-1", "-1"]);
        if matches!(env.kind, EnvelopeKind::Volume | EnvelopeKind::VolumePreFx | EnvelopeKind::Trim) {
            c.line(["VOLTYPE", "1"]);
        }
        c
    });
    let at =
        c.children.iter().position(|n| matches!(n, Node::Line(l) if line_key(l) == "PT")).unwrap_or(c.children.len());
    c.children.retain(|n| !matches!(n, Node::Line(l) if line_key(l) == "PT"));
    for node in &mut c.children {
        if let Node::Line(l) = node {
            let key = line_key(l);
            if key == "ACT" || key == "VIS" {
                let mut t = tokenize(l);
                t.resize(t.len().max(2), "0".into());
                t[1] = bit(if key == "ACT" { env.active } else { env.visible });
                *l = super::join_tokens(t);
            }
        }
    }
    let points = env.points.iter().map(|p| {
        let mut set = vec![(1, fmt_num(p.time)), (2, fmt_num(p.value)), (3, p.shape.rpp_code().to_string())];
        if let PointShape::Bezier(t) = p.shape {
            set.push((7, fmt_num(t)));
        }
        Node::Line(super::join_tokens(line_from(
            &LineMemo::from([("PT".to_owned(), p.raw.clone())]),
            "PT",
            &["PT"],
            &set,
        )))
    });
    let at = at.min(c.children.len());
    c.children.splice(at..at, points);
    c
}

/// Each plugin in an FX chain starts with a `BYPASS` line, followed by the plugin chunk and
/// per-plugin lines (FXID, automation, …) up to the next `BYPASS`.
fn parse_fx_chain(c: &Chunk) -> (Vec<FxSlot>, Vec<Node>) {
    let mut prelude = Vec::new();
    let mut fx: Vec<FxSlot> = Vec::new();
    for node in &c.children {
        if let Node::Line(l) = node
            && line_key(l) == "BYPASS"
        {
            let t = tokenize(l);
            fx.push(FxSlot { name: String::new(), enabled: !flag(&t, 1), raw: vec![node.clone()] });
            continue;
        }
        match fx.last_mut() {
            Some(slot) => {
                if let Node::Chunk(plugin) = node
                    && slot.name.is_empty()
                {
                    slot.name = plugin.args().into_iter().next().unwrap_or_else(|| plugin.name().to_owned());
                }
                slot.raw.push(node.clone());
            }
            None => prelude.push(node.clone()),
        }
    }
    (fx, prelude)
}

const TAKE_KEYS: &[&str] = &["NAME", "TAKEVOLPAN", "SOFFS", "PLAYRATE", "CHANMODE", "GUID", "TAKECOLOR"];

fn parse_item(c: &Chunk) -> Item {
    let mut item = Item::new(0.0, 0.0, empty_take());
    item.takes.clear();
    let mut take: Option<Take> = None;
    let mut active = 0;

    for node in &c.children {
        match node {
            Node::Line(line) => {
                let t = tokenize(line);
                let key = t[0].as_str();
                if key == "TAKE" {
                    item.takes.extend(take.take());
                    if t.iter().any(|x| x == "SEL") {
                        active = item.takes.len();
                    }
                    let mut next = empty_take();
                    remember(&mut next.lines, &t);
                    take = Some(next);
                    continue;
                }
                if TAKE_KEYS.contains(&key) {
                    let tk = take.get_or_insert_with(empty_take);
                    if matches!(key, "NAME" | "TAKEVOLPAN" | "SOFFS" | "PLAYRATE") {
                        remember(&mut tk.lines, &t);
                    }
                    match key {
                        "NAME" => tk.name = t.get(1).cloned().unwrap_or_default(),
                        "TAKEVOLPAN" => {
                            tk.pan = num(&t, 1, 0.0);
                            tk.volume = num(&t, 2, 1.0);
                        }
                        "SOFFS" => tk.source_offset = num(&t, 1, 0.0),
                        "PLAYRATE" => {
                            tk.playrate = num(&t, 1, 1.0);
                            tk.preserve_pitch = num(&t, 2, 1.0) != 0.0;
                            tk.pitch = num(&t, 3, 0.0);
                        }
                        _ => tk.extra.push(node.clone()),
                    }
                    continue;
                }
                if matches!(key, "POSITION" | "LENGTH" | "LOOP" | "FADEIN" | "FADEOUT" | "MUTE" | "VOLPAN" | "YPOS") {
                    remember(&mut item.lines, &t);
                }
                match key {
                    "POSITION" => item.position = num(&t, 1, 0.0),
                    "LENGTH" => item.length = num(&t, 1, 0.0),
                    "LOOP" => item.looped = flag(&t, 1),
                    "FADEIN" => item.fade_in = parse_fade(&t),
                    "FADEOUT" => item.fade_out = parse_fade(&t),
                    "MUTE" => item.muted = flag(&t, 1),
                    "VOLPAN" => item.volume = num(&t, 1, 1.0),
                    "YPOS" => item.ypos = Some((num(&t, 1, 0.0), num(&t, 2, 1.0))),
                    "IGUID" => item.guid = t.get(1).cloned().unwrap_or_default(),
                    _ => match take.as_mut() {
                        Some(tk) => tk.extra.push(node.clone()),
                        None => item.extra.push(node.clone()),
                    },
                }
            }
            Node::Chunk(ch) if ch.name() == "SOURCE" => {
                let tk = take.get_or_insert_with(empty_take);
                tk.source = parse_source(ch);
                if matches!(tk.source, Source::File(_)) {
                    tk.lines.insert("SOURCE".into(), tokenize(&ch.header));
                    if let Some(Node::Line(file)) = ch.children.first() {
                        tk.lines.insert("FILE".into(), tokenize(file));
                    }
                }
            }
            Node::Chunk(_) => match take.as_mut() {
                Some(tk) => tk.extra.push(node.clone()),
                None => item.extra.push(node.clone()),
            },
        }
    }
    item.takes.extend(take);
    if item.takes.is_empty() {
        item.takes.push(empty_take());
    }
    item.active_take = active.min(item.takes.len() - 1);
    item
}

/// `FADEIN|FADEOUT <shape> <length> <auto length> <?> <?> <curvature> <?>`. The fourth field
/// usually repeats the shape, but REAPER renders follow the first one.|FADEOUT <shape> <length> <auto length> <shape> <?> <curvature> <?>`.
fn parse_fade(t: &[String]) -> Fade {
    Fade {
        length: num(t, 2, 0.0),
        auto_length: num(t, 3, 0.0),
        shape: num(t, 1, 0.0).clamp(0.0, 6.0) as u8,
        curve: num(t, 6, 0.0),
    }
}

fn fade_line(lines: &LineMemo, key: &str, f: &Fade) -> Vec<String> {
    let mut set = vec![(2, fmt_num(f.length)), (3, fmt_num(f.auto_length)), (6, fmt_num(f.curve))];
    // The shape field can carry a fraction (`1.1`) that REAPER renders as its integer part; only
    // rewrite it, and the fourth field when it mirrors it, if the shape actually changed.
    let old = lines.get(key);
    if old.is_none_or(|t| num(t, 1, 0.0).clamp(0.0, 6.0) as u8 != f.shape) {
        set.push((1, f.shape.to_string()));
        if old.is_none_or(|t| num(t, 1, -1.0) == num(t, 4, -2.0)) {
            set.push((4, f.shape.to_string()));
        }
    }
    line_from(lines, key, &[key, "1", "0", "0", "1", "0", "0", "0"], &set)
}

fn empty_take() -> Take {
    Take {
        name: String::new(),
        source: Source::Empty,
        source_offset: 0.0,
        playrate: 1.0,
        preserve_pitch: true,
        pitch: 0.0,
        volume: 1.0,
        pan: 0.0,
        lines: LineMemo::new(),
        extra: Vec::new(),
    }
}

const FILE_SOURCE_TYPES: &[&str] = &["WAVE", "MP3", "FLAC", "VORBIS", "OPUS", "WAVPACK"];

fn parse_source(c: &Chunk) -> Source {
    let kind = c.args().into_iter().next().unwrap_or_default();
    if kind == "EMPTY" {
        return Source::Empty;
    }
    if kind == "SECTION" {
        let Some(inner) = c.chunks("SOURCE").next() else { return Source::Raw(c.clone()) };
        let line = |key| c.find_line(key).map_or(0.0, |t| num(&t, 0, 0.0));
        return Source::Section(Section {
            inner: Box::new(parse_source(inner)),
            start: line("STARTPOS"),
            length: line("LENGTH"),
            reverse: line("MODE") as i64 & 2 != 0,
            raw: c.clone(),
        });
    }
    let file = c.find_line("FILE").and_then(|t| t.into_iter().next());
    match file {
        Some(f) if FILE_SOURCE_TYPES.contains(&kind.as_str()) && c.children.len() == 1 => {
            Source::File(PathBuf::from(f))
        }
        _ => Source::Raw(c.clone()),
    }
}

/// The take-level memo `write_source` uses to keep a file source's type and flags.
fn source_memo(c: &Chunk) -> LineMemo {
    let mut memo = LineMemo::new();
    memo.insert("SOURCE".into(), tokenize(&c.header));
    if let Some(Node::Line(file)) = c.children.first() {
        memo.insert("FILE".into(), tokenize(file));
    }
    memo
}

fn write_section(s: &Section) -> Chunk {
    let mut c = Chunk::new("SOURCE", &["SECTION"]);
    c.header = s.raw.header.clone();
    let mode = s.raw.find_line("MODE").map_or(0.0, |t| num(&t, 0, 0.0)) as i64;
    let mode = if s.reverse { mode | 2 } else { mode & !2 };
    let (mut wrote_len, mut wrote_start, mut wrote_mode) = (false, false, false);
    for node in &s.raw.children {
        match node {
            Node::Line(l) => match line_key(l) {
                "LENGTH" => {
                    c.line(["LENGTH".to_owned(), fmt_num(s.length)]);
                    wrote_len = true;
                }
                "STARTPOS" => {
                    c.line(["STARTPOS".to_owned(), fmt_num(s.start)]);
                    wrote_start = true;
                }
                "MODE" => {
                    c.line(["MODE".to_owned(), mode.to_string()]);
                    wrote_mode = true;
                }
                _ => c.children.push(node.clone()),
            },
            Node::Chunk(inner) if inner.name() == "SOURCE" => c.push_chunk(write_source(&s.inner, &source_memo(inner))),
            Node::Chunk(_) => c.children.push(node.clone()),
        }
    }
    if !wrote_len {
        c.children.insert(0, Node::Line(format!("LENGTH {}", fmt_num(s.length))));
    }
    if !wrote_start {
        c.children.insert(0, Node::Line(format!("STARTPOS {}", fmt_num(s.start))));
    }
    if !wrote_mode && mode != 0 {
        c.children.insert(0, Node::Line(format!("MODE {mode}")));
    }
    if !c.children.iter().any(|n| matches!(n, Node::Chunk(_))) {
        c.push_chunk(write_source(&s.inner, &LineMemo::new()));
    }
    c
}

fn source_kind_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("mp3") => "MP3",
        Some("flac") => "FLAC",
        Some("ogg") => "VORBIS",
        Some("opus") => "OPUS",
        Some("wv") => "WAVPACK",
        _ => "WAVE",
    }
}

pub fn project_to_rpp(p: &Project) -> String {
    let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let version = format!("scythe-{}", env!("CARGO_PKG_VERSION"));
    let mut root = Chunk::new("REAPER_PROJECT", &[PROJECT_VERSION, &version, &timestamp.to_string()]);

    let l = &p.lines;
    let t = &p.tempo;
    root.line(line_from(
        l,
        "TEMPO",
        &["TEMPO"],
        &[(1, fmt_num(t.bpm)), (2, t.beats_per_bar.to_string()), (3, t.beat_unit.to_string())],
    ));
    root.line(line_from(l, "LOOP", &["LOOP"], &[(1, bit(p.loop_enabled))]));
    root.line(match p.time_selection {
        Some(sel) => line_from(l, "SELECTION", &["SELECTION"], &[(1, fmt_num(sel.start)), (2, fmt_num(sel.end))]),
        // An empty or reversed selection as stored by REAPER is kept verbatim.
        None if l.get("SELECTION").is_some_and(|t| num(t, 1, 0.0) >= num(t, 2, 0.0)) => l["SELECTION"].clone(),
        None => vec!["SELECTION".into(), "0".into(), "0".into()],
    });
    root.line(line_from(l, "CURSOR", &["CURSOR"], &[(1, fmt_num(p.cursor))]));
    if let Some(sr) = p.sample_rate {
        root.line(line_from(l, "SAMPLERATE", &["SAMPLERATE", "0", "0", "0"], &[(1, sr.to_string())]));
    }
    let m = &p.master;
    root.line(line_from(
        l,
        "MASTER_VOLUME",
        &["MASTER_VOLUME", "1", "0", "-1", "-1", "1"],
        &[(1, fmt_num(m.volume)), (2, fmt_num(m.pan))],
    ));
    root.line(line_from(l, "MASTERMUTESOLO", &["MASTERMUTESOLO"], &[(1, bit(m.muted))]));
    let (mut wrote_law, mut wrote_flags, mut wrote_tempo) = (false, false, false);
    for node in &p.extra {
        match node {
            Node::Line(l) if line_key(l) == "PANLAW" => {
                let mut toks = tokenize(l);
                toks.resize(toks.len().max(2), String::new());
                toks[1] = fmt_num(p.pan_law.law);
                root.line(toks);
                wrote_law = true;
            }
            Node::Line(l) if line_key(l) == "PANLAWFLAGS" => {
                root.line(["PANLAWFLAGS".to_owned(), p.pan_law.taper.flags().to_string()]);
                wrote_flags = true;
            }
            Node::Chunk(c) if c.name() == "TEMPOENVEX" => {
                root.push_chunk(write_tempo_env(Some(c), &p.tempo.points));
                wrote_tempo = true;
            }
            _ => root.children.push(node.clone()),
        }
    }
    if !wrote_tempo && !p.tempo.points.is_empty() {
        root.push_chunk(write_tempo_env(None, &p.tempo.points));
    }
    let default_law = crate::units::PanLaw::default();
    if !wrote_law && p.pan_law.law != default_law.law {
        root.line(["PANLAW".to_owned(), fmt_num(p.pan_law.law)]);
    }
    if !wrote_flags && p.pan_law.taper != default_law.taper {
        root.line(["PANLAWFLAGS".to_owned(), p.pan_law.taper.flags().to_string()]);
    }

    for m in &p.markers {
        let old_flags = m.lines.get("MARKER").map_or(0, |t| num(t, 4, 0.0) as i64);
        let flags = (old_flags & !1) | m.region_end.is_some() as i64;
        root.line(line_from(
            &m.lines,
            "MARKER",
            &["MARKER"],
            &[
                (1, m.index.to_string()),
                (2, fmt_num(m.position)),
                (3, m.name.clone()),
                (4, flags.to_string()),
                (5, color_to_native(m.color).to_string()),
            ],
        ));
        if let Some(end) = m.region_end {
            root.line(line_from(
                &m.lines,
                "MARKER_END",
                &["MARKER", "0", "0", "", "1"],
                &[(1, m.index.to_string()), (2, fmt_num(end))],
            ));
        }
    }

    if !p.master.fx.is_empty() || !p.master.fx_chain_prelude.is_empty() {
        root.push_chunk(write_fx_chain("MASTERFXLIST", &p.master));
    }

    for track in &p.tracks {
        root.push_chunk(write_track(p, track));
    }
    write(&root)
}

fn write_track(p: &Project, t: &Track) -> Chunk {
    let guid = if t.guid.is_empty() { new_guid() } else { t.guid.clone() };
    let mut c = Chunk::new("TRACK", &[&guid]);
    let l = &t.lines;
    c.line(line_from(l, "NAME", &["NAME"], &[(1, t.name.clone())]));
    let peakcol = if t.color.is_some() { color_to_native(t.color) } else { 16576 };
    c.line(line_from(l, "PEAKCOL", &["PEAKCOL"], &[(1, peakcol.to_string())]));
    c.line(line_from(
        l,
        "VOLPAN",
        &["VOLPAN", "1", "0", "-1", "-1", "1"],
        &[(1, fmt_num(t.volume)), (2, fmt_num(t.pan)), (3, fmt_num(t.pan_law))],
    ));
    // Keep a stored solo mode (in-place, safe solo) as long as the track stays soloed.
    let old_solo = l.get("MUTESOLO").map(|t| t.get(2).cloned().unwrap_or_default()).filter(|s| s != "0");
    let solo = if t.solo { old_solo.unwrap_or_else(|| "1".into()) } else { "0".into() };
    c.line(line_from(l, "MUTESOLO", &["MUTESOLO", "0", "0", "0"], &[(1, bit(t.muted)), (2, solo)]));
    let state = match t.folder_depth {
        d if d > 0 => "1",
        d if d < 0 => "2",
        _ => "0",
    };
    c.line(line_from(l, "ISBUS", &["ISBUS"], &[(1, state.into()), (2, t.folder_depth.to_string())]));
    let has_mainsend = t.extra.iter().any(|n| matches!(n, Node::Line(l) if line_key(l) == "MAINSEND"));
    if !has_mainsend && !t.main_send {
        c.line(["MAINSEND", "0", "0"]);
    }

    let mut wrote_rec = false;
    let mut wrote_envs = Vec::new();
    for node in &t.extra {
        match node {
            Node::Chunk(ch) if let Some(kind) = EnvelopeKind::from_chunk_name(ch.name()) => {
                if let Some(env) = t.envelope(kind) {
                    c.push_chunk(write_envelope(env));
                }
                wrote_envs.push(kind);
            }
            Node::Line(l) if line_key(l) == "MAINSEND" => {
                let mut toks = tokenize(l);
                if toks.len() > 1 {
                    toks[1] = if t.main_send { "1" } else { "0" }.to_owned();
                }
                c.line(toks);
            }
            Node::Line(l) if line_key(l) == "REC" => {
                let mut toks = tokenize(l);
                if toks.len() > 1 {
                    toks[1] = if t.armed { "1" } else { "0" }.to_owned();
                }
                if toks.len() > 2 {
                    toks[2] = t.rec_input.to_string();
                }
                // REAPER has several monitoring modes; keep the stored one while it agrees on on/off.
                if toks.len() > 3 && (num(&toks, 3, 0.0) != 0.0) != t.monitor {
                    toks[3] = bit(t.monitor);
                }
                c.line(toks);
                wrote_rec = true;
            }
            _ => c.children.push(node.clone()),
        }
    }
    if !wrote_rec && t.armed {
        c.line(["REC", "1", &t.rec_input.to_string(), &bit(t.monitor), "0", "0", "0", "0", "0"]);
    }
    for env in t.envelopes.iter().filter(|e| !wrote_envs.contains(&e.kind)) {
        c.push_chunk(write_envelope(env));
    }

    for (src_idx, src) in p.tracks.iter().enumerate() {
        for s in src.sends.iter().filter(|s| s.dest == t.id) {
            let mut toks: Vec<String> = s.raw.clone().filter(|r| r.len() >= 6).unwrap_or_else(|| {
                ["AUXRECV", "0", "0", "1", "0", "0", "0", "0", "0", "0", "-1:U", "4177951", "-1", ""]
                    .map(String::from)
                    .to_vec()
            });
            toks[1] = src_idx.to_string();
            toks[3] = fmt_num(s.volume);
            toks[4] = fmt_num(s.pan);
            toks[5] = if s.muted { "1" } else { "0" }.to_owned();
            c.line(toks);
        }
    }

    if !t.fx.is_empty() || !t.fx_chain_prelude.is_empty() {
        c.push_chunk(write_fx_chain("FXCHAIN", t));
    }
    for item in &t.items {
        c.push_chunk(write_item(item));
    }
    c
}

fn write_fx_chain(name: &str, t: &Track) -> Chunk {
    let mut c = Chunk::new(name, &[]);
    c.children.extend(t.fx_chain_prelude.iter().cloned());
    for slot in &t.fx {
        for (i, node) in slot.raw.iter().enumerate() {
            match node {
                Node::Line(l) if i == 0 && line_key(l) == "BYPASS" => {
                    let mut toks = tokenize(l);
                    toks[1] = if slot.enabled { "0" } else { "1" }.to_owned();
                    c.line(toks);
                }
                _ => c.children.push(node.clone()),
            }
        }
    }
    c
}

fn write_item(it: &Item) -> Chunk {
    let mut c = Chunk::new("ITEM", &[]);
    let l = &it.lines;
    c.line(line_from(l, "POSITION", &["POSITION"], &[(1, fmt_num(it.position))]));
    c.line(line_from(l, "LENGTH", &["LENGTH"], &[(1, fmt_num(it.length))]));
    c.line(line_from(l, "LOOP", &["LOOP"], &[(1, bit(it.looped))]));
    c.line(fade_line(l, "FADEIN", &it.fade_in));
    c.line(fade_line(l, "FADEOUT", &it.fade_out));
    c.line(line_from(l, "MUTE", &["MUTE", "0", "0"], &[(1, bit(it.muted))]));
    c.line(["IGUID", &it.guid]);
    c.line(line_from(l, "VOLPAN", &["VOLPAN", "1", "0", "1", "-1"], &[(1, fmt_num(it.volume))]));
    if let Some((top, height)) = it.ypos {
        c.line(line_from(l, "YPOS", &["YPOS", "0", "1", "2"], &[(1, fmt_num(top)), (2, fmt_num(height))]));
    }
    c.children.extend(it.extra.iter().cloned());

    for (i, tk) in it.takes.iter().enumerate() {
        let l = &tk.lines;
        // Take 0 only gets a TAKE line when REAPER wrote one (e.g. to mark it selected).
        if i > 0 || l.contains_key("TAKE") {
            let mut take = line_from(l, "TAKE", &["TAKE"], &[]);
            take.retain(|t| t != "SEL");
            if i == it.active_take && (i > 0 || l.contains_key("TAKE")) {
                take.insert(1, "SEL".into());
            }
            let null = take.iter().any(|t| t == "NULL");
            c.line(take);
            // An empty take lane is just its TAKE NULL line.
            if null && tk.name.is_empty() && tk.extra.is_empty() && tk.source == Source::Empty {
                continue;
            }
        }
        c.line(line_from(l, "NAME", &["NAME"], &[(1, tk.name.clone())]));
        if tk.volume != 1.0 || tk.pan != 0.0 || l.contains_key("TAKEVOLPAN") {
            c.line(line_from(
                l,
                "TAKEVOLPAN",
                &["TAKEVOLPAN", "0", "1", "-1"],
                &[(1, fmt_num(tk.pan)), (2, fmt_num(tk.volume))],
            ));
        }
        c.line(line_from(l, "SOFFS", &["SOFFS"], &[(1, fmt_num(tk.source_offset))]));
        c.line(line_from(
            l,
            "PLAYRATE",
            &["PLAYRATE", "1", "1", "0", "-1", "0", "0.0025"],
            &[(1, fmt_num(tk.playrate)), (2, (tk.preserve_pitch as u8).to_string()), (3, fmt_num(tk.pitch))],
        ));
        c.children.extend(tk.extra.iter().cloned());
        c.push_chunk(write_source(&tk.source, l));
    }
    c
}

fn write_source(source: &Source, l: &LineMemo) -> Chunk {
    match source {
        Source::File(path) => {
            // Keep REAPER's source type and any flags after the path unless the file changed.
            let same_file = l.get("FILE").and_then(|t| t.get(1)).is_some_and(|f| Path::new(f) == path);
            let kind = match l.get("SOURCE") {
                Some(t) if same_file && t.len() > 1 => t[1].clone(),
                _ => source_kind_for(path).to_owned(),
            };
            let mut s = Chunk::new("SOURCE", &[&kind]);
            let mut file = format!("FILE {}", quote_always(&path.to_string_lossy()));
            if same_file && let Some(t) = l.get("FILE") {
                for extra in t.iter().skip(2) {
                    file.push(' ');
                    file.push_str(&super::quote(extra));
                }
            }
            s.children.push(Node::Line(file));
            s
        }
        Source::Empty => Chunk::new("SOURCE", &["EMPTY"]),
        Source::Section(s) => write_section(s),
        Source::Raw(raw) => raw.clone(),
    }
}

/// Resolves a take's source path against the project directory, as REAPER stores media
/// relative to the project file when it lives below it.
pub fn resolve_media_path(project: &Project, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_owned();
    }
    match project.path.as_deref().and_then(Path::parent) {
        Some(dir) => dir.join(path),
        None => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<REAPER_PROJECT 0.1 "7.73/linux-x86_64" 1762500000
  RIPPLE 0
  TEMPO 168 4 4
  LOOP 1
  SELECTION 64 96
  MASTER_VOLUME 0.8 0 -1 -1 1
  MARKER 1 64 "Verse 2" 0 0 1 B {AAAAAAAA-0000-0000-0000-000000000001} 0
  MARKER 2 80 Chorus 1 16777471 1 B {AAAAAAAA-0000-0000-0000-000000000002} 0
  MARKER 2 90 "" 1
  <RECORD_CFG
    ZXZhdxgAAQ==
  >
  <TRACK {11111111-2222-3333-4444-555555555555}
    NAME "Guitar L · 7-str"
    PEAKCOL 31166068
    VOLPAN 0.72 -0.8 -1 -1 1
    MUTESOLO 0 1 0
    REC 1 2 1 0 0 0 0 0
    NCHAN 2
    <FXCHAIN
      SHOW 0
      BYPASS 0 0 0
      <VST "VST: ReaEQ (Cockos)" reaeq.vst.so 0 "" 1919247729<56535472656571726561657100000000> ""
        cWVyUu5e7f4CAAAAAQAAAAAAAAACAAAAAAAAAAIAAAABAAAAAAAAAAIAAAAAAAAAPAAAAAAAAAAAABAAAAAAAAAAAAAAAAAAAAAA
      >
      FXID {FFFFFFFF-0000-0000-0000-000000000000}
      BYPASS 1 0 0
      <JS utility/volume ""
        0 - - - - - - - -
      >
    >
    <ITEM
      POSITION 64
      LENGTH 16
      FADEIN 1 0.01 0 1 0 0 0
      FADEOUT 1 0.02 0 1 0 0 0
      MUTE 0 0
      IGUID {99999999-0000-0000-0000-000000000000}
      NAME Rhythm_L_v2.wav
      VOLPAN 0.5 0 1 -1
      SOFFS 1.5
      PLAYRATE 1 1 0 -1 0 0.0025
      <SOURCE WAVE
        FILE "media/Rhythm L v2.wav"
      >
      TAKE SEL
      NAME "comp take"
      TAKEVOLPAN 0.25 0.5 -1
      SOFFS 0
      <SOURCE MIDI
        HASDATA 1 960 QN
        E 0 90 3c 60
      >
    >
  >
  <TRACK {77777777-2222-3333-4444-555555555555}
    NAME "Bus"
    ISBUS 1 1
    VOLPAN 1 0 -1 0.5 1
  >
  <TRACK {88888888-2222-3333-4444-555555555555}
    NAME "In bus"
    MAINSEND 0 0
    ISBUS 2 -1
  >
  <TRACK {66666666-2222-3333-4444-555555555555}
    NAME "Room reverb"
    AUXRECV 0 0 0.125 0 0 0 0 0 0 -1:U 4177951 -1 ''
  >
>
"#;

    #[test]
    fn reads_project_fields() {
        let p = project_from_rpp(SAMPLE).unwrap();
        assert_eq!(p.tempo.bpm, 168.0);
        assert!(p.loop_enabled);
        assert_eq!(p.time_selection, Some(TimeRange::new(64.0, 96.0)));
        assert_eq!(p.master.volume, 0.8);
        assert_eq!(p.markers.len(), 2);
        assert_eq!(p.markers[1].name, "Chorus");
        assert_eq!(p.markers[1].region_end, Some(90.0));
        assert_eq!(p.markers[1].color, Some(Color::from_hex(0x0000ff)));

        let g = &p.tracks[0];
        assert_eq!(g.name, "Guitar L · 7-str");
        assert_eq!(g.color, Some(Color::from_hex(0xdb_8e_74)));
        assert_eq!((g.volume, g.pan, g.solo, g.armed), (0.72, -0.8, true, true));
        assert_eq!(g.fx.len(), 2);
        assert_eq!(g.fx[0].name, "VST: ReaEQ (Cockos)");
        assert!(g.fx[0].enabled && !g.fx[1].enabled);
        assert_eq!(g.fx[1].name, "utility/volume");

        let it = &g.items[0];
        assert_eq!((it.position, it.length, it.fade_out.length, it.volume), (64.0, 16.0, 0.02, 0.5));
        assert_eq!(it.takes.len(), 2);
        assert_eq!(it.active_take, 1);
        assert_eq!(it.takes[0].source, Source::File("media/Rhythm L v2.wav".into()));
        assert_eq!(it.takes[0].source_offset, 1.5);
        assert!(matches!(it.takes[1].source, Source::Raw(_)));
        assert_eq!((it.takes[1].pan, it.takes[1].volume), (0.25, 0.5));

        assert_eq!((p.tracks[1].folder_depth, p.tracks[2].folder_depth), (1, -1));
        assert!(!p.tracks[2].main_send);

        let send = &p.tracks[0].sends[0];
        assert_eq!(send.dest, p.tracks[3].id);
        assert_eq!(send.volume, 0.125);
    }

    #[test]
    fn round_trip_is_stable() {
        let p1 = project_from_rpp(SAMPLE).unwrap();
        let text1 = project_to_rpp(&p1);
        let p2 = project_from_rpp(&text1).unwrap();
        let text2 = project_to_rpp(&p2);
        let strip = |s: &str| s.lines().skip(1).collect::<Vec<_>>().join("\n");
        assert_eq!(strip(&text1), strip(&text2));
        assert!(text1.contains("<RECORD_CFG\n    ZXZhdxgAAQ==\n  >"));
        assert!(text1.contains("NCHAN 2"));
        assert!(text1.contains("cWVyUu5e7f4"));
        assert!(text1.contains("E 0 90 3c 60"));
        assert_eq!(p2.tracks[0].fx.len(), 2);
        assert_eq!(p2.tracks[0].sends.len(), 1);
        assert_eq!(p2.markers, p1.markers);
        assert!(text1.contains("AUXRECV 0 0 0.125 0 0 0 0 0 0 -1:U 4177951 -1 \"\""));
        assert!(text1.contains("VOLPAN 1 0 -1 0.5 1"));
        assert!(text1.contains("ISBUS 2 -1"));
        assert!(text1.contains("MAINSEND 0 0"));
        assert_eq!(p2.tracks[2].folder_depth, -1);
    }

    #[test]
    fn edits_survive_round_trip() {
        let mut p = project_from_rpp(SAMPLE).unwrap();
        p.tracks[0].fx[1].enabled = true;
        p.tracks[0].armed = false;
        p.tracks[0].items[0].position = 70.0;
        let p2 = project_from_rpp(&project_to_rpp(&p)).unwrap();
        assert!(p2.tracks[0].fx[1].enabled);
        assert!(!p2.tracks[0].armed);
        assert_eq!(p2.tracks[0].items[0].position, 70.0);
    }

    #[test]
    fn autosave_names() {
        let p = autosave_path(Path::new("/songs/Song.RPP"), 1_762_552_871);
        assert_eq!(p, PathBuf::from("/songs/Backups/Song-2025-11-07_220111.rpp-bak"));
    }

    #[test]
    fn rejects_other_roots() {
        assert!(matches!(project_from_rpp("<FXCHAIN\n>\n"), Err(ParseError::WrongRoot { .. })));
    }
}

#[cfg(test)]
mod section_tests {
    use super::*;

    const REVERSED: &str = r#"<REAPER_PROJECT 0.1 "7.73/linux-x86_64" 1762500000
  PANLAW 0.5
  PANLAWFLAGS 0
  <TRACK
    VOLPAN 1 0 0.707 -1 1
    <ITEM
      POSITION 1
      LENGTH 2
      FADEIN 5 0.25 0 5 0 0.5 0
      FADEOUT 1 0.01 0 1 0 0 0
      <SOURCE SECTION
        LENGTH 2
        MODE 2
        STARTPOS 1.5
        OVERLAP 0.01
        <SOURCE WAVE
          FILE "drums.wav" 1
        >
      >
    >
  >
>
"#;

    #[test]
    fn reads_sections_fades_and_pan_law() {
        let p = project_from_rpp(REVERSED).unwrap();
        assert_eq!(p.pan_law.law, 0.5);
        assert_eq!(p.pan_law.taper, PanTaper::Sine);
        assert_eq!(p.tracks[0].pan_law, 0.707);
        let it = &p.tracks[0].items[0];
        assert_eq!((it.fade_in.shape, it.fade_in.length, it.fade_in.curve), (5, 0.25, 0.5));
        let Source::Section(s) = &it.takes[0].source else { panic!("expected a section") };
        assert_eq!((s.start, s.length, s.reverse), (1.5, 2.0, true));
        assert_eq!(*s.inner, Source::File("drums.wav".into()));
        assert_eq!(it.takes[0].source.file(), Some(&PathBuf::from("drums.wav")));
    }

    #[test]
    fn edits_to_sections_and_fades_are_written() {
        let mut p = project_from_rpp(REVERSED).unwrap();
        let it = &mut p.tracks[0].items[0];
        it.fade_in.shape = 2;
        let Source::Section(s) = &mut it.takes[0].source else { panic!() };
        s.reverse = false;
        s.start = 3.0;
        p.pan_law.law = 1.0;
        let text = project_to_rpp(&p);
        assert!(text.contains("FADEIN 2 0.25 0 2 0 0.5 0"), "{text}");
        assert!(text.contains("MODE 0") && text.contains("STARTPOS 3") && text.contains("OVERLAP 0.01"));
        assert!(text.contains("FILE \"drums.wav\" 1"));
        assert!(text.contains("PANLAW 1\n") && text.contains("VOLPAN 1 0 0.707 -1 1"));
        let again = project_from_rpp(&text).unwrap();
        let Source::Section(s) = &again.tracks[0].items[0].takes[0].source else { panic!() };
        assert_eq!((s.start, s.reverse), (3.0, false));
    }
}
