//! Editing behaviour REAPER keeps per project: ripple editing, the snap grid, locking, item
//! grouping and auto-crossfades, with the edit operations that follow them ([`ops`]), the
//! editing actions by REAPER command id ([`actions`]), the nudge/set dialog ([`nudge`]) and
//! transient detection for dynamic split and tab-to-transient ([`transients`]).
//!
//! Measured in REAPER 7.73 with Lua probes (kept in the README's verification table): the
//! RPP lines are `RIPPLE <mode> <all>`, `GRID <flags> 8 <quarter notes> 8 1 0 <kind> <swing>`,
//! `LOCK <flags>`, `AUTOXFADE <flags>` and `GROUPOVERRIDE <off> 0 0 0`.

pub mod actions;
pub mod nudge;
pub mod ops;
pub mod transients;

use crate::TempoMap;

/// What ripple editing moves along with an edit (`RIPPLE`, actions 40309 to 40311).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RippleMode {
    #[default]
    Off,
    /// Later items and envelope points on the edited tracks.
    PerTrack,
    /// Later items, envelope points, markers and regions on every track.
    AllTracks,
}

impl RippleMode {
    pub fn from_code(code: i64) -> Self {
        match code {
            1 => Self::PerTrack,
            2 => Self::AllTracks,
            _ => Self::Off,
        }
    }

    pub fn code(self) -> i64 {
        match self {
            Self::Off => 0,
            Self::PerTrack => 1,
            Self::AllTracks => 2,
        }
    }

    /// The next mode of "Options: Cycle ripple editing mode".
    pub fn cycled(self) -> Self {
        match self {
            Self::Off => Self::PerTrack,
            Self::PerTrack => Self::AllTracks,
            Self::AllTracks => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Ripple off",
            Self::PerTrack => "Ripple per-track",
            Self::AllTracks => "Ripple all tracks",
        }
    }
}

/// How grid lines are spaced (`GRID` field 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GridKind {
    /// Every `qn` quarter notes.
    #[default]
    Normal,
    /// Every `qn` quarter notes, every second line pushed later by the swing amount.
    Swing,
    /// At bar lines.
    Measure,
    /// At video frames; kept from the file, snapped like `Normal`.
    Frames,
}

const GRID_SWING: i64 = 8;
const GRID_MEASURE: i64 = 64;
const GRID_FRAMES: i64 = 1;
/// `GRID` field 1: grid lines hidden when clear.
const GRID_LINES: i64 = 1;
/// `GRID` field 1: snapping off when set.
const GRID_SNAP_OFF: i64 = 256;

/// The project's snap grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    /// Grid spacing in quarter notes; `GetSetProjectGrid` gives a quarter of this, in whole
    /// notes.
    pub qn: f64,
    pub kind: GridKind,
    /// Swing amount, -1 to 1: how far every second line moves towards the next one, in
    /// half-steps.
    pub swing: f64,
    /// Snapping is on ("Options: Toggle snapping").
    pub snap: bool,
    /// Grid lines are drawn ("Options: Toggle grid lines").
    pub lines: bool,
}

impl Default for Grid {
    fn default() -> Self {
        Self { qn: 1.0, kind: GridKind::Normal, swing: 0.0, snap: true, lines: true }
    }
}

impl Grid {
    /// The nearest grid line to `t`; `t` itself while snapping is off.
    pub fn snap(&self, tempo: &TempoMap, t: f64) -> f64 {
        if self.snap { self.nearest_line(tempo, t) } else { t }
    }

    /// The nearest grid line to `t`, snapping on or not.
    pub fn nearest_line(&self, tempo: &TempoMap, t: f64) -> f64 {
        let tl = tempo.timeline();
        if self.kind == GridKind::Measure {
            let bar = tl.bar_beat(t.max(0.0)).bar;
            let (a, b) = (tl.bar_start(bar), tl.bar_start(bar + 1));
            return if t - a <= b - t { a } else { b };
        }
        let step = self.qn.max(1e-6);
        let q = tl.qn_at(t.max(0.0));
        let k = (q / step).floor();
        let mut best = f64::INFINITY;
        let mut out = t;
        for n in [k - 1.0, k, k + 1.0, k + 2.0] {
            if n < 0.0 {
                continue;
            }
            let line = tl.time_at_qn(self.line_qn(n));
            if (line - t).abs() < best {
                best = (line - t).abs();
                out = line;
            }
        }
        out
    }

    /// Quarter-note position of grid line `n`.
    fn line_qn(&self, n: f64) -> f64 {
        let step = self.qn.max(1e-6);
        let odd = (n as i64) % 2 == 1;
        if self.kind == GridKind::Swing && odd { (n + 0.5 * self.swing.clamp(-1.0, 1.0)) * step } else { n * step }
    }

