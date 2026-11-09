use std::path::{Path, PathBuf};

use super::{Chunk, Node, ParseError, line_key, parse, quote_always, tokenize, write};
use crate::envelope::{EnvPoint, Envelope, EnvelopeKind, PointShape};
use crate::fade::Fade;
use crate::fader::PanMode;
use crate::groups::GroupFlags;
use crate::lanes::{COMP_FADE, CompArea};
use crate::model::{
    AutoMode, ChannelMode, Color, FxSlot, GlobalAutomation, HwOut, Item, LineMemo, Marker, Processing, Project,
    RegionRenderTrack,
    Section, Send, SendMode, SoloMode, Source, Take, TimeRange, Track, TrackId, new_guid,
};
use crate::recording::{RecordMode, RecordTap};
use crate::tempo::TempoPoint;
use crate::timebase::{PlayRate, Timebase};
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

/// Deletes all but the newest `keep` backups [`autosave_path`] made for `path`.
pub fn prune_backups(path: &Path, keep: usize) {
    let Some(dir) = autosave_path(path, 0).parent().map(Path::to_path_buf) else { return };
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut backups: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            name.strip_prefix(&stem)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|rest| rest.len() == "YYYY-MM-DD_HHMMSS.rpp-bak".len() && rest.ends_with(".rpp-bak"))
        })
        .collect();
    // The timestamp in the name sorts oldest first.
    backups.sort();
    let excess = backups.len().saturating_sub(keep);
    for old in &backups[..excess] {
        let _ = std::fs::remove_file(old);
    }
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
    let mut master_hw: Option<Vec<HwOut>> = None;

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
                    // A bare `MASTERHWOUT` is how REAPER saves a master without hardware outputs;
                    // a project without the line gets the default pair.
                    "MASTERHWOUT" => {
                        let outs = master_hw.get_or_insert_with(|| {
                            p.extra.push(Node::Line("MASTERHWOUT".into()));
                            Vec::new()
                        });
                        if t.len() > 1 {
                            outs.push(parse_hw_out(&t));
                        }
                    }
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
                    "MASTER_NCH" => {
                        p.master.channels = (num(&t, 1, 2.0) as u32).clamp(2, 128);
                        p.extra.push(node.clone());
                    }
                    "MARKER" => parse_marker(&t, &mut p.markers, &mut region_starts),
                    "PANLAW" => {
                        p.pan_law.law = num(&t, 1, 1.0);
                        p.extra.push(node.clone());
                    }
                    "DEFPITCHMODE" => {
                        p.default_pitch_mode = num(&t, 1, Processing::DEFAULT_MODE as f64) as i32;
                        p.extra.push(node.clone());
                    }
                    "PANLAWFLAGS" => {
                        p.pan_law.taper = PanTaper::from_flags(num(&t, 1, 3.0) as i64);
                        p.extra.push(node.clone());
                    }
                    "PANMODE" => {
                        p.pan_mode = PanMode::from_code(num(&t, 1, 3.0) as i64);
                        p.extra.push(node.clone());
                    }
                    "GROUP_NAME" => {
                        if p.group_names.is_empty() {
                            p.extra.push(Node::Line("GROUP_NAME".into()));
                        }
                        let group = num(&t, 1, -1.0);
                        if (0.0..crate::groups::GROUP_COUNT as f64).contains(&group) {
                            p.group_names.insert(group as usize, t.get(2).cloned().unwrap_or_default());
                        }
                    }
                    "TIMELOCKMODE" => {
                        p.timebase = Timebase::from_rpp(num(&t, 1, 1.0) as i64).unwrap_or_default();
                        p.extra.push(node.clone());
                    }
                    "TEMPOENVLOCKMODE" => {
                        p.tempo_follows_beats = flag(&t, 1);
                        p.extra.push(node.clone());
                    }
                    "PLAYRATE" => {
                        p.playrate = PlayRate { rate: num(&t, 1, 1.0), preserve_pitch: flag(&t, 2) };
                        p.extra.push(node.clone());
                    }
                    "METRONOMEOUT" => p.metronome.outputs.push(num(&t, 1, 0.0) as i64),
                    "SMPTESYNC" => {
                        p.timecode_sync = crate::midi_route::TimecodeSync::parse(&t[1..]);
                        p.extra.push(node.clone());
                    }
                    "GLOBAL_AUTO" => {
                        p.global_automation = GlobalAutomation::from_rpp(num(&t, 1, -1.0) as i64);
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
            Node::Chunk(c) if c.name() == "METRONOME" => {
                parse_metronome(c, &mut p.metronome);
                p.extra.push(node.clone());
            }
            Node::Chunk(c) if c.name() == "POOLEDENV" => {
                if p.automation_pools.is_empty() {
                    p.extra.push(node.clone());
                }
                p.automation_pools.push(crate::automation_item::AutomationPool::parse(c));
            }
            Node::Chunk(c) if c.name() == "MASTERFXLIST" => {
                let (fx, prelude) = parse_fx_chain(c);
                p.master.fx = fx;
                p.master.fx_chain_prelude = prelude;
            }
            // REAPER writes a region's matrix row right after the line that starts the region.
            Node::Chunk(c) if c.name() == "REGIONRENDER" && !region_starts.is_empty() => {
                let index = tokenize(&c.header).get(1).and_then(|v| v.parse::<u32>().ok());
                let region = region_starts.iter_mut().rev().find(|r| Some(r.index) == index);
                let region = match region {
                    Some(r) => r,
                    None => region_starts.last_mut().unwrap(),
                };
                region.render_matrix = c
                    .children
                    .iter()
                    .filter_map(|n| match n {
                        Node::Line(l) => {
                            let t = tokenize(l);
                            (t.first().map(String::as_str) == Some("TRACK") && t.len() > 1)
                                .then(|| RegionRenderTrack { guid: t[1].clone(), channels: num(&t, 2, 0.0) as i64 })
                        }
                        Node::Chunk(_) => None,
                    })
                    .collect();
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
    if let Some(outs) = master_hw {
        p.master.hw_outs = outs;
    }
    p.markers.extend(region_starts);
    p.markers.sort_by(|a, b| a.position.total_cmp(&b.position).then(a.index.cmp(&b.index)));
    p.time_selection = selection;
    p.resolve_automation_items();
    Ok(p)
}

/// `<METRONOME <flags> <count-in measures>` with `VOL <primary> <secondary>` inside.
fn parse_metronome(c: &Chunk, m: &mut crate::metronome::Metronome) {
    let args = c.args();
    m.flags = args.first().and_then(|v| v.parse().ok()).unwrap_or(m.flags);
    m.count_in_measures = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(m.count_in_measures);
    if let Some(vol) = c.find_line("VOL").and_then(|v| v.first()?.parse().ok()) {
        m.volume = vol;
    }
}

fn write_metronome(root: &mut Chunk, original: Option<&Chunk>, m: &crate::metronome::Metronome) {
    let mut chunk = original.cloned().unwrap_or_else(|| Chunk::new("METRONOME", &[]));
    let mut args = chunk.args();
    args.resize(args.len().max(2), String::new());
    args[0] = m.flags.to_string();
    args[1] = fmt_num(m.count_in_measures);
    chunk.header = super::join_tokens(std::iter::once("METRONOME".to_owned()).chain(args));
    let vol = chunk.children.iter_mut().find_map(|n| match n {
        Node::Line(l) if line_key(l) == "VOL" => Some(l),
        _ => None,
    });
    match vol {
        Some(line) => {
            let mut toks = tokenize(line);
            toks.resize(toks.len().max(2), String::new());
            toks[1] = fmt_num(m.volume);
            *line = super::join_tokens(toks);
        }
        None if m.volume != crate::metronome::Metronome::default().volume => {
            chunk.line(["VOL".to_owned(), fmt_num(m.volume), fmt_num(m.volume / 2.0)]);
        }
        None => {}
    }
    root.push_chunk(chunk);
    for o in &m.outputs {
        root.line(["METRONOMEOUT".to_owned(), o.to_string()]);
    }
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
    // A point made in Scythe has no line of its own to keep fields of.
    let memo = if pt.raw.is_empty() { LineMemo::new() } else { LineMemo::from([("PT".to_owned(), pt.raw.clone())]) };
    line_from(&memo, "PT", &["PT"], &set)
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
        markers.push(Marker { index, position, name, color, region_end: None, render_matrix: Vec::new(), lines });
    } else if let Some(pos) = open_regions.iter().position(|m| m.index == index && m.region_end.is_none()) {
        let mut region = open_regions.remove(pos);
        region.region_end = Some(position);
        region.lines.insert("MARKER_END".into(), t.to_vec());
        markers.push(region);
    } else {
        open_regions.push(Marker { index, position, name, color, region_end: None, render_matrix: Vec::new(), lines });
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
                        track.dual_pan = [num(&t, 4, -1.0), num(&t, 5, 1.0)];
                    }
                    "ISBUS" => track.folder_depth = num(&t, 2, 0.0) as i32,
                    "MAINSEND" => {
                        track.main_send = flag(&t, 1);
                        track.main_send_offset = num(&t, 2, 0.0).max(0.0) as u32;
                        track.extra.push(node.clone());
                    }
                    "NCHAN" => {
                        track.channels = (num(&t, 1, 2.0) as u32).clamp(2, 128);
                        track.extra.push(node.clone());
                    }
                    "MUTESOLO" => {
                        track.muted = flag(&t, 1);
                        let solo = SoloMode::from_code(num(&t, 2, 0.0) as i64);
                        track.solo = solo.is_some();
                        (track.solo_mode, track.solo_safe) = solo.unwrap_or((SoloMode::InPlace, false));
                        track.solo_defeat = flag(&t, 3);
                    }
                    // REAPER writes `MIDIOUT device*32+channel`, -1 for none; earlier scythe
                    // versions wrote the same code as `MIDIHWOUT`.
                    "MIDIOUT" | "MIDIHWOUT" => {
                        let v = num(&t, 1, -1.0) as i64;
                        track.midi_out = (v >= 0).then_some(((v >> 5) as u8, (v & 31) as u8));
                        track.extra.push(node.clone());
                    }
                    "IPHASE" => {
                        track.phase = flag(&t, 1);
                        track.extra.push(node.clone());
                    }
                    "PANMODE" => {
                        let code = num(&t, 1, -1.0) as i64;
                        track.pan_mode = (code >= 0).then(|| PanMode::from_code(code));
                        track.extra.push(node.clone());
                    }
                    "WIDTH" => {
                        track.width = num(&t, 1, 1.0);
                        track.extra.push(node.clone());
                    }
                    "GROUP_FLAGS" | "GROUP_FLAGS_HIGH" => {
                        track.groups.read_line(&t[1..], t[0] == "GROUP_FLAGS_HIGH");
                        track.extra.push(node.clone());
                    }
                    "FREEMODE" => {
                        track.fixed_lanes = num(&t, 1, 0.0) as i64 == 2;
                        track.extra.push(node.clone());
                    }
                    "BEAT" => {
                        track.timebase = Timebase::from_rpp(num(&t, 1, -1.0) as i64);
                        track.extra.push(node.clone());
                    }
                    "LANESOLO" => {
                        track.lane_solo = Some(t[1..].iter().map(|v| v.parse::<i64>().unwrap_or(0) as u32).collect());
                        track.extra.push(node.clone());
                    }
                    "AUTOMODE" => {
                        track.auto_mode = AutoMode::from_rpp(num(&t, 1, 0.0) as i64);
                        track.extra.push(node.clone());
                    }
                    "REC" => {
                        track.armed = flag(&t, 1);
                        track.rec_input = crate::midi_route::rec_input_from_rpp(num(&t, 2, 0.0) as i64);
                        let monitor = num(&t, 3, 1.0) as i64;
                        (track.monitor, track.monitor_auto) = (monitor != 0, monitor == 2);
                        track.rec_mode = RecordMode::from_code(num(&t, 4, 0.0) as i64);
                        track.rec_tap = RecordTap::from_code(num(&t, 8, 0.0) as i64);
                        track.monitor_items = flag(&t, 5);
                        track.extra.push(node.clone());
                    }
                    "AUTO_RECARM" => {
                        track.auto_arm = flag(&t, 1);
                        track.extra.push(node.clone());
                    }
                    "LANENAME" => {
                        track.lane_names = t[1..].to_vec();
                        track.extra.push(node.clone());
                    }
                    "LANEREC" => {
                        track.lane_rec = Some(std::array::from_fn(|i| num(&t, i + 1, -1.0) as i64));
                        track.extra.push(node.clone());
                    }
                    "LINKEDLANE" => {
                        let field = |i: usize, d: &str| t.get(i).cloned().unwrap_or_else(|| d.to_owned());
                        track.comp_areas.push(CompArea {
                            start: num(&t, 1, 0.0),
                            end: num(&t, 2, 0.0),
                            lane: num(&t, 3, 0.0).max(0.0) as usize,
                            fade_in: num(&t, 6, COMP_FADE),
                            fade_out: num(&t, 7, COMP_FADE),
                            extra: [field(4, "0"), field(5, "-1")],
                        });
                        track.extra.push(node.clone());
                    }
                    "HWOUT" => {
                        if track.hw_outs.is_empty() {
                            track.extra.push(Node::Line("HWOUT".into()));
                        }
                        track.hw_outs.push(parse_hw_out(&t));
                    }
                    k if k.starts_with("HWOUT_") && !track.hw_outs.is_empty() => {
                        track.hw_outs.last_mut().unwrap().trailing.push(node.clone());
                    }
                    "AUXRECV" => receives.push((
                        num(&t, 1, 0.0) as usize,
                        Send {
                            dest: TrackId(0),
                            volume: num(&t, 3, 1.0),
                            pan: num(&t, 4, 0.0),
                            muted: flag(&t, 5),
                            mode: SendMode::from_code(num(&t, 2, 0.0) as i64),
                            mono: flag(&t, 6),
                            phase: flag(&t, 7),
                            envelopes: Vec::new(),
                            src_channels: num(&t, 8, 0.0) as i64,
                            dst_channels: num(&t, 9, 0.0) as i64,
                            midi_flags: num(&t, 11, crate::midi_route::SEND_MIDI_OFF as f64) as i64,
                            raw: Some(t.clone()),
                        },
                    )),
                    _ => track.extra.push(node.clone()),
                }
            }
            Node::Chunk(ch) if ch.name() == "ITEM" => track.items.push(parse_item(ch)),
            // A send's envelopes follow its `AUXRECV` line.
            Node::Chunk(ch)
                if let (Some(kind), Some((_, send))) =
                    (EnvelopeKind::from_send_chunk_name(ch.name()), receives.last_mut()) =>
            {
                send.envelopes.push(parse_envelope(kind, ch));
            }
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

