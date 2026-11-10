//! The MIDI editor actions Scythe has, by their command ids in REAPER's MIDI editor section
//! (`SECTION`, 32060 in `reaper-kb.ini` and `MIDIEditor_OnCommand`), each run on a take's
//! source and its editor settings as REAPER runs it on the selected notes (all notes when none
//! is selected) and the take's grid.

use super::edit::{self, Grid, Quantize, QuantizeWhat};
use super::scale::KeySnap;
use super::view::{EditorMode, EditorView, LaneKind};
use super::{CcShape, MidiSource, ShapeKind};

/// REAPER's MIDI editor section id.
pub const SECTION: u32 = 32060;

/// (command id, REAPER's action name).
pub const ACTIONS: &[(u32, &str)] = &[
    (40002, "Edit: Delete notes"),
    (40003, "Edit: Select all notes"),
    (40214, "Edit: Unselect all"),
    (40055, "Edit: Mute events (toggle)"),
    (40057, "Edit: Mute events"),
    (40058, "Edit: Unmute events"),
    (40177, "Edit: Move notes up one semitone"),
    (40178, "Edit: Move notes down one semitone"),
    (40179, "Edit: Move notes up one octave"),
    (40180, "Edit: Move notes down one octave"),
    (41026, "Edit: Move notes up one semitone ignoring scale/key"),
    (41027, "Edit: Move notes down one semitone ignoring scale/key"),
    (40183, "Edit: Move notes left one grid unit"),
    (40184, "Edit: Move notes right one grid unit"),
    (40446, "Edit: Lengthen notes one grid unit"),
    (40447, "Edit: Shorten notes one grid unit"),
    (40633, "Edit: Set note lengths to grid size"),
    (40462, "Edit: Note velocity +01"),
    (40463, "Edit: Note velocity +10"),
    (40464, "Edit: Note velocity -01"),
    (40465, "Edit: Note velocity -10"),
    (40456, "Edit: Join notes"),
    (40641, "Split notes on grid"),
    (40405, "Set note ends to start of next note (legato)"),
    (40765, "Edit: Make notes legato, preserving note start times"),
    (40766, "Edit: Make notes legato, preserving relative note spacing"),
    (40469, "Quantize notes position to grid"),
    (40728, "Quantize events to grid"),
    (40729, "Quantize notes position and end to grid"),
    (40884, "Edit: Duplicate events one octave higher"),
    (40885, "Edit: Duplicate events one octave lower"),
    (40767, "Force selected notes into key signature"),
    (42471, "Toggle snap to key signature"),
    (40042, "Mode: Piano Roll"),
    (40043, "Mode: Named Notes (Drum Map)"),
    (40056, "Mode: Event List"),
    (40190, "Grid: Set to 1/32"),
    (40192, "Grid: Set to 1/16"),
    (40197, "Grid: Set to 1/8"),
    (40201, "Grid: Set to 1/4"),
    (40203, "Grid: Set to 1/2"),
    (40204, "Grid: Set to 1"),
    (40199, "Grid: Set to 1/6 (1/4 note triplet)"),
    (40193, "Grid: Set to 1/12 (1/8 note triplet)"),
    (40191, "Grid: Set to 1/24 (1/16 note triplet)"),
    (41003, "Grid: Set grid type to straight"),
    (41006, "Grid: Set grid type to swing"),
    (40217, "Channel: Show all channels"),
    (40237, "CC: Set CC lane to Velocity"),
    (40366, "CC: Set CC lane to Pitch"),
    (40367, "CC: Set CC lane to Program"),
    (40368, "CC: Set CC lane to Channel Pressure"),
    (40358, "CC: Set CC lane to Poly Aftertouch"),
    (40371, "CC: Set CC lane to Sysex"),
    (40370, "CC: Set CC lane to Text Events"),
    (42080, "Set CC shape to linear"),
    (42081, "Set CC shape to square"),
    (42082, "Set CC shape to slow start/end"),
    (42083, "Set CC shape to fast start"),
    (42084, "Set CC shape to fast end"),
    (42085, "Set CC shape to bezier"),
];

/// Ranges of numbered actions: channels for events (40020-40035), channels shown
/// (40218-40233) and CC lanes 0-119 (40238-40357).
pub fn name(id: u32) -> Option<String> {
    if let Some((_, n)) = ACTIONS.iter().find(|a| a.0 == id) {
        return Some((*n).to_owned());
    }
    match id {
        40020..=40035 => Some(format!("Edit: Set events to channel {:02}", id - 40019)),
        40218..=40233 => Some(format!("Channel: Show only channel {:02}", id - 40217)),
        40238..=40357 => Some(format!("CC: Set CC lane to {:03}", id - 40238)),
        _ => None,
    }
}