    /// The grid lines in `[t0, t1]`, at most `max` of them.
    pub fn lines_between(&self, tempo: &TempoMap, t0: f64, t1: f64, max: usize) -> Vec<f64> {
        let tl = tempo.timeline();
        let mut out = Vec::new();
        if self.kind == GridKind::Measure {
            let mut bar = tl.bar_beat(t0.max(0.0)).bar;
            loop {
                let t = tl.bar_start(bar);
                if t > t1 || out.len() >= max {
                    break;
                }
                if t >= t0 {
                    out.push(t);
                }
                bar += 1;
            }
            return out;
        }
        let step = self.qn.max(1e-6);
        let mut n = (tl.qn_at(t0.max(0.0)) / step).floor() - 1.0;
        while out.len() < max {
            let t = tl.time_at_qn(self.line_qn(n.max(0.0)));
            if t > t1 {
                break;
            }
            if t >= t0 && n >= 0.0 {
                out.push(t);
            }
            n += 1.0;
        }
        out
    }

    /// The grid step in seconds at `t`, as "move by grid size" uses it.
    pub fn step_seconds(&self, tempo: &TempoMap, t: f64) -> f64 {
        let tl = tempo.timeline();
        if self.kind == GridKind::Measure {
            return tempo.bar_seconds_at(t.max(0.0));
        }
        let q = tl.qn_at(t.max(0.0));
        tl.time_at_qn(q + self.qn) - tl.time_at_qn(q)
    }

    /// `GetSetProjectGrid`'s division (whole notes) and swing mode (0 off, 1 swing, 3
    /// measure).
    pub fn api(&self) -> (f64, i64, f64) {
        let mode = match self.kind {
            GridKind::Swing => 1,
            GridKind::Measure => 3,
            GridKind::Normal | GridKind::Frames => 0,
        };
        (self.qn / 4.0, mode, self.swing)
    }

    /// Applies `GetSetProjectGrid`'s values; negative ones leave a field as it is.
    pub fn set_api(&mut self, division: f64, mode: i64, swing: f64) {
        if division > 0.0 {
            self.qn = division * 4.0;
        }
        match mode {
            0 => self.kind = GridKind::Normal,
            1 => self.kind = GridKind::Swing,
            3 => self.kind = GridKind::Measure,
            _ => {}
        }
        if (-1.0..=1.0).contains(&swing) && mode >= 0 {
            self.swing = swing;
        }
    }

    /// A short name: "1/16", "1/8T", "bar", with " swing" when swung.
    pub fn label(&self) -> String {
        if self.kind == GridKind::Measure {
            return "bar".into();
        }
        let whole = self.qn / 4.0;
        let base = if whole >= 1.0 && (whole - whole.round()).abs() < 1e-9 {
            format!("{}", whole.round())
        } else {
            let den = 1.0 / whole;
            let triplet = den * 2.0 / 3.0;
            if (den - den.round()).abs() < 1e-6 && den.round() as i64 % 3 != 0 {
                format!("1/{}", den.round())
            } else if (triplet - triplet.round()).abs() < 1e-6 {
                format!("1/{}T", triplet.round())
            } else if (den - den.round()).abs() < 1e-6 {
                format!("1/{}", den.round())
            } else {
                format!("{:.3} QN", self.qn)
            }
        };
        if self.kind == GridKind::Swing { format!("{base} swing") } else { base }
    }
}

/// `LOCK` bits: what locking covers while it is on.
pub const LOCK_TIME_SELECTION: u32 = 1;
pub const LOCK_ITEMS: u32 = 2;
pub const LOCK_ENVELOPES: u32 = 4;
pub const LOCK_MARKERS: u32 = 8;
pub const LOCK_REGIONS: u32 = 16;
pub const LOCK_TIME_SIGNATURES: u32 = 32;
pub const LOCK_ITEM_LEFT_RIGHT: u32 = 64;
pub const LOCK_ITEM_UP_DOWN: u32 = 128;
pub const LOCK_ITEM_EDGES: u32 = 256;
pub const LOCK_ITEM_FADES: u32 = 512;
pub const LOCK_LOOP_POINTS: u32 = 1024;
/// Locking is on ("Options: Toggle locking").
pub const LOCK_ENABLED: u32 = 16384;
/// The lock modes with their names, as REAPER's "Locking: …" actions name them, with the
/// command id of each "Toggle" action.
pub const LOCK_MODES: [(u32, &str, u32); 11] = [
    (LOCK_TIME_SELECTION, "time selection", 40573),
    (LOCK_ITEMS, "full item", 40576),
    (LOCK_ITEM_LEFT_RIGHT, "left/right item", 40579),
    (LOCK_ITEM_UP_DOWN, "up/down item", 40582),
    (LOCK_ENVELOPES, "track envelope", 40585),
    (LOCK_REGIONS, "region", 40588),
    (LOCK_MARKERS, "marker", 40591),
    (LOCK_TIME_SIGNATURES, "time signature marker", 40594),
    (LOCK_ITEM_EDGES, "item edges", 40597),
    (LOCK_ITEM_FADES, "item fade/volume handles", 40600),
    (LOCK_LOOP_POINTS, "loop points", 40629),
];
/// Every lock mode bit.
pub const LOCK_ALL: u32 = 2047;