/// `HWOUT <dst> <mode> <volume> <pan> <mute> <phase> <src> <pan law>:U <automation mode>`;
/// `MASTERHWOUT` has the same fields up to the pan law, without the `:U`.
fn parse_hw_out(t: &[String]) -> HwOut {
    let law = t.get(8).and_then(|v| v.split(':').next()?.parse().ok()).unwrap_or(-1.0);
    HwOut {
        dst_channels: num(t, 1, 0.0) as i64,
        mode: SendMode::from_code(num(t, 2, 0.0) as i64),
        volume: num(t, 3, 1.0),
        pan: num(t, 4, 0.0),
        muted: flag(t, 5),
        phase: flag(t, 6),
        src_channels: num(t, 7, 0.0) as i64,
        pan_law: law,
        raw: Some(t.to_vec()),
        trailing: Vec::new(),
    }
}

fn hw_out_line(o: &HwOut, key: &str) -> Vec<String> {
    let default: &[&str] = if key == "HWOUT" {
        &["HWOUT", "0", "0", "1", "0", "0", "0", "0", "-1:U", "-1"]
    } else {
        &["MASTERHWOUT", "0", "0", "1", "0", "0", "0", "0", "-1"]
    };
    let mut toks: Vec<String> = match &o.raw {
        Some(r) if r.len() >= 9 => r.clone(),
        _ => default.iter().map(|s| (*s).to_owned()).collect(),
    };
    toks[0] = key.to_owned();
    let suffix = toks[8].find(':').map(|i| toks[8][i..].to_owned()).unwrap_or_default();
    let fields = [
        o.dst_channels.to_string(),
        o.mode.code().to_string(),
        fmt_num(o.volume),
        fmt_num(o.pan),
        bit(o.muted),
        bit(o.phase),
        o.src_channels.to_string(),
        format!("{}{suffix}", fmt_num(o.pan_law)),
    ];
    for (i, v) in fields.into_iter().enumerate() {
        toks[i + 1] = v;
    }
    toks
}

fn write_master_hw_outs(root: &mut Chunk, outs: &[HwOut]) {
    if outs.is_empty() {
        root.line(["MASTERHWOUT"]);
    }
    write_hw_outs(root, outs, "MASTERHWOUT");
}

fn write_hw_outs(c: &mut Chunk, outs: &[HwOut], key: &str) {
    for o in outs {
        c.line(hw_out_line(o, key));
        c.children.extend(o.trailing.iter().cloned());
    }
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
            "VOLTYPE" => env.fader_scaled = kind.is_volume() && num(&t, 1, 0.0) as i64 == 1,
            "POOLEDENVINST" => env.items.push(crate::automation_item::AutomationItem::parse(&t)),
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
    write_envelope_named(env, env.kind.chunk_name())
}