/// Every action id this module runs, the numbered ones included.
pub fn ids() -> Vec<u32> {
    let mut out: Vec<u32> = ACTIONS.iter().map(|a| a.0).collect();
    out.extend((40020..=40035).chain(40218..=40233).chain(40238..=40357));
    out
}

/// What running an action needs besides the source: key snap's root and scale (REAPER keeps
/// them in the editor, not the take).
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub key: Option<KeySnap>,
}

/// Runs action `id` on a take's source and editor settings. `None` when Scythe has no such
/// action; the returned source and settings otherwise (unchanged when the action changes
/// nothing).
pub fn run(id: u32, src: &MidiSource, view: &EditorView, ctx: &Context) -> Option<(MidiSource, EditorView)> {
    let ppq = src.ticks_per_qn;
    let grid = Grid { qn: view.grid_qn, swing: view.swing.unwrap_or(0.0) };
    let g = (view.grid_qn * ppq as f64).round() as i64;
    let mut notes = src.edit_notes();
    let mut v = view.clone();
    let snap = ctx.key.as_ref().filter(|_| view.key_snap);
    let notes_changed = match id {
        40002 => {
            let t = edit::targets(&notes);
            let mut i = 0;
            notes.retain(|_| {
                i += 1;
                !t.contains(&(i - 1))
            });
            true
        }
        40003 => notes.iter_mut().map(|n| n.selected = true).count() > 0,
        40214 => notes.iter_mut().map(|n| n.selected = false).count() > 0,
        40055 => {
            edit::mute(&mut notes, None);
            true
        }
        40057 | 40058 => {
            edit::mute(&mut notes, Some(id == 40057));
            true
        }
        40177 | 40178 => {
            edit::transpose(&mut notes, if id == 40177 { 1 } else { -1 }, snap);
            true
        }
        40179 | 40180 => {
            edit::transpose(&mut notes, if id == 40179 { 12 } else { -12 }, None);
            true
        }
        41026 | 41027 => {
            edit::transpose(&mut notes, if id == 41026 { 1 } else { -1 }, None);
            true
        }
        40183 | 40184 => {
            edit::shift(&mut notes, if id == 40184 { g } else { -g });
            true
        }
        40446 | 40447 => {
            edit::lengthen(&mut notes, if id == 40446 { g } else { -g });
            true
        }
        40633 => {
            edit::set_length(&mut notes, g.max(1) as u64);
            true
        }
        40462..=40465 => {
            edit::velocity(&mut notes, [1, 10, -1, -10][(id - 40462) as usize], false);
            true
        }
        40456 => {
            edit::join(&mut notes);
            true
        }
        40641 => {
            edit::split_on_grid(&mut notes, ppq, grid);
            true
        }
        40405 | 40765 => {
            edit::legato(&mut notes);
            true
        }
        40766 => {
            edit::legato_relative(&mut notes);
            true
        }
        40469 | 40728 | 40729 => {
            let what = if id == 40729 { QuantizeWhat::PositionAndEnd } else { QuantizeWhat::Position };
            edit::quantize(&mut notes, ppq, &Quantize { what, ..Quantize::to_grid(grid) });
            true
        }
        40884 | 40885 => {
            edit::duplicate_transposed(&mut notes, if id == 40884 { 12 } else { -12 });
            true
        }
        40767 => match ctx.key.as_ref() {
            Some(k) => {
                edit::force_into_key(&mut notes, k);
                true
            }
            None => false,
        },
        40020..=40035 => {
            edit::set_channel(&mut notes, (id - 40020) as u8);
            true
        }
        _ => {
            match id {
                42471 => v.key_snap = !v.key_snap,
                40042 => v.mode = EditorMode::PianoRoll,
                40043 => v.mode = EditorMode::NamedNotes,
                40056 => v.mode = EditorMode::EventList,
                40190 => v.grid_qn = 0.125,
                40192 => v.grid_qn = 0.25,
                40197 => v.grid_qn = 0.5,
                40201 => v.grid_qn = 1.0,
                40203 => v.grid_qn = 2.0,
                40204 => v.grid_qn = 4.0,
                40199 => v.grid_qn = 2.0 / 3.0,
                40193 => v.grid_qn = 1.0 / 3.0,
                40191 => v.grid_qn = 1.0 / 6.0,
                41003 => v.swing = None,
                41006 => v.swing = Some(v.swing.unwrap_or(0.5)),
                40217 => v.channel_filter = 0,
                40218..=40233 => v.channel_filter = 1 << (id - 40218),
                40237 | 40366 | 40367 | 40368 | 40358 | 40371 | 40370 | 40238..=40357 => {
                    let lane = match id {
                        40237 => LaneKind::Velocity,
                        40366 => LaneKind::PitchBend,
                        40367 => LaneKind::Program,
                        40368 => LaneKind::ChannelPressure,
                        40358 => LaneKind::PolyAftertouch,
                        40371 => LaneKind::Sysex,
                        40370 => LaneKind::Text,
                        n => LaneKind::Cc((n - 40238) as u8),
                    };
                    match v.lanes.first_mut() {
                        Some(first) => first.0 = lane,
                        None => v.lanes.push((lane, 100)),
                    }
                }
                42080..=42085 => {
                    let kind = [
                        ShapeKind::Linear,
                        ShapeKind::Square,
                        ShapeKind::SlowStartEnd,
                        ShapeKind::FastStart,
                        ShapeKind::FastEnd,
                        ShapeKind::Bezier,
                    ][(id - 42080) as usize];
                    let mut out = src.clone();
                    for e in out.events.iter_mut().filter(|e| e.flags & super::SELECTED != 0) {
                        if matches!(e.bytes[0] & 0xf0, 0xa0 | 0xb0 | 0xd0 | 0xe0) {
                            e.shape = (kind != ShapeKind::Square).then(|| CcShape::new(kind, 0.0));
                        }
                    }
                    return Some((out, v));
                }
                _ => return None,
            }
            false
        }
    };
    let out = if notes_changed { src.with_edit_notes(&notes) } else { src.clone() };
    Some((out, v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::parse;

    /// The notes of the REAPER 7.73 probe (all selected, 1/16 grid), and what REAPER's actions
    /// left (`MIDI_GetNote` after `MIDIEditor_OnCommand`).
    fn probe() -> (MidiSource, EditorView) {
        let c = parse("<SOURCE MIDI\n  HASDATA 1 960 QN\n  e 10 90 3d 64\n  e 307 80 3d 00\n  e 183 90 3e 5a\n  e 314 80 3e 00\n  e 186 90 3f 50\n  e 321 80 3f 00\n  e 379 90 40 46\n  e 328 80 40 00\n  e 372 90 3d 3c\n  e 335 80 3d 00\n  e 165 90 42 32\n  e 342 80 42 00\n  e 58 90 42 28\n  e 100 80 42 00\n  E 440 b0 7b 00\n>\n").unwrap();
        (MidiSource::parse(&c).unwrap(), EditorView::default())
    }

    fn spans(src: &MidiSource) -> Vec<(u64, u64, u8, u8)> {
        src.edit_notes().iter().map(|n| (n.start, n.end, n.key, n.velocity)).collect()
    }

    #[test]
    fn actions_match_reaper() {
        let (src, view) = probe();
        let run = |id| spans(&run(id, &src, &view, &Context::default()).unwrap().0);
        assert_eq!(run(40469).iter().map(|n| n.0).collect::<Vec<_>>(), [0, 480, 960, 1680, 2400, 2880, 3360]);
        assert_eq!(run(40729)[5], (2880, 3360, 66, 50));
        assert_eq!(run(40633)[0], (10, 250, 61, 100));
        assert_eq!(run(40447)[6], (3300, 3400, 66, 40));
        assert_eq!(run(40184)[0], (250, 557, 61, 100));
        assert_eq!(run(40463)[0].3, 110);
        assert_eq!(run(40464)[0].3, 99);
        assert_eq!(run(41026)[0].2, 62);
        assert_eq!(run(40456).len(), 5);
        assert_eq!(run(40641).len(), 14);
        assert_eq!(run(40884).len(), 14);
    }

    #[test]
    fn settings_actions_change_the_view() {
        let (src, view) = probe();
        let (_, v) = run(41006, &src, &view, &Context::default()).unwrap();
        assert_eq!(v.swing, Some(0.5));
        let (_, v) = run(40220, &src, &v, &Context::default()).unwrap();
        assert_eq!(v.channel_filter, 4);
        let (_, v) = run(40239, &src, &v, &Context::default()).unwrap();
        assert_eq!(v.lanes[0].0, LaneKind::Cc(1));
        assert!(run(1, &src, &view, &Context::default()).is_none());
        assert_eq!(name(40027).as_deref(), Some("Edit: Set events to channel 08"));
    }
}