/// `AUTOXFADE` bit: overlapping items crossfade automatically.
const AUTOXFADE_ON: i64 = 1;

/// The editing settings a project carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EditSettings {
    pub ripple: RippleMode,
    pub grid: Grid,
    /// `LOCK`: the `LOCK_*` modes and whether locking is on (`LOCK_ENABLED`).
    pub lock: u32,
    /// Items that come to overlap crossfade (`AUTOXFADE` bit 1, "Options: Auto-crossfade
    /// media items when editing").
    pub auto_crossfade: bool,
    /// Item groups act together (`GROUPOVERRIDE` field 1 clear).
    pub grouping: bool,
}

impl Default for EditSettings {
    /// What a new REAPER 7.73 project has.
    fn default() -> Self {
        Self {
            ripple: RippleMode::Off,
            grid: Grid::default(),
            lock: LOCK_TIME_SELECTION,
            auto_crossfade: true,
            grouping: true,
        }
    }
}

impl EditSettings {
    /// Whether locking is on and covers `mode`.
    pub fn locks(&self, mode: u32) -> bool {
        self.lock & LOCK_ENABLED != 0 && self.lock & mode != 0
    }

    /// Reads one of the project lines this covers; false for other lines.
    pub fn read_line(&mut self, t: &[String]) -> bool {
        let n = |i: usize, d: f64| t.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(d);
        match t.first().map(String::as_str) {
            Some("RIPPLE") => self.ripple = RippleMode::from_code(n(1, 0.0) as i64),
            Some("GRID") => {
                let flags = n(1, 3199.0) as i64;
                let kind = n(7, 0.0) as i64;
                self.grid = Grid {
                    qn: n(3, 1.0).max(1e-6),
                    kind: if kind & GRID_MEASURE != 0 {
                        GridKind::Measure
                    } else if kind & GRID_SWING != 0 {
                        GridKind::Swing
                    } else if kind & GRID_FRAMES != 0 {
                        GridKind::Frames
                    } else {
                        GridKind::Normal
                    },
                    swing: n(8, 0.0),
                    snap: flags & GRID_SNAP_OFF == 0,
                    lines: flags & GRID_LINES != 0,
                };
            }
            Some("LOCK") => self.lock = n(1, 1.0) as u32,
            Some("AUTOXFADE") => self.auto_crossfade = n(1, 129.0) as i64 & AUTOXFADE_ON != 0,
            Some("GROUPOVERRIDE") => self.grouping = n(1, 0.0) == 0.0,
            _ => return false,
        }
        true
    }