/// Writes an envelope under `name` when it has no original chunk (send and take envelopes
/// share the track kinds but not their chunk names).
fn write_envelope_named(env: &Envelope, name: &str) -> Chunk {
    let mut c = env.raw.clone().unwrap_or_else(|| {
        let mut c = Chunk::new(name, &[]);
        c.line(["EGUID", &new_guid()]);
        c.line(["ACT", "1", "-1"]);
        c.line(["VIS", "1", "1", "1"]);
        c.line(["LANEHEIGHT", "0", "0"]);
        c.line(["ARM", "0"]);
        c.line(["DEFSHAPE", "0", "-1", "-1"]);
        if env.fader_scaled {
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
    crate::automation_item::write_instances(&mut c, &env.items);
    c
}

fn parse_fx_chain(c: &Chunk) -> (Vec<FxSlot>, Vec<Node>) {
    crate::fx_chain::split_chain(&c.children)
}

const TAKE_KEYS: &[&str] = &["NAME", "TAKEVOLPAN", "SOFFS", "PLAYRATE", "CHANMODE", "GUID", "TAKECOLOR", "SM"];

pub(crate) fn parse_item(c: &Chunk) -> Item {
    let mut item = Item::new(0.0, 0.0, empty_take());
    item.takes.clear();
    let mut first_take = None;
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
                            tk.pitch_mode = num(&t, 4, -1.0) as i32;
                        }
                        // Kept in place in `extra` and rewritten from the model on save.
                        "CHANMODE" => {
                            tk.channel_mode = ChannelMode::from_code(num(&t, 1, 0.0) as i64);
                            tk.extra.push(node.clone());
                        }
                        "SM" => {
                            tk.stretch_markers = crate::stretch::parse_markers(&t[1..]);
                            tk.extra.push(node.clone());
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
                    "VOLPAN" => {
                        item.volume = num(&t, 1, 1.0);
                        // The first take's pan and volume follow the item's volume.
                        if t.len() > 3 {
                            first_take = Some((num(&t, 2, 0.0), num(&t, 3, 1.0)));
                        }
                    }
                    "YPOS" => item.ypos = Some((num(&t, 1, 0.0), num(&t, 2, 1.0))),
                    "IGUID" => item.guid = t.get(1).cloned().unwrap_or_default(),
                    "BEAT" => {
                        item.timebase = Timebase::from_rpp(num(&t, 1, -1.0) as i64);
                        item.extra.push(node.clone());
                    }
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
            Node::Chunk(ch)
                if let (Some(kind), Some(tk)) = (EnvelopeKind::from_take_chunk_name(ch.name()), take.as_mut()) =>
            {
                tk.envelopes.push(parse_envelope(kind, ch));
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
    if let (Some((pan, volume)), Some(t)) = (first_take, item.takes.first_mut())
        && !t.lines.contains_key("TAKEVOLPAN")
    {
        (t.pan, t.volume) = (pan, volume);
    }
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
        power: (num(t, 1, 0.0) - 1.1).abs() < 1e-9,
    }
}

fn fade_line(lines: &LineMemo, key: &str, f: &Fade) -> Vec<String> {
    let mut set = vec![(2, fmt_num(f.length)), (3, fmt_num(f.auto_length)), (6, fmt_num(f.curve))];
    // The shape field can carry a fraction (`1.1`, the equal-power variant); only rewrite it, and
    // the fourth field when it mirrors it, if the shape actually changed.
    let old = lines.get(key);
    if old.is_none_or(|t| parse_fade(t).shape != f.shape || parse_fade(t).power != f.power) {
        let shape = if f.power && f.shape == 1 { "1.1".to_owned() } else { f.shape.to_string() };
        set.push((1, shape));
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
        pitch_mode: -1,
        pitch: 0.0,
        volume: 1.0,
        pan: 0.0,
        envelopes: Vec::new(),
        channel_mode: ChannelMode::Normal,
        stretch_markers: Vec::new(),
        lines: LineMemo::new(),
        extra: Vec::new(),
    }
}

/// Sources read from one media file. `VIDEO` items play their file's audio stream as well,
/// `RPP_PROJECT` items (subprojects) the render of another project.
const FILE_SOURCE_TYPES: &[&str] = &["WAVE", "MP3", "FLAC", "VORBIS", "OPUS", "WAVPACK", "VIDEO", "RPP_PROJECT"];

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
        Some("rpp") => "RPP_PROJECT",
        Some("mp4" | "m4v" | "mov" | "mkv" | "webm" | "avi" | "mpg" | "mpeg" | "wmv" | "flv" | "ogv" | "mxf") => {
            "VIDEO"
        }
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
    let (mut wrote_law, mut wrote_flags, mut wrote_tempo, mut wrote_nch) = (false, false, false, false);
    let mut wrote_hw = false;
    let (mut wrote_pan_mode, mut wrote_group_names) = (false, false);
    let mut wrote_metronome = false;
    let mut wrote_sync = false;
    let (mut wrote_global_auto, mut wrote_pools) = (false, false);
    let (mut wrote_timebase, mut wrote_tempo_lock, mut wrote_playrate) = (false, false, false);
    for node in &p.extra {
        match node {
            Node::Line(l) if line_key(l) == "MASTERHWOUT" => {
                if !wrote_hw {
                    write_master_hw_outs(&mut root, &m.hw_outs);
                }
                wrote_hw = true;
            }
            Node::Line(l) if line_key(l) == "MASTER_NCH" => {
                let mut toks = tokenize(l);
                toks.resize(toks.len().max(2), String::new());
                toks[1] = m.channels.to_string();
                root.line(toks);
                wrote_nch = true;
            }
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
            Node::Line(l) if line_key(l) == "PANMODE" => {
                let mut toks = tokenize(l);
                toks.resize(toks.len().max(2), String::new());
                if PanMode::from_code(num(&toks, 1, 3.0) as i64) != p.pan_mode {
                    toks[1] = p.pan_mode.code().to_string();
                }
                root.line(toks);
                wrote_pan_mode = true;
            }
            Node::Line(l) if line_key(l) == "GROUP_NAME" => {
                write_group_names(&mut root, p);
                wrote_group_names = true;
            }
            Node::Line(l) if line_key(l) == "GLOBAL_AUTO" => {
                root.line(["GLOBAL_AUTO".to_owned(), GlobalAutomation::rpp_code(p.global_automation).to_string()]);
                wrote_global_auto = true;
            }
            Node::Chunk(c) if c.name() == "POOLEDENV" => {
                if !wrote_pools {
                    p.automation_pools.iter().for_each(|pool| root.push_chunk(pool.write()));
                }
                wrote_pools = true;
            }
            Node::Line(l) if line_key(l) == "SMPTESYNC" => {
                let mut toks = p.timecode_sync.tokens();
                let old = tokenize(l);
                // Keep REAPER's own spelling of the numbers while nothing changed.
                if crate::midi_route::TimecodeSync::parse(&old[1..]) == p.timecode_sync {
                    toks = old;
                }
                root.line(toks);
                wrote_sync = true;
            }
            Node::Chunk(c) if c.name() == "TEMPOENVEX" => {
                root.push_chunk(write_tempo_env(Some(c), &p.tempo.points));
                wrote_tempo = true;
            }
            Node::Chunk(c) if c.name() == "METRONOME" => {
                if !wrote_metronome {
                    write_metronome(&mut root, Some(c), &p.metronome);
                }
                wrote_metronome = true;
            }
            Node::Line(l) if line_key(l) == "TIMELOCKMODE" => {
                root.line(["TIMELOCKMODE".to_owned(), p.timebase.rpp_code().to_string()]);
                wrote_timebase = true;
            }
            Node::Line(l) if line_key(l) == "TEMPOENVLOCKMODE" => {
                root.line(["TEMPOENVLOCKMODE".to_owned(), bit(p.tempo_follows_beats)]);
                wrote_tempo_lock = true;
            }
            Node::Line(l) if line_key(l) == "PLAYRATE" => {
                let mut toks = tokenize(l);
                toks.resize(toks.len().max(3), "0".to_owned());
                toks[1] = fmt_num(p.playrate.rate);
                toks[2] = bit(p.playrate.preserve_pitch);
                root.line(toks);
                wrote_playrate = true;
            }
            _ => root.children.push(node.clone()),
        }
    }
    if !wrote_global_auto && p.global_automation.is_some() {
        root.line(["GLOBAL_AUTO".to_owned(), GlobalAutomation::rpp_code(p.global_automation).to_string()]);
    }
    if !wrote_pools {
        p.automation_pools.iter().for_each(|pool| root.push_chunk(pool.write()));
    }
    if !wrote_hw && !p.master_default_output() {
        write_master_hw_outs(&mut root, &m.hw_outs);
    }
    if !wrote_metronome && p.metronome != crate::metronome::Metronome::default() {
        write_metronome(&mut root, None, &p.metronome);
    }
    if !wrote_sync && p.timecode_sync != crate::midi_route::TimecodeSync::default() {
        root.line(p.timecode_sync.tokens());
    }
    if !wrote_nch && m.channels != 2 {
        root.line(["MASTER_NCH".to_owned(), m.channels.to_string(), "2".to_owned()]);
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
    if !wrote_pan_mode && p.pan_mode != PanMode::default() {
        root.line(["PANMODE".to_owned(), p.pan_mode.code().to_string()]);
    }
    if !wrote_group_names {
        write_group_names(&mut root, p);
    }
    if !wrote_timebase && p.timebase != Timebase::default() {
        root.line(["TIMELOCKMODE".to_owned(), p.timebase.rpp_code().to_string()]);
    }
    if !wrote_tempo_lock && !p.tempo_follows_beats {
        root.line(["TEMPOENVLOCKMODE", "0"]);
    }
    if !wrote_playrate && p.playrate != PlayRate::default() {
        root.line(vec![
            "PLAYRATE".to_owned(),
            fmt_num(p.playrate.rate),
            bit(p.playrate.preserve_pitch),
            "0.25".to_owned(),
            "4".to_owned(),
        ]);
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
        if m.region_end.is_some() && !m.render_matrix.is_empty() {
            let mut c = Chunk::new("REGIONRENDER", &[&m.index.to_string()]);
            for t in &m.render_matrix {
                c.line(["TRACK", &t.guid, &t.channels.to_string()]);
            }
            root.push_chunk(c);
        }
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

fn write_group_flags(c: &mut Chunk, flags: &GroupFlags) {
    for (key, high) in [("GROUP_FLAGS", false), ("GROUP_FLAGS_HIGH", true)] {
        let values = flags.line_values(high);
        if !values.is_empty() {
            c.line(std::iter::once(key.to_owned()).chain(values).collect::<Vec<_>>());
        }
    }
}

fn write_group_names(root: &mut Chunk, p: &Project) {
    for (g, name) in &p.group_names {
        root.line(["GROUP_NAME".to_owned(), g.to_string(), name.clone()]);
    }
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
        &[
            (1, fmt_num(t.volume)),
            (2, fmt_num(t.pan)),
            (3, fmt_num(t.pan_law)),
            (4, fmt_num(t.dual_pan[0])),
            (5, fmt_num(t.dual_pan[1])),
        ],
    ));
    let solo = if t.solo { t.solo_mode.code(t.solo_safe) } else { 0 };
    c.line(line_from(
        l,
        "MUTESOLO",
        &["MUTESOLO", "0", "0", "0"],
        &[(1, bit(t.muted)), (2, solo.to_string()), (3, bit(t.solo_defeat))],
    ));
    let state = match t.folder_depth {
        d if d > 0 => "1",
        d if d < 0 => "2",
        _ => "0",
    };
    c.line(line_from(l, "ISBUS", &["ISBUS"], &[(1, state.into()), (2, t.folder_depth.to_string())]));
    let has_mainsend = t.extra.iter().any(|n| matches!(n, Node::Line(l) if line_key(l) == "MAINSEND"));
    let has_hw_marker = t.extra.iter().any(|n| matches!(n, Node::Line(l) if line_key(l) == "HWOUT"));
    // New hardware outputs follow `MAINSEND`, as REAPER writes them.
    let mut wrote_hw = false;
    if !has_mainsend && !t.main_send {
        c.line(["MAINSEND", "0", "0"]);
    }
    if !has_mainsend && !has_hw_marker {
        write_hw_outs(&mut c, &t.hw_outs, "HWOUT");
        wrote_hw = true;
    }

    let mut wrote_rec = false;
    let mut wrote_auto_arm = false;
    // FREEMODE, LANESOLO, LANEREC, LANENAME, ITEMLANES, LINKEDLANE.
    let mut wrote_lanes = [false; 6];
    let mut wrote_auto = false;
    let mut wrote_midi_out = false;
    let mut wrote_nchan = false;
    let mut wrote_envs = Vec::new();
    let (mut wrote_phase, mut wrote_pan_mode, mut wrote_width, mut wrote_groups) = (false, false, false, false);
    let mut wrote_beat = false;
    for node in &t.extra {
        match node {
            Node::Chunk(ch) if let Some(kind) = EnvelopeKind::from_chunk_name(ch.name()) => {
                if let Some(env) = t.envelope(kind) {
                    c.push_chunk(write_envelope(env));
                }
                wrote_envs.push(kind);
            }
            Node::Line(l) if matches!(line_key(l), "MIDIOUT" | "MIDIHWOUT") => {
                if !wrote_midi_out {
                    c.line(["MIDIOUT".to_owned(), midi_out_code(t.midi_out).to_string()]);
                }
                wrote_midi_out = true;
            }
            Node::Line(l) if line_key(l) == "AUTOMODE" => {
                c.line(["AUTOMODE".to_owned(), t.auto_mode.rpp_code().to_string()]);
                wrote_auto = true;
            }
            Node::Line(l) if line_key(l) == "MAINSEND" => {
                let mut toks = tokenize(l);
                toks.resize(toks.len().max(3), "0".into());
                toks[1] = if t.main_send { "1" } else { "0" }.to_owned();
                toks[2] = t.main_send_offset.to_string();
                c.line(toks);
                if !has_hw_marker && !wrote_hw {
                    write_hw_outs(&mut c, &t.hw_outs, "HWOUT");
                    wrote_hw = true;
                }
            }
            Node::Line(l) if line_key(l) == "HWOUT" => {
                if !wrote_hw {
                    write_hw_outs(&mut c, &t.hw_outs, "HWOUT");
                    wrote_hw = true;
                }
            }
            Node::Line(l) if line_key(l) == "NCHAN" => {
                c.line(["NCHAN".to_owned(), t.channels.to_string()]);
                wrote_nchan = true;
            }
            Node::Line(l) if line_key(l) == "IPHASE" => {
                c.line(["IPHASE".to_owned(), bit(t.phase)]);
                wrote_phase = true;
            }
            Node::Line(l) if line_key(l) == "PANMODE" => {
                let toks = tokenize(l);
                let code = num(&toks, 1, -1.0) as i64;
                let stored = (code >= 0).then(|| PanMode::from_code(code));
                match t.pan_mode {
                    m if m == stored => c.line(toks),
                    Some(m) => c.line(["PANMODE".to_owned(), m.code().to_string()]),
                    None => {}
                }
                wrote_pan_mode = true;
            }
            Node::Line(l) if line_key(l) == "WIDTH" => {
                c.line(["WIDTH".to_owned(), fmt_num(t.width)]);
                wrote_width = true;
            }
            Node::Line(l) if line_key(l) == "BEAT" => {
                c.line(["BEAT".to_owned(), t.timebase.map_or(-1, |b| b.rpp_code()).to_string()]);
                wrote_beat = true;
            }
            Node::Line(l) if matches!(line_key(l), "GROUP_FLAGS" | "GROUP_FLAGS_HIGH") => {
                if !wrote_groups {
                    write_group_flags(&mut c, &t.groups);
                }
                wrote_groups = true;
            }
            Node::Line(l) if line_key(l) == "REC" => {
                c.line(rec_line(t, tokenize(l)));
                wrote_rec = true;
            }
            Node::Line(l) if line_key(l) == "AUTO_RECARM" => {
                if t.auto_arm {
                    c.line(["AUTO_RECARM", "1"]);
                }
                wrote_auto_arm = true;
            }
            Node::Line(l) if line_key(l) == "FREEMODE" => {
                let stored = num(&tokenize(l), 1, 0.0) as i64;
                let mode = if t.fixed_lanes {
                    2
                } else if stored == 2 {
                    0
                } else {
                    stored
                };
                c.line(["FREEMODE".to_owned(), mode.to_string()]);
                wrote_lanes[0] = true;
            }
            Node::Line(l) if line_key(l) == "LANESOLO" => {
                if !wrote_lanes[1] {
                    write_lane_solo(&mut c, t);
                }
                wrote_lanes[1] = true;
            }
            Node::Line(l) if line_key(l) == "LANEREC" => {
                if let Some(rec) = t.lane_rec {
                    c.line(std::iter::once("LANEREC".to_owned()).chain(rec.iter().map(i64::to_string)));
                }
                wrote_lanes[2] = true;
            }
            Node::Line(l) if line_key(l) == "LANENAME" => {
                if !t.lane_names.is_empty() {
                    c.line(std::iter::once("LANENAME".to_owned()).chain(t.lane_names.iter().cloned()));
                }
                wrote_lanes[3] = true;
            }
            Node::Line(l) if line_key(l) == "ITEMLANES" => {
                if t.fixed_lanes {
                    c.line(["ITEMLANES".to_owned(), t.lane_count().to_string()]);
                }
                wrote_lanes[4] = true;
            }
            Node::Line(l) if line_key(l) == "LINKEDLANE" => {
                if !wrote_lanes[5] {
                    write_comp_areas(&mut c, t);
                }
                wrote_lanes[5] = true;
            }
            _ => c.children.push(node.clone()),
        }
    }
    if !wrote_phase && t.phase {
        c.line(["IPHASE", "1"]);
    }
    if let (false, Some(m)) = (wrote_pan_mode, t.pan_mode) {
        c.line(["PANMODE".to_owned(), m.code().to_string()]);
    }
    if !wrote_width && t.width != 1.0 {
        c.line(["WIDTH".to_owned(), fmt_num(t.width)]);
    }
    if let (false, Some(b)) = (wrote_beat, t.timebase) {
        c.line(["BEAT".to_owned(), b.rpp_code().to_string()]);
    }
    if !wrote_groups {
        write_group_flags(&mut c, &t.groups);
    }
    if !wrote_nchan && t.channels != 2 {
        c.line(["NCHAN".to_owned(), t.channels.to_string()]);
    }
    if !wrote_midi_out && t.midi_out.is_some() {
        c.line(["MIDIOUT".to_owned(), midi_out_code(t.midi_out).to_string()]);
    }
    if !wrote_auto && t.auto_mode != AutoMode::TrimRead {
        c.line(["AUTOMODE".to_owned(), t.auto_mode.rpp_code().to_string()]);
    }
    let default_rec =
        t.monitor && !t.monitor_auto && t.rec_mode == RecordMode::Input && t.rec_tap.code() == 0 && !t.monitor_items;
    if !wrote_rec && (t.armed || !default_rec) {
        c.line(rec_line(t, ["REC", "0", "0", "1", "0", "0", "0", "0", "0"].map(String::from).to_vec()));
    }
    if !wrote_auto_arm && t.auto_arm {
        c.line(["AUTO_RECARM", "1"]);
    }
    if t.fixed_lanes {
        if !wrote_lanes[0] {
            c.line(["FREEMODE", "2"]);
        }
        if !wrote_lanes[1] {
            write_lane_solo(&mut c, t);
        }
        if let (false, Some(rec)) = (wrote_lanes[2], t.lane_rec) {
            c.line(std::iter::once("LANEREC".to_owned()).chain(rec.iter().map(i64::to_string)));
        }
        if !wrote_lanes[3] && !t.lane_names.is_empty() {
            c.line(std::iter::once("LANENAME".to_owned()).chain(t.lane_names.iter().cloned()));
        }
        if !wrote_lanes[4] {
            c.line(["ITEMLANES".to_owned(), t.lane_count().to_string()]);
        }
        if !wrote_lanes[5] {
            write_comp_areas(&mut c, t);
        }
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
            toks.resize(toks.len().max(10), "0".into());
            toks[2] = s.mode.code().to_string();
            toks[6] = bit(s.mono);
            toks[7] = bit(s.phase);
            toks[8] = s.src_channels.to_string();
            toks[9] = s.dst_channels.to_string();
            toks.resize(toks.len().max(12), "-1".into());
            toks[11] = s.midi_flags.to_string();
            c.line(toks);
            for env in &s.envelopes {
                if let Some(name) = env.kind.send_chunk_name() {
                    c.push_chunk(write_envelope_named(env, name));
                }
            }
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

/// `REC armed input monitor mode monitor-items preserve-pdc ? flags`, from the stored line.
fn rec_line(t: &Track, mut toks: Vec<String>) -> Vec<String> {
    toks.resize(toks.len().max(9), "0".into());
    toks[1] = bit(t.armed);
    toks[2] = crate::midi_route::rec_input_to_rpp(t.rec_input).to_string();
    // A stored value REAPER does not document (3) stays while it still means "on".
    let stored = num(&toks, 3, 1.0) as i64;
    let monitor = match (t.monitor, t.monitor_auto) {
        (false, _) => 0,
        (true, true) => 2,
        (true, false) if stored != 0 && stored != 2 => stored,
        (true, false) => 1,
    };
    toks[3] = monitor.to_string();
    toks[4] = t.rec_mode.code().to_string();
    toks[5] = bit(t.monitor_items);
    toks[8] = ((num(&toks, 8, 0.0) as i64 & !3) | t.rec_tap.code()).to_string();
    toks
}

fn write_lane_solo(c: &mut Chunk, t: &Track) {
    if let Some(words) = &t.lane_solo {
        c.line(std::iter::once("LANESOLO".to_owned()).chain(words.iter().map(u32::to_string)));
    }
}

fn write_comp_areas(c: &mut Chunk, t: &Track) {
    for a in &t.comp_areas {
        c.line([
            "LINKEDLANE".to_owned(),
            fmt_num(a.start),
            fmt_num(a.end),
            a.lane.to_string(),
            a.extra[0].clone(),
            a.extra[1].clone(),
            fmt_num(a.fade_in),
            fmt_num(a.fade_out),
        ]);
    }
}

fn midi_out_code(out: Option<(u8, u8)>) -> i64 {
    out.map_or(-1, |(dev, ch)| (dev as i64) << 5 | ch as i64)
}

fn write_fx_chain(name: &str, t: &Track) -> Chunk {
    let mut c = Chunk::new(name, &[]);
    c.children.extend(t.fx_chain_prelude.iter().cloned());
    c.children.extend(crate::fx_chain::join_chain(&t.fx));
    c
}

pub(crate) fn write_item(it: &Item) -> Chunk {
    let mut c = Chunk::new("ITEM", &[]);
    let l = &it.lines;
    c.line(line_from(l, "POSITION", &["POSITION"], &[(1, fmt_num(it.position))]));
    c.line(line_from(l, "LENGTH", &["LENGTH"], &[(1, fmt_num(it.length))]));
    c.line(line_from(l, "LOOP", &["LOOP"], &[(1, bit(it.looped))]));
    c.line(fade_line(l, "FADEIN", &it.fade_in));
    c.line(fade_line(l, "FADEOUT", &it.fade_out));
    c.line(line_from(l, "MUTE", &["MUTE", "0", "0"], &[(1, bit(it.muted))]));
    c.line(["IGUID", &it.guid]);
    let mut volpan = vec![(1, fmt_num(it.volume))];
    let had_take_fields = l.get("VOLPAN").is_none_or(|t| t.len() > 3);
    if let Some(t) = it
        .takes
        .first()
        .filter(|t| !t.lines.contains_key("TAKEVOLPAN"))
        .filter(|t| had_take_fields || t.pan != 0.0 || t.volume != 1.0)
    {
        volpan.extend([(2, fmt_num(t.pan)), (3, fmt_num(t.volume))]);
    }
    c.line(line_from(l, "VOLPAN", &["VOLPAN", "1", "0", "1", "-1"], &volpan));
    if let Some((top, height)) = it.ypos {
        c.line(line_from(l, "YPOS", &["YPOS", "0", "1", "2"], &[(1, fmt_num(top)), (2, fmt_num(height))]));
    }
    let mut wrote_beat = false;
    for node in &it.extra {
        match node {
            Node::Line(l) if line_key(l) == "BEAT" => {
                c.line(["BEAT".to_owned(), it.timebase.map_or(-1, |b| b.rpp_code()).to_string()]);
                wrote_beat = true;
            }
            _ => c.children.push(node.clone()),
        }
    }
    if let (false, Some(b)) = (wrote_beat, it.timebase) {
        c.line(["BEAT".to_owned(), b.rpp_code().to_string()]);
    }

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
        // The first take keeps its pan and volume in the item's VOLPAN unless it had its own line.
        if (i > 0 && (tk.volume != 1.0 || tk.pan != 0.0)) || l.contains_key("TAKEVOLPAN") {
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
            &[
                (1, fmt_num(tk.playrate)),
                (2, (tk.preserve_pitch as u8).to_string()),
                (3, fmt_num(tk.pitch)),
                (4, tk.pitch_mode.to_string()),
            ],
        ));
        write_take_extra(&mut c, tk);
        c.push_chunk(write_source(&tk.source, l));
        for env in &tk.envelopes {
            if let Some(name) = env.kind.take_chunk_name() {
                c.push_chunk(write_envelope_named(env, name));
            }
        }
    }
    c
}

/// The take's other lines, with `CHANMODE` and `SM` rewritten where they were and added after
/// `PLAYRATE` and at the end when new.
fn write_take_extra(c: &mut Chunk, tk: &Take) {
    let chanmode = || vec!["CHANMODE".to_owned(), tk.channel_mode.code().to_string()];
    let had = |key: &str| tk.extra.iter().any(|n| matches!(n, Node::Line(l) if line_key(l) == key));
    if !had("CHANMODE") && tk.channel_mode != ChannelMode::Normal {
        c.line(chanmode());
    }
    for node in &tk.extra {
        match node {
            Node::Line(l) if line_key(l) == "CHANMODE" => c.line(chanmode()),
            Node::Line(l) if line_key(l) == "SM" => {
                if !tk.stretch_markers.is_empty() {
                    c.line(crate::stretch::write_markers(&tk.stretch_markers, fmt_num));
                }
            }
            _ => c.children.push(node.clone()),
        }
    }
    if !had("SM") && !tk.stretch_markers.is_empty() {
        c.line(crate::stretch::write_markers(&tk.stretch_markers, fmt_num));
    }
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

/// The body of a `.RfxChain` file: the track's FX chain without its enclosing chunk.
pub fn fx_chain_to_text(t: &Track) -> String {
    super::write_body(&write_fx_chain("FXCHAIN", t).children)
}

/// The effects of a `.RfxChain` file, ready to append to a track's chain.
pub fn fx_chain_from_text(text: &str) -> Result<Vec<FxSlot>, ParseError> {
    let chunk = parse(&format!("<FXCHAIN\n{text}\n>\n"))?;
    Ok(parse_fx_chain(&chunk).0)
}

/// A `.RTrackTemplate` of `ids`, in project order, without their items. Sends between the
/// chosen tracks are kept; sends to other tracks are left out.
pub fn track_template(p: &Project, ids: &[TrackId]) -> String {
    let mut sub =
        Project { tracks: p.tracks.iter().filter(|t| ids.contains(&t.id)).cloned().collect(), ..Project::default() };
    let kept: Vec<TrackId> = sub.tracks.iter().map(|t| t.id).collect();
    for t in &mut sub.tracks {
        t.items.clear();
        t.sends.retain(|s| kept.contains(&s.dest));
    }
    sub.tracks.iter().map(|t| super::write(&write_track(&sub, t))).collect()
}

/// The tracks of a `.RTrackTemplate`, with fresh identities and the sends between them.
pub fn tracks_from_template(text: &str) -> Result<Vec<Track>, ParseError> {
    let root = parse(&format!("<TEMPLATE\n{text}\n>\n"))?;
    let mut tracks = Vec::new();
    let mut receives = Vec::new();
    for node in &root.children {
        if let Node::Chunk(c) = node
            && c.name() == "TRACK"
        {
            let (mut track, recvs) = parse_track(c);
            track.guid = new_guid();
            receives.extend(recvs.into_iter().map(|(src, send)| (src, tracks.len(), send)));
            tracks.push(track);
        }
    }
    for (src, dest, mut send) in receives {
        if let (Some(dest_id), true) = (tracks.get(dest).map(|t: &Track| t.id), src < tracks.len()) {
            send.dest = dest_id;
            tracks[src].sends.push(send);
        }
    }
    Ok(tracks)
}

/// `GetTrackStateChunk`: the track as the project file has it, its receives as `AUXRECV`
/// lines numbering the project's tracks.
pub fn track_state_chunk(p: &Project, id: TrackId) -> Option<String> {
    let t = if p.master.id == id { &p.master } else { p.track(id)? };
    Some(unindented(&write(&write_track(p, t))))
}

/// A chunk as REAPER's state chunk functions give it: every line flush left.
fn unindented(text: &str) -> String {
    text.lines().map(|l| format!("{}\n", l.trim_start())).collect()
}

/// `SetTrackStateChunk`: replaces the track (keeping its identity) with a `<TRACK` chunk,
/// receives included. False when the text is not a track.
pub fn set_track_state_chunk(p: &mut Project, id: TrackId, text: &str) -> bool {
    let Ok(c) = parse(text.trim()) else { return false };
    if c.name() != "TRACK" || p.track(id).is_none() {
        return false;
    }
    let (mut track, receives) = parse_track(&c);
    track.id = id;
    track.sends = p.track(id).map(|t| t.sends.clone()).unwrap_or_default();
    for t in &mut p.tracks {
        t.sends.retain(|s| s.dest != id);
    }
    for (src, mut send) in receives {
        if let Some(t) = p.tracks.get_mut(src).filter(|t| t.id != id) {
            send.dest = id;
            t.sends.push(send);
        }
    }
    if let Some(slot) = p.track_mut(id) {
        *slot = track;
    }
    true
}

/// `GetItemStateChunk`: the item as the project file has it.
pub fn item_state_chunk(item: &Item) -> String {
    unindented(&write(&write_item(item)))
}

/// `SetItemStateChunk`: the item an `<ITEM` chunk describes, with `id` as its identity.
pub fn item_from_state_chunk(text: &str, id: crate::ItemId) -> Option<Item> {
    let c = parse(text.trim()).ok()?;
    if c.name() != "ITEM" {
        return None;
    }
    let mut item = parse_item(&c);
    item.id = id;
    Some(item)
}

/// `GetEnvelopeStateChunk`: the envelope's chunk (`<VOLENV2`, `<PANENV2`…).
pub fn envelope_state_chunk(env: &Envelope) -> String {
    unindented(&write(&write_envelope(env)))
}

/// `SetEnvelopeStateChunk`: the envelope a chunk describes, keeping `kind`.
pub fn envelope_from_state_chunk(kind: EnvelopeKind, text: &str) -> Option<Envelope> {
    let c = parse(text.trim()).ok()?;
    Some(parse_envelope(kind, &c))
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

    pub(super) const SAMPLE: &str = r#"<REAPER_PROJECT 0.1 "7.73/linux-x86_64" 1762500000
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

    // Lines as REAPER 7.73 saved them after setting the fields through ReaScript.
    #[test]
    fn track_mix_fields_round_trip() {
        let rpp = "<REAPER_PROJECT 0.1 \"7.73/linux-x86_64\" 1
  PANMODE 5
  GROUP_NAME 0 Drums
  GROUP_NAME 1 \"Vox VCA\"
  <TRACK
    PANMODE 6
    VOLPAN 1 0 -1 -0.7 0.4
    MUTESOLO 0 1 1
    IPHASE 1
    GROUP_FLAGS 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1
    GROUP_FLAGS_HIGH 0 0 0 0 128
  >
  <TRACK
    WIDTH 0.3
    MUTESOLO 0 2 0
    GROUP_FLAGS 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 2 2
  >
>
";
        let p = project_from_rpp(rpp).unwrap();
        assert_eq!(p.pan_mode, PanMode::Stereo);
        assert_eq!(p.group_name(1), Some("Vox VCA"));
        let (a, b) = (&p.tracks[0], &p.tracks[1]);
        assert_eq!((a.pan_mode, a.dual_pan, a.phase), (Some(PanMode::Dual), [-0.7, 0.4], true));
        assert!(a.solo && a.solo_mode == SoloMode::IgnoreRouting && a.solo_defeat);
        assert!(a.groups.has(crate::groups::GroupParam::PanReverse, 0));
        assert!(a.groups.has(crate::groups::GroupParam::MuteLead, 39));
        assert_eq!((b.pan_mode, b.width, b.solo_mode), (None, 0.3, SoloMode::InPlace));
        assert!(b.groups.has(crate::groups::GroupParam::VcaFollowPreFx, 1));
        let text = project_to_rpp(&p);
        for line in [
            "PANMODE 5",
            "GROUP_NAME 0 Drums",
            "GROUP_NAME 1 \"Vox VCA\"",
            "PANMODE 6",
            "VOLPAN 1 0 -1 -0.7 0.4",
            "MUTESOLO 0 1 1",
            "IPHASE 1",
            "GROUP_FLAGS 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1",
            "GROUP_FLAGS_HIGH 0 0 0 0 128",
            "WIDTH 0.3",
            "MUTESOLO 0 2 0",
        ] {
            assert!(text.contains(line), "{line} missing from\n{text}");
        }
        let mut p2 = project_from_rpp(&text).unwrap();
        assert_eq!(project_to_rpp(&p2).lines().skip(1).collect::<Vec<_>>(), text.lines().skip(1).collect::<Vec<_>>());
        p2.tracks[0].groups = crate::groups::GroupFlags::default();
        p2.tracks[0].phase = false;
        p2.tracks[1].pan_mode = Some(PanMode::Classic);
        let text = project_to_rpp(&p2);
        assert!(!text.contains("GROUP_FLAGS 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1") && !text.contains("GROUP_FLAGS_HIGH"));
        assert!(text.contains("IPHASE 0") && text.contains("PANMODE 0"));
    }

    #[test]
    fn edits_survive_round_trip() {
        let mut p = project_from_rpp(super::tests::SAMPLE).unwrap();
        p.tracks[0].fx[1].enabled = true;
        p.tracks[0].armed = false;
        p.tracks[0].items[0].position = 70.0;
        let p2 = project_from_rpp(&project_to_rpp(&p)).unwrap();
        assert!(p2.tracks[0].fx[1].enabled);
        assert!(!p2.tracks[0].armed);
        assert_eq!(p2.tracks[0].items[0].position, 70.0);
    }

    #[test]
    fn tempo_points_made_here_are_written() {
        let mut p = project_from_rpp(super::tests::SAMPLE).unwrap();
        p.tempo.insert_point(8.0);
        p.tempo.points.push(TempoPoint { time_sig: Some((7, 8)), ..TempoPoint::new(16.0, 140.0) });
        let text = project_to_rpp(&p);
        assert!(text.contains("PT 8 168 1"), "{text}");
        let p2 = project_from_rpp(&text).unwrap();
        assert_eq!(p2.tempo.points.len(), 3);
        assert_eq!(p2.tempo.points[2].time_sig, Some((7, 8)));
        assert_eq!(p2.tempo.points[2].bpm, 140.0);
    }

    /// MIDI inputs, the MIDI fields of sends and the timecode sync line as REAPER 7.73 saves
    /// them, read and written back unchanged.
    #[test]
    fn midi_routing_round_trips() {
        let text = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  SMPTESYNC 1 25 100 40 1000 300 4192 0.5 1 0 0\n  <TRACK\n    REC 1 5088 1 0 0 0 0 0\n  >\n  <TRACK\n    REC 1 5155 1 0 0 0 0 0\n    AUXRECV 0 0 1 0 0 0 0 -1 0 -1:U 12583009 -1 ''\n  >\n>\n";
        let mut p = project_from_rpp(text).unwrap();
        assert_eq!(p.tracks[0].rec_input, 4096 + (63 << 5), "31 in the file is all inputs");
        assert_eq!(p.tracks[1].rec_input, 4096 + (31 << 5) + 3, "33 in the file is device 31");
        let send = &p.tracks[0].sends[0];
        let midi = send.midi();
        assert_eq!(
            (send.src_channels, midi.src_channel, midi.dst_channel, midi.dst_bus),
            (-1, Some(0), Some(2), Some(2))
        );
        assert!(p.timecode_sync.enabled());
        assert_eq!((p.timecode_sync.input, p.timecode_sync.offset), (4192, 0.5));
        let out = project_to_rpp(&p);
        for line in [
            "SMPTESYNC 1 25 100 40 1000 300 4192 0.5 1 0 0",
            "REC 1 5088 1",
            "REC 1 5155 1",
            "AUXRECV 0 0 1 0 0 0 0 -1 0 -1:U 12583009 -1",
        ] {
            assert!(out.contains(line), "{line}\n{out}");
        }
        p.tracks[0].rec_input = 4096 + (62 << 5) + 1;
        p.tracks[0].sends[0].set_midi(crate::midi_route::SendMidi::ALL);
        p.timecode_sync.set_enabled(false);
        let out = project_to_rpp(&p);
        assert!(out.contains("REC 1 5057 1") && out.contains("-1:U 0 -1") && out.contains("SMPTESYNC 0 25"), "{out}");
    }

    #[test]
    fn midi_output_uses_reapers_midiout_line() {
        // As REAPER 7.73 saves I_MIDIHWOUT 37 (device 1, channel 5).
        let text = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  <TRACK\n    MIDIOUT 37\n  >\n  <TRACK\n    MIDIOUT -1\n  >\n>\n";
        let mut p = project_from_rpp(text).unwrap();
        assert_eq!((p.tracks[0].midi_out, p.tracks[1].midi_out), (Some((1, 5)), None));
        p.tracks[1].midi_out = Some((0, 16));
        let out = project_to_rpp(&p);
        assert!(out.contains("MIDIOUT 37\n") && out.contains("MIDIOUT 16\n") && !out.contains("MIDIHWOUT"), "{out}");
    }

    #[test]
    fn autosave_names() {
        let p = autosave_path(Path::new("/songs/Song.RPP"), 1_762_552_871);
        assert_eq!(p, PathBuf::from("/songs/Backups/Song-2025-11-07_220111.rpp-bak"));
    }

    #[test]
    fn old_backups_are_pruned() {
        let dir = std::env::temp_dir().join(format!("scythe-prune-{}", std::process::id()));
        let song = dir.join("Song.RPP");
        std::fs::create_dir_all(dir.join("Backups")).unwrap();
        let other = dir.join("Backups").join("Song B-2025-01-01_000000.rpp-bak");
        std::fs::write(&other, "").unwrap();
        let made: Vec<PathBuf> = (0..5).map(|i| autosave_path(&song, 1_762_552_871 + i * 300)).collect();
        for p in &made {
            std::fs::write(p, "").unwrap();
        }
        prune_backups(&song, 2);
        let left: Vec<bool> = made.iter().map(|p| p.exists()).collect();
        assert_eq!(left, [false, false, false, true, true]);
        assert!(other.exists(), "another project's backups stay");
        std::fs::remove_dir_all(&dir).unwrap();
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

    #[test]
    fn send_and_take_envelopes_round_trip_in_place() {
        let text = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  <TRACK\n    NAME src\n    <ITEM\n      POSITION 0\n      LENGTH 2\n      NAME a\n      <SOURCE WAVE\n        FILE \"a.wav\"\n      >\n      <VOLENV\n        ACT 1 -1\n        PT 0 0 0\n        PT 2 1 0\n      >\n    >\n  >\n  <TRACK\n    NAME bus\n    AUXRECV 0 0 1 0 0 0 0 0 0 -1:U 4177951 -1 ''\n    <AUXPANENV\n      ACT 1 -1\n      PT 0 1 0\n      PT 2 -1 0\n    >\n  >\n>\n";
        let p = project_from_rpp(text).unwrap();
        let send = &p.tracks[0].sends[0];
        assert_eq!(send.envelopes.len(), 1);
        assert_eq!((send.envelopes[0].kind, send.envelopes[0].points.len()), (EnvelopeKind::Pan, 2));
        let take = &p.tracks[0].items[0].takes[0];
        assert_eq!((take.envelopes[0].kind, take.envelopes[0].value_at(1.0)), (EnvelopeKind::Volume, Some(0.5)));
        assert!(p.tracks[1].extra.iter().all(|n| !matches!(n, Node::Chunk(c) if c.name() == "AUXPANENV")));

        let out = project_to_rpp(&p);
        let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("{needle} missing:\n{out}"));
        assert!(at("AUXRECV") < at("<AUXPANENV"), "{out}");
        assert!(at("<SOURCE WAVE") < at("<VOLENV"), "{out}");
        let again = project_from_rpp(&out).unwrap();
        assert_eq!(again.tracks[0].sends[0].envelopes[0].points, send.envelopes[0].points);
        assert_eq!(again.tracks[0].items[0].takes[0].envelopes[0].points, take.envelopes[0].points);
    }

    #[test]
    fn fx_chains_and_track_templates() {
        let mut p = project_from_rpp(super::tests::SAMPLE).unwrap();
        let with_fx = p.tracks.iter().position(|t| !t.fx.is_empty()).expect("sample has FX");
        let text = fx_chain_to_text(&p.tracks[with_fx]);
        assert!(!text.starts_with("<FXCHAIN"));
        let slots = fx_chain_from_text(&text).unwrap();
        assert_eq!(slots.len(), p.tracks[with_fx].fx.len());
        assert_eq!(slots[0].raw, p.tracks[with_fx].fx[0].raw);

        let (a, b) = (p.tracks[0].id, p.tracks[1].id);
        p.tracks[0].sends = vec![Send::new(b)];
        let template = track_template(&p, &[a, b]);
        let tracks = tracks_from_template(&template).unwrap();
        assert_eq!(tracks.len(), 2);
        assert!(tracks.iter().all(|t| t.items.is_empty()));
        assert_eq!(tracks[0].name, p.tracks[0].name);
        assert_eq!(tracks[0].sends.len(), 1);
        assert_eq!(tracks[0].sends[0].dest, tracks[1].id);
        assert_ne!(tracks[0].guid, p.tracks[0].guid);
    }

    /// The record settings and comp lanes as REAPER 7.73 saves them (Lua probes).
    #[test]
    fn record_modes_and_comp_lanes_round_trip() {
        let text = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  RECMODE 0\n  <TRACK\n    FREEMODE 2\n    FIXEDLANES 9 0 0 0 0\n    LANESOLO 1 0 0 0 0 0 0 0\n    LANEREC -1 0 -1 -1\n    LANENAME C1 1 2 3\n    REC 1 1024 2 6 1 0 0 2\n    AUTO_RECARM 1\n    ITEMLANES 4\n    LINKEDLANE 1.5 2.01 1 0 -1 0.01 0.01\n    LINKEDLANE 2 2.75 2 0 -1 0.01 0.01\n  >\n>\n";
        let p = project_from_rpp(text).unwrap();
        let t = &p.tracks[0];
        assert!(t.armed && t.monitor && t.monitor_auto && t.monitor_items && t.auto_arm);
        assert_eq!(t.rec_mode, RecordMode::Output { format: crate::recording::OutputFormat::Mono, compensated: true });
        assert_eq!(t.rec_tap, RecordTap::PostFxPreFader);
        assert_eq!(t.lane_names, ["C1", "1", "2", "3"]);
        assert_eq!((t.comp_lane(), t.comp_areas.len(), t.comp_areas[1].lane), (Some(0), 2, 2));
        assert_eq!(crate::recording::ProjectRecordMode::of(&p), crate::recording::ProjectRecordMode::ItemPunch);
        let out = project_to_rpp(&p);
        for line in [
            "REC 1 1024 2 6 1 0 0 2",
            "AUTO_RECARM 1",
            "LANEREC -1 0 -1 -1",
            "LANENAME C1 1 2 3",
            "LINKEDLANE 1.5 2.01 1 0 -1 0.01 0.01",
            "LINKEDLANE 2 2.75 2 0 -1 0.01 0.01",
            "LANESOLO 1 0 0 0 0 0 0 0",
            "FREEMODE 2",
        ] {
            assert!(out.contains(line), "{line} missing in\n{out}");
        }
        let mut p = p;
        let t = &mut p.tracks[0];
        (t.monitor_auto, t.rec_mode, t.rec_tap, t.auto_arm) =
            (false, RecordMode::MidiLatchReplace, RecordTap::PreFx, false);
        let out = project_to_rpp(&p);
        assert!(out.contains("REC 1 1024 1 16 1 0 0 1") && !out.contains("AUTO_RECARM"), "{out}");
        // A track that is not armed but records its output keeps a REC line.
        let mut t = Track::new("o");
        t.rec_mode = RecordMode::Output { format: crate::recording::OutputFormat::Stereo, compensated: false };
        p.tracks.push(t);
        assert!(project_to_rpp(&p).contains("REC 0 0 1 1 0 0 0 0"));
    }

    #[test]
    fn lane_soloing() {
        let p = project_from_rpp(
            "<REAPER_PROJECT 0.1 \"7.73\" 0\n  <TRACK\n    FREEMODE 2\n    FIXEDLANES 9 0 0 0 0\n    LANESOLO 2 0 0 0 0 0 0 0\n    <ITEM\n      POSITION 0\n      LENGTH 1\n      YPOS 0 0.5 2\n    >\n    <ITEM\n      POSITION 0\n      LENGTH 1\n      YPOS 0.5 0.5 2\n    >\n  >\n>\n",
        )
        .unwrap();
        let t = &p.tracks[0];
        assert_eq!(t.items.iter().map(|i| i.lane()).collect::<Vec<_>>(), [0, 1]);
        assert!(t.fixed_lanes && !t.lane_plays(0) && t.lane_plays(1));
        let free = Track { fixed_lanes: false, ..t.clone() };
        assert!(free.lane_plays(0), "without fixed lanes every item plays");
        assert!(project_to_rpp(&p).contains("LANESOLO 2 0 0 0 0 0 0 0"));
    }

    #[test]
    fn first_take_volume_lives_in_volpan() {
        let p = project_from_rpp("<REAPER_PROJECT 0.1 \"7.73\" 0\n  <TRACK\n    <ITEM\n      POSITION 0\n      LENGTH 1\n      VOLPAN 0.8 -0.5 0.1 -1\n    >\n  >\n>\n").unwrap();
        let it = &p.tracks[0].items[0];
        assert_eq!((it.volume, it.takes[0].pan, it.takes[0].volume), (0.8, -0.5, 0.1));
        let out = project_to_rpp(&p);
        assert!(out.contains("VOLPAN 0.8 -0.5 0.1 -1") && !out.contains("TAKEVOLPAN"), "{out}");
    }

    #[test]
    fn pitch_modes() {
        let p = project_from_rpp("<REAPER_PROJECT 0.1 \"7.73\" 0\n  DEFPITCHMODE 851968 0\n  <TRACK\n    <ITEM\n      POSITION 0\n      LENGTH 1\n      PLAYRATE 0.75 1 3 5 0 0.0025\n    >\n    <ITEM\n      POSITION 2\n      LENGTH 1\n    >\n  >\n>\n").unwrap();
        assert_eq!(p.default_pitch_mode, 13 << 16);
        let (a, b) = (&p.tracks[0].items[0].takes[0], &p.tracks[0].items[1].takes[0]);
        assert_eq!((a.pitch_mode, b.pitch_mode), (5, -1));
        let out = project_to_rpp(&p);
        assert!(out.contains("PLAYRATE 0.75 1 3 5 0 0.0025") && out.contains("DEFPITCHMODE 851968 0"), "{out}");
        let plain = project_from_rpp("<REAPER_PROJECT 0.1 \"7.73\" 0\n>\n").unwrap();
        assert_eq!(plain.default_pitch_mode, Processing::DEFAULT_MODE);
    }

    /// Lines as REAPER 7.73 saves them for hardware outputs set up through
    /// `SetTrackSendInfo_Value(tr, 1, …)`.
    const HW_PROJECT: &str = "<REAPER_PROJECT 0.1 \"7.73\" 0
  MASTERHWOUT 0 0 1 0 0 0 0 -1
  MASTERHWOUT 6 3 0.25 0.5 1 1 1024 0.5
  MASTER_NCH 2 2
  <TRACK
    NAME a
    MAINSEND 0 0
    HWOUT 1026 3 0.5 -0.25 0 0 0 -1:U -1
    HWOUT 4 1 1 0 1 1 1025 -1:U -1
    HWOUT 2 0 1 0 0 0 2048 0.5:U 2
    HWOUT_WITHOUT_PANLAW_BOOST
    HWOUT 0 0 1 0 0 0 -1 -1:U -1
  >
  <TRACK
    NAME b
    AUXRECV 0 3 0.7 -0.3 1 1 1 0 0 -1:U 0 -1 ''
    MAINSEND 1 0
  >
>
";

    #[test]
    fn hardware_outputs_round_trip() {
        let mut p = project_from_rpp(HW_PROJECT).unwrap();
        let hw = &p.tracks[0].hw_outs;
        assert_eq!(hw.len(), 4);
        assert_eq!(
            (hw[0].first_output(), hw[0].is_mono(), hw[0].mode, hw[0].volume, hw[0].pan),
            (2, true, SendMode::PostFx, 0.5, -0.25)
        );
        assert_eq!((hw[1].mode, hw[1].muted, hw[1].phase, hw[1].src_channels), (SendMode::PreFx, true, true, 1025));
        assert_eq!((hw[2].outputs(), hw[2].pan_law, hw[2].trailing.len()), (2..6, 0.5, 1));
        assert_eq!(hw[3].outputs(), 0..0, "no source channels");
        assert_eq!(hw[0].label(), "Out 3 (mono)");
        let m = &p.master.hw_outs;
        assert_eq!(m.len(), 2);
        assert_eq!(
            (m[1].first_output(), m[1].mode, m[1].volume, m[1].pan, m[1].muted, m[1].phase, m[1].pan_law),
            (6, SendMode::PostFx, 0.25, 0.5, true, true, 0.5)
        );
        assert!(!p.master_default_output());
        let send = &p.tracks[0].sends[0];
        assert_eq!((send.mode, send.muted, send.mono, send.phase), (SendMode::PostFx, true, true, true));

        let out = project_to_rpp(&p);
        for line in HW_PROJECT.lines().filter(|l| l.contains("HWOUT") || l.contains("AUXRECV")) {
            assert_eq!(out.matches(&format!("{}\n", line.trim().replace('\'', "\""))).count(), 1, "{line}\n{out}");
        }
        let at = |key: &str| out.find(key).unwrap();
        assert!(
            at("MAINSEND 0 0") < at("HWOUT 1026")
                && at("HWOUT 2 0") < at("HWOUT_WITHOUT")
                && at("HWOUT_WITHOUT") < at("    HWOUT 0 0")
        );

        let t = &mut p.tracks[0];
        t.hw_outs[0].volume = 1.0;
        t.hw_outs[0].mode = SendMode::PostFader;
        t.hw_outs.remove(3);
        t.sends[0].mode = SendMode::PreFx;
        p.tracks[1].hw_outs.push(HwOut::stereo(4));
        p.master.hw_outs.clear();
        let out = project_to_rpp(&p);
        assert!(out.contains("HWOUT 1026 0 1 -0.25 0 0 0 -1:U -1\n") && !out.contains("HWOUT 0 0 1 0 0 0 -1"), "{out}");
        assert!(out.contains("MAINSEND 1 0\n    HWOUT 4 0 1 0 0 0 0 -1:U -1\n"), "{out}");
        assert!(out.contains("AUXRECV 0 1 0.7"), "{out}");
        assert!(out.contains("  MASTERHWOUT\n") && !out.contains("MASTERHWOUT 0"), "{out}");
        assert!(project_from_rpp(&out).unwrap().master.hw_outs.is_empty());
    }

    #[test]
    fn master_output_defaults() {
        let p = project_from_rpp("<REAPER_PROJECT 0.1 \"7.73\" 0\n>\n").unwrap();
        assert!(p.master_default_output(), "REAPER gives a project without MASTERHWOUT the default pair");
        assert!(!project_to_rpp(&p).contains("MASTERHWOUT"));
        let mut p = project_from_rpp("<REAPER_PROJECT 0.1 \"7.73\" 0\n  MASTERHWOUT 0 0 1 0 0 0 0 -1\n>\n").unwrap();
        assert!(p.master_default_output());
        assert!(project_to_rpp(&p).contains("MASTERHWOUT 0 0 1 0 0 0 0 -1\n"));
        p.master.hw_outs[0].volume = 0.5;
        assert!(!p.master_default_output());
        let mut fresh = Project::default();
        fresh.master.hw_outs.push(HwOut::stereo(2));
        let out = project_to_rpp(&fresh);
        assert!(out.contains("  MASTERHWOUT 0 0 1 0 0 0 0 -1\n  MASTERHWOUT 2 0 1 0 0 0 0 -1\n"), "{out}");
    }

    #[test]
    fn metronome_settings_round_trip() {
        // As REAPER 7.73 saves count-in before playback and recording, 3.5 measures, and two outputs.
        let text = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  <METRONOME 31 3.5\n    VOL 0.25 0.125\n    BEATLEN 4\n    PATTERNSTR ABBB\n  >\n  METRONOMEOUT 2\n  METRONOMEOUT 1028\n  GLOBAL_AUTO -1\n>\n";
        let mut p = project_from_rpp(text).unwrap();
        let m = &p.metronome;
        assert_eq!((m.flags, m.count_in_measures, m.volume, &m.outputs[..]), (31, 3.5, 0.25, &[2, 1028][..]));
        assert!(m.enabled() && m.counts_in(false) && m.counts_in(true));
        assert!(
            project_to_rpp(&p).contains(&text[text.find("  <METRONOME").unwrap()..]),
            "the chunk and outputs are written as read"
        );
        p.metronome.flags = 6;
        p.metronome.volume = 0.5;
        p.metronome.outputs = vec![4];
        let out = project_to_rpp(&p);
        assert!(
            out.contains(
                "  <METRONOME 6 3.5\n    VOL 0.5 0.125\n    BEATLEN 4\n    PATTERNSTR ABBB\n  >\n  METRONOMEOUT 4\n"
            ),
            "{out}"
        );

        let plain = project_from_rpp("<REAPER_PROJECT 0.1 \"7.73\" 0\n>\n").unwrap();
        assert!(!project_to_rpp(&plain).contains("METRONOME"));
        let mut set = plain;
        set.metronome.set(crate::metronome::COUNT_IN_PLAY, true);
        set.metronome.outputs.push(1030);
        let out = project_to_rpp(&set);
        assert!(out.contains("  <METRONOME 14 2\n  >\n  METRONOMEOUT 1030\n"), "{out}");
        assert_eq!(project_from_rpp(&out).unwrap().metronome, set.metronome);
    }

    #[test]
    fn timing_lines_round_trip_and_follow_the_model() {
        let text = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  TIMELOCKMODE 2\n  TEMPOENVLOCKMODE 0\n  PLAYRATE 1.25 0 0.25 4\n  <TRACK\n    BEAT 0\n    <ITEM\n      POSITION 1\n      LENGTH 2\n      BEAT 1\n      NAME \"\"\n      SOFFS 0\n      PLAYRATE 1 1 0 -1 0 0.0025\n      CHANMODE 3\n      GUID {D69F1D2C-4A90-C62C-811C-E649EC1E09B8}\n      SM 0 -0.1 0 1 + 0.5 0.4 0.3 + 2 2.2 0 1\n      <SOURCE WAVE\n        FILE \"a.wav\"\n      >\n      <PITCHENV\n        ACT 1 -1\n        PT 0 0 0\n        PT 1.5 -2 0\n      >\n    >\n  >\n>\n";
        let p = project_from_rpp(text).unwrap();
        assert_eq!((p.timebase, p.tempo_follows_beats), (Timebase::BeatsPosition, false));
        assert_eq!(p.playrate, PlayRate { rate: 1.25, preserve_pitch: false });
        assert_eq!(p.tracks[0].timebase, Some(Timebase::Time));
        let it = &p.tracks[0].items[0];
        assert_eq!(it.timebase, Some(Timebase::Beats));
        let tk = &it.takes[0];
        assert_eq!(tk.channel_mode, ChannelMode::Mono(0));
        assert_eq!(tk.stretch_markers.len(), 3);
        assert_eq!(tk.stretch_markers[1].slope, 0.3);
        assert_eq!(tk.pitch_envelope().map(|e| e.points.len()), Some(2));
        let out = project_to_rpp(&p);
        for line in [
            "TIMELOCKMODE 2",
            "TEMPOENVLOCKMODE 0",
            "PLAYRATE 1.25 0 0.25 4",
            "    BEAT 0\n",
            "      BEAT 1\n",
            "CHANMODE 3\n      GUID",
            "SM 0 -0.1 0 1 + 0.5 0.4 0.3 + 2 2.2 0 1\n      <SOURCE",
            "<PITCHENV",
            "PT 1.5 -2 0",
        ] {
            assert!(out.contains(line), "{line:?} in {out}");
        }
        assert_eq!(project_from_rpp(&out).unwrap().tracks[0].items[0].takes[0], *tk);

        let mut q = p.clone();
        q.timebase = Timebase::Time;
        q.playrate.rate = 2.0;
        q.tracks[0].items[0].timebase = None;
        let tk = &mut q.tracks[0].items[0].takes[0];
        tk.channel_mode = ChannelMode::Stereo(2);
        tk.stretch_markers.clear();
        let out = project_to_rpp(&q);
        assert!(out.contains("TIMELOCKMODE 0") && out.contains("PLAYRATE 2 0 0.25 4"), "{out}");
        assert!(out.contains("      BEAT -1\n") && out.contains("CHANMODE 69") && !out.contains("SM "), "{out}");

        let fresh = project_from_rpp(
            "<REAPER_PROJECT 0.1 \"7.73\" 0\n  <TRACK\n    <ITEM\n      POSITION 0\n      LENGTH 1\n    >\n  >\n>\n",
        )
        .unwrap();
        assert_eq!(
            (fresh.timebase, fresh.tempo_follows_beats, fresh.playrate),
            (Timebase::Beats, true, PlayRate::default())
        );
        let mut fresh = fresh;
        fresh.tracks[0].items[0].takes[0].channel_mode = ChannelMode::ReverseStereo;
        fresh.tracks[0].items[0].takes[0].stretch_markers.push(crate::stretch::StretchMarker::new(0.5, 0.25));
        let out = project_to_rpp(&fresh);
        assert!(out.contains("CHANMODE 1") && out.contains("SM 0.5 0.25"), "{out}");
        assert!(!out.contains("TIMELOCKMODE") && !out.contains("PLAYRATE 1 0"), "{out}");
    }
}
