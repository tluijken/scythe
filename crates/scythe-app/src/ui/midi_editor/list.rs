//! The event list: every note, controller, system exclusive and text event of the take in time
//! order, with their fields editable in place.

use egui::{Rect, Ui};
use scythe_core::midi::MidiSource;

use super::roll::{key_name, shown};
use super::{Take, apply_notes, apply_source};
use crate::app::ScytheApp;

enum Row {
    Note(usize),
    Event(usize),
    Long(usize),
}

fn tick_of(take: &Take, r: &Row) -> u64 {
    match r {
        Row::Note(i) => take.notes[*i].start,
        Row::Event(i) => take.src.events[*i].tick,
        Row::Long(i) => take.src.long[*i].tick,
    }
}

fn kind(bytes: [u8; 3]) -> &'static str {
    match bytes[0] & 0xf0 {
        0xa0 => "Poly aftertouch",
        0xb0 => "Controller",
        0xc0 => "Program",
        0xd0 => "Channel pressure",
        0xe0 => "Pitch bend",
        _ => "Other",
    }
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, take: &Take) {
    let ppq = take.src.ticks_per_qn as f64;
    let mut rows: Vec<Row> =
        (0..take.notes.len()).filter(|&i| shown(take, take.notes[i].channel)).map(Row::Note).collect();
    rows.extend(
        take.src
            .events
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e.bytes[0] & 0xf0, 0xa0..=0xe0) && shown(take, e.bytes[0] & 0x0f))
            .filter(|(_, e)| !(e.tick == take.src.length_ticks && e.bytes[0] & 0xf0 == 0xb0 && e.bytes[1] == 0x7b))
            .map(|(i, _)| Row::Event(i)),
    );
    rows.extend((0..take.src.long.len()).map(Row::Long));
    rows.sort_by_key(|r| tick_of(take, r));

    let mut notes = take.notes.clone();
    let mut src: MidiSource = take.src.clone();
    let (mut notes_changed, mut src_changed, mut delete) = (false, false, None);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
        egui::Grid::new(("midi-list", take.id)).striped(true).num_columns(7).show(ui, |ui| {
            for h in ["Position", "Length", "Channel", "Type", "Parameter", "Value", ""] {
                ui.strong(h);
            }
            ui.end_row();
            for (n, r) in rows.iter().enumerate() {
                match *r {
                    Row::Note(i) => {
                        let note = &mut notes[i];
                        let mut start = note.start as f64 / ppq;
                        let mut len = note.length() as f64 / ppq;
                        let mut ch = note.channel + 1;
                        let a =
                            ui.add(egui::DragValue::new(&mut start).speed(0.01).range(0.0..=f64::MAX).max_decimals(3));
                        let b = ui.add(
                            egui::DragValue::new(&mut len).speed(0.01).range(1.0 / ppq..=f64::MAX).max_decimals(3),
                        );
                        let c = ui.add(egui::DragValue::new(&mut ch).range(1..=16));
                        ui.label(if note.muted { "Note (muted)" } else { "Note" });
                        let d = ui.add(
                            egui::DragValue::new(&mut note.key)
                                .range(0..=127)
                                .custom_formatter(|v, _| key_name(v as u8)),
                        );
                        let e = ui.add(egui::DragValue::new(&mut note.velocity).range(1..=127));
                        if [a, b, c, d, e].iter().any(|r| r.changed()) {
                            note.start = (start * ppq).round() as u64;
                            note.end = note.start + (len * ppq).round().max(1.0) as u64;
                            note.channel = ch - 1;
                            notes_changed = true;
                        }
                    }
                    Row::Event(i) => {
                        let e = &mut src.events[i];
                        let mut at = e.tick as f64 / ppq;
                        let mut ch = (e.bytes[0] & 0x0f) + 1;
                        let a = ui.add(egui::DragValue::new(&mut at).speed(0.01).range(0.0..=f64::MAX).max_decimals(3));
                        ui.label("");
                        let c = ui.add(egui::DragValue::new(&mut ch).range(1..=16));
                        let shape = e.shape.map(|s| format!(" · {}", super::shape_name(s.kind))).unwrap_or_default();
                        ui.label(format!("{}{shape}", kind(e.bytes)));
                        let (d, v) = match e.bytes[0] & 0xf0 {
                            0xe0 => {
                                ui.label("");
                                let mut v = (e.bytes[2] as u16) << 7 | e.bytes[1] as u16;
                                let r = ui.add(egui::DragValue::new(&mut v).range(0..=16383));
                                if r.changed() {
                                    e.bytes[1] = (v & 0x7f) as u8;
                                    e.bytes[2] = (v >> 7) as u8;
                                }
                                (None, r)
                            }
                            0xc0 | 0xd0 => {
                                ui.label("");
                                (None, ui.add(egui::DragValue::new(&mut e.bytes[1]).range(0..=127)))
                            }
                            _ => {
                                let d = ui.add(egui::DragValue::new(&mut e.bytes[1]).range(0..=127));
                                (Some(d), ui.add(egui::DragValue::new(&mut e.bytes[2]).range(0..=127)))
                            }
                        };
                        if a.changed() || c.changed() || v.changed() || d.is_some_and(|d| d.changed()) {
                            e.tick = (at * ppq).round() as u64;
                            e.bytes[0] = (e.bytes[0] & 0xf0) | (ch - 1);
                            src_changed = true;
                        }
                    }
                    Row::Long(i) => {
                        let x = &mut src.long[i];
                        let mut at = x.tick as f64 / ppq;
                        if ui
                            .add(egui::DragValue::new(&mut at).speed(0.01).range(0.0..=f64::MAX).max_decimals(3))
                            .changed()
                        {
                            x.tick = (at * ppq).round() as u64;
                            src_changed = true;
                        }
                        ui.label("");
                        ui.label("");
                        let (k, text) = match x.data.as_slice() {
                            [0xf0, ..] => {
                                ("Sysex", x.data.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "))
                            }
                            [0xff, 0x0f, rest @ ..] => ("Notation", String::from_utf8_lossy(rest).into_owned()),
                            [0xff, _, rest @ ..] => ("Text", String::from_utf8_lossy(rest).into_owned()),
                            _ => ("Meta", String::new()),
                        };
                        ui.label(k);
                        ui.label(text);
                        ui.label("");
                    }
                }
                if ui.small_button("×").on_hover_text("Delete").clicked() {
                    delete = Some(n);
                }
                ui.end_row();
            }
        });
    });
    if let Some(n) = delete {
        match rows[n] {
            Row::Note(i) => {
                notes.remove(i);
                notes_changed = true;
            }
            Row::Event(i) => {
                src.events.remove(i);
                src_changed = true;
            }
            Row::Long(i) => {
                src.long.remove(i);
                src_changed = true;
            }
        }
    }
    if src_changed {
        src.events.sort_by_key(|e| e.tick);
        src.long.sort_by_key(|x| x.tick);
        apply_source(app, take.id, src, "Edit MIDI events", true);
    } else if notes_changed {
        apply_notes(app, take.id, &notes, "Edit MIDI events", true);
    }
    if !ui.ctx().egui_is_using_pointer() {
        app.end_gesture();
    }
}