    /// The line `key` as it is now: `old` (the file's tokens) with the modelled fields set, or
    /// a new line when there was none. `None` for a key this does not cover.
    pub fn line(&self, key: &str, old: Option<&[String]>) -> Option<Vec<String>> {
        let default: &[&str] = match key {
            "RIPPLE" => &["RIPPLE", "0", "0"],
            "GRID" => &["GRID", "3199", "8", "1", "8", "1", "0", "0", "0"],
            "LOCK" => &["LOCK", "1"],
            "AUTOXFADE" => &["AUTOXFADE", "129"],
            "GROUPOVERRIDE" => &["GROUPOVERRIDE", "0", "0", "0", "0"],
            _ => return None,
        };
        let mut t: Vec<String> = match old {
            Some(o) if o.len() >= default.len() => o.to_vec(),
            _ => default.iter().map(|s| (*s).to_owned()).collect(),
        };
        let n = |t: &[String], i: usize| t.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) as i64;
        match key {
            "RIPPLE" => {
                t[1] = self.ripple.code().to_string();
                if self.ripple != RippleMode::Off {
                    t[2] = ((self.ripple == RippleMode::AllTracks) as i64).to_string();
                }
            }
            "GRID" => {
                let mut flags = n(&t, 1) & !(GRID_SNAP_OFF | GRID_LINES);
                if !self.grid.snap {
                    flags |= GRID_SNAP_OFF;
                }
                if self.grid.lines {
                    flags |= GRID_LINES;
                }
                t[1] = flags.to_string();
                if t[3].parse::<f64>().ok() != Some(self.grid.qn) {
                    t[3] = crate::rpp::fmt_num(self.grid.qn);
                }
                let kind = n(&t, 7) & !(GRID_SWING | GRID_MEASURE | GRID_FRAMES)
                    | match self.grid.kind {
                        GridKind::Normal => 0,
                        GridKind::Swing => GRID_SWING,
                        GridKind::Measure => GRID_MEASURE,
                        GridKind::Frames => GRID_FRAMES,
                    };
                t[7] = kind.to_string();
                if t[8].parse::<f64>().ok() != Some(self.grid.swing) {
                    t[8] = crate::rpp::fmt_num(self.grid.swing);
                }
            }
            "LOCK" => t[1] = self.lock.to_string(),
            "AUTOXFADE" => {
                let v = n(&t, 1) & !AUTOXFADE_ON | if self.auto_crossfade { AUTOXFADE_ON } else { 0 };
                t[1] = v.to_string();
            }
            _ => t[1] = (!self.grouping as i64).to_string(),
        }
        Some(t)
    }

    pub const KEYS: [&'static str; 5] = ["RIPPLE", "GRID", "LOCK", "AUTOXFADE", "GROUPOVERRIDE"];
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn grid_snaps_like_snap_to_grid() {
        let tempo = TempoMap::constant(120.0, 4, 4);
        let mut g = Grid::default();
        // REAPER 7.73's SnapToGrid with a 1/4 grid at 120 BPM.
        for (t, want) in [(0.1, 0.0), (0.13, 0.0), (0.4, 0.5), (0.62, 0.5), (1.1, 1.0)] {
            assert!((g.snap(&tempo, t) - want).abs() < 1e-9, "{t}");
        }
        g.snap = false;
        assert_eq!(g.snap(&tempo, 0.13), 0.13);
        // 1/8 with 50 % swing: every second line moves a quarter step later.
        g = Grid { qn: 0.5, kind: GridKind::Swing, swing: 0.5, ..Grid::default() };
        for (t, want) in [(0.1, 0.0), (0.2, 0.3125), (0.3, 0.3125), (0.4, 0.3125), (0.62, 0.5)] {
            assert!((g.snap(&tempo, t) - want).abs() < 1e-9, "{t}");
        }
    }

    #[test]
    fn lines_round_trip_and_keep_unknown_fields() {
        let mut s = EditSettings::default();
        for line in ["RIPPLE 2 1", "GRID 3455 8 0.5 8 1 0 8 0.5", "LOCK 16387", "AUTOXFADE 128", "GROUPOVERRIDE 1 0 0 0"]
        {
            assert!(s.read_line(&toks(line)));
        }
        assert_eq!(s.ripple, RippleMode::AllTracks);
        assert_eq!(s.grid, Grid { qn: 0.5, kind: GridKind::Swing, swing: 0.5, snap: false, lines: true });
        assert!(s.locks(LOCK_ITEMS) && !s.locks(LOCK_MARKERS));
        assert!(!s.auto_crossfade && !s.grouping);
        let line = |s: &EditSettings, k: &str, old: &str| s.line(k, Some(&toks(old))).unwrap().join(" ");
        assert_eq!(line(&s, "GRID", "GRID 3455 8 0.5 8 1 0 8 0.5"), "GRID 3455 8 0.5 8 1 0 8 0.5");
        assert_eq!(line(&s, "AUTOXFADE", "AUTOXFADE 385"), "AUTOXFADE 384");
        s.grid.kind = GridKind::Measure;
        s.grid.snap = true;
        assert_eq!(line(&s, "GRID", "GRID 3455 8 0.5 8 1 0 8 0.5"), "GRID 3199 8 0.5 8 1 0 64 0.5");
        assert_eq!(s.line("RIPPLE", None).unwrap().join(" "), "RIPPLE 2 1");
    }

    #[test]
    fn grid_labels() {
        let g = |qn| Grid { qn, ..Grid::default() }.label();
        assert_eq!(g(1.0), "1/4");
        assert_eq!(g(0.25), "1/16");
        assert_eq!(g(1.0 / 6.0), "1/16T");
        assert_eq!(g(2.0 / 3.0), "1/4T");
        assert_eq!(g(4.0), "1");
        assert_eq!(g(16.0), "4");
    }
}
