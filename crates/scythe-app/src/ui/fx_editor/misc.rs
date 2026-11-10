//! The smaller editors: ReaVerbate's decay, ReaVerb's impulse response, ReaSynth's oscillator
//! and envelope, ReaInsert's routing and ping, ReaControlMIDI's controls.

use egui::{Align2, CornerRadius, Pos2, Rect, Stroke, Ui, pos2, vec2};
use scythe_engine::reaverb::ReaVerbCompat;
use scythe_engine::stock::reacontrolmidi::ControlMidi;
use scythe_engine::stock::reainsert::{INPUT, OUTPUT, PING};
use scythe_engine::stock::reasynth::Synth;
use scythe_engine::stock::reaverbate::Reverb;

use super::model::{Plot, Scale};
use super::{Ctx, area, card, grid, knob_row, polyline, section_title, split_left, split_top, value_box};
use crate::theme::{self, ACCENT, BG, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets;

/// Knobs under a graph, in one or two rows as the height allows.
fn knob_panel(cx: &mut Ctx, ui: &mut Ui, rect: Rect, knobs: &[(u32, &str)]) {
    card(ui, rect);
    let inner = rect.shrink2(vec2(8.0, 6.0));
    if inner.height() >= 150.0 && knobs.len() > 4 {
        let half = knobs.len().div_ceil(2);
        let (a, b) = split_top(inner, inner.height() / 2.0, 0.0);
        knob_row(cx, ui, a, &knobs[..half]);
        knob_row(cx, ui, b, &knobs[half..]);
    } else {
        knob_row(cx, ui, inner, knobs);
    }
}

fn panel_height(rect: Rect) -> f32 {
    if rect.height() > 420.0 { 180.0 } else { 100.0 }
}

pub fn verbate(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let Some((decays, predelay)) = cx.read(|r: &mut Reverb| {
        let decays: Vec<(f64, f64)> =
            (0..=60).map(|i| 20.0 * 1000f64.powf(i as f64 / 60.0)).map(|f| (f, r.decay_seconds(f))).collect();
        (decays, r.predelay())
    }) else {
        return;
    };
    let (graphs, knobs) = split_top(rect, rect.height() - panel_height(rect) - 8.0, 8.0);
    let (decay_card, tail_card) = split_left(graphs, graphs.width() / 2.0 - 4.0, 8.0);
    card(ui, decay_card);
    section_title(ui, decay_card, "Decay time by frequency");
    let longest = decays.iter().map(|d| d.1).fold(0.5, f64::max);
    let top = (longest * 1.2).max(0.5);
    let plot = Plot::new(
        Rect::from_min_max(decay_card.min + vec2(10.0, 30.0), decay_card.max - vec2(10.0, 10.0)),
        Scale::log(20.0, 20000.0),
        Scale::lin(0.0, top),
    );
    grid(ui, &plot, true, nice_step(top), " s");
    let points: Vec<Pos2> = decays.iter().map(|&(f, s)| plot.point(f, s)).collect();
    area(ui, plot.rect, &points, plot.rect.bottom(), theme::with_alpha(ACCENT, 40));
    polyline(ui, plot.rect, points, Stroke::new(2.5, TEXT));
    card(ui, tail_card);
    let low = decays.first().map_or(0.0, |d| d.1);
    section_title(ui, tail_card, &format!("Tail · {:.2} s at low frequencies", low));
    let span = (predelay + low * 1.1).max(0.2);
    let tplot = Plot::new(
        Rect::from_min_max(tail_card.min + vec2(10.0, 30.0), tail_card.max - vec2(10.0, 10.0)),
        Scale::lin(0.0, span),
        Scale::lin(-60.0, 0.0),
    );
    grid(ui, &tplot, false, 12.0, " dB");
    let curve = |hz_index: usize, color| {
        let t60 = decays[hz_index].1.max(1e-3);
        let pts: Vec<Pos2> = (0..=120)
            .map(|i| {
                let t = span * i as f64 / 120.0;
                let db = if t < predelay { -60.0 } else { -60.0 * (t - predelay) / t60 };
                tplot.point(t, db.max(-60.0))
            })
            .collect();
        polyline(ui, tplot.rect, pts, Stroke::new(2.0, color));
    };
    curve(0, TEXT);
    curve(40, theme::with_alpha(ACCENT, 220));
    curve(56, theme::with_alpha(theme::YELLOW, 200));
    let legend = [(TEXT, "20 Hz"), (ACCENT, "2 kHz"), (theme::YELLOW, "10 kHz")];
    for (i, (c, l)) in legend.iter().enumerate() {
        let p = tplot.rect.left_bottom() + vec2(10.0, -52.0 + i as f32 * 14.0);
        ui.painter().line_segment([p, p + vec2(12.0, 0.0)], Stroke::new(2.0, *c));
        ui.painter().text(p + vec2(16.0, 0.0), Align2::LEFT_CENTER, *l, theme::regular(10.0), TEXT_DIM);
    }
    knob_panel(
        cx,
        ui,
        knobs,
        &[
            (2, "Room size"),
            (3, "Damping"),
            (4, "Width"),
            (5, "Pre-delay"),
            (7, "Highpass"),
            (6, "Lowpass"),
            (0, "Wet"),
            (1, "Dry"),
        ],
    );
}

/// A round step for a linear axis up to `top`.
pub(crate) fn nice_step(top: f64) -> f64 {
    [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0].into_iter().find(|s| top / s <= 6.0).unwrap_or(20.0)
}

pub fn verb(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let Some((envelope, length, file)) = cx.read(|r: &mut ReaVerbCompat| {
        (r.envelope().0.to_vec(), r.envelope().1, r.file().map(|f| f.display().to_string()))
    }) else {
        return;
    };
    let (graph_card, knobs) = split_top(rect, rect.height() - panel_height(rect).min(110.0) - 8.0, 8.0);
    card(ui, graph_card);
    let title = match &file {
        Some(f) => format!(
            "Impulse response · {} · {length:.2} s",
            std::path::Path::new(f).file_name().map_or(f.clone(), |n| n.to_string_lossy().into_owned())
        ),
        None => format!("Impulse response · {length:.2} s"),
    };
    section_title(ui, graph_card, &title);
    let plot = Plot::new(
        Rect::from_min_max(graph_card.min + vec2(10.0, 30.0), graph_card.max - vec2(10.0, 10.0)),
        Scale::lin(0.0, length.max(0.01)),
        Scale::lin(-90.0, 0.0),
    );
    grid(ui, &plot, false, 12.0, " dB");
    if envelope.is_empty() {
        ui.painter().text(
            plot.rect.center(),
            Align2::CENTER_CENTER,
            "No impulse response loaded",
            theme::regular(12.0),
            TEXT_DIM,
        );
    } else {
        let top = envelope.iter().copied().fold(f32::MIN, f32::max);
        let n = envelope.len();
        let points: Vec<Pos2> = envelope
            .iter()
            .enumerate()
            .map(|(i, db)| plot.point(length * (i as f64 + 0.5) / n as f64, (*db - top).max(-90.0) as f64))
            .collect();
        area(ui, plot.rect, &points, plot.rect.bottom(), theme::with_alpha(ACCENT, 50));
        polyline(ui, plot.rect, points, Stroke::new(2.0, TEXT));
    }
    knob_panel(cx, ui, knobs, &[(0, "Wet"), (1, "Dry")]);
}

pub fn synth(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let attack = cx.value(0) / 1000.0;
    let decay = cx.value(6) / 1000.0;
    let release = cx.value(1) / 1000.0;
    let hold = attack + decay.min(2.0) + 0.25;
    let limit = hold + release.min(5.0) + 0.05;
    let Some((cycle, envelope)) = cx.read(|s: &mut Synth| (s.cycle(256), s.envelope(hold, limit / 400.0, limit)))
    else {
        return;
    };
    let (graphs, knobs) = split_top(rect, rect.height() - panel_height(rect) - 8.0, 8.0);
    let (osc_card, env_card) = split_left(graphs, (graphs.width() * 0.4).max(160.0), 8.0);
    card(ui, osc_card);
    section_title(ui, osc_card, "Oscillator");
    let plot = Plot::new(
        Rect::from_min_max(osc_card.min + vec2(10.0, 30.0), osc_card.max - vec2(10.0, 10.0)),
        Scale::lin(0.0, 1.0),
        Scale::lin(-1.2, 1.2),
    );
    grid(ui, &plot, false, 1.0, "");
    let n = cycle.len().max(1);
    let points: Vec<Pos2> =
        (0..n * 2).map(|i| plot.point(i as f64 / (2 * n) as f64, cycle[i % n].clamp(-1.2, 1.2))).collect();
    polyline(ui, plot.rect, points, Stroke::new(2.0, ACCENT));
    card(ui, env_card);
    section_title(ui, env_card, "Envelope");
    let eplot = Plot::new(
        Rect::from_min_max(env_card.min + vec2(10.0, 30.0), env_card.max - vec2(10.0, 10.0)),
        Scale::lin(0.0, limit),
        Scale::lin(0.0, 1.05),
    );
    grid(ui, &eplot, false, 0.25, "");
    let points: Vec<Pos2> = envelope.iter().map(|&(t, l)| eplot.point(t, l)).collect();
    area(ui, eplot.rect, &points, eplot.rect.bottom(), theme::with_alpha(ACCENT, 40));
    polyline(ui, eplot.rect, points, Stroke::new(2.5, TEXT));
    let x = eplot.sx(hold);
    ui.painter_at(eplot.rect).line_segment(
        [pos2(x, eplot.rect.top()), pos2(x, eplot.rect.bottom())],
        Stroke::new(1.0, theme::with_alpha(TEXT_DIM, 120)),
    );
    ui.painter().text(
        pos2(x - 4.0, eplot.rect.top() + 4.0),
        Align2::RIGHT_TOP,
        "note off",
        theme::regular(10.0),
        TEXT_DIM,
    );
    for (label, t) in [
        ("A", attack * 0.5),
        ("D", attack + decay.min(2.0) * 0.3),
        ("S", (attack + hold) / 2.0 + 0.1),
        ("R", hold + release.min(5.0) * 0.4),
    ] {
        if t < limit {
            ui.painter().text(
                pos2(eplot.sx(t), eplot.rect.bottom() - 6.0),
                Align2::CENTER_BOTTOM,
                label,
                theme::semibold(11.0),
                TEXT_MID,
            );
        }
    }
    knob_panel(
        cx,
        ui,
        knobs,
        &[
            (2, "Square"),
            (3, "Saw"),
            (4, "Triangle"),
            (7, "Sine"),
            (8, "Tune"),
            (10, "Width"),
            (0, "Attack"),
            (6, "Decay"),
            (9, "Sustain"),
            (1, "Release"),
            (11, "Detune"),
            (5, "Volume"),
        ],
    );
}

pub fn insert(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let (diagram, knobs) = split_top(rect, rect.height() - panel_height(rect).min(110.0) - 8.0, 8.0);
    card(ui, diagram);
    section_title(ui, diagram, "Hardware insert");
    let inner = Rect::from_min_max(diagram.min + vec2(12.0, 34.0), diagram.max - vec2(12.0, 12.0));
    let row_h = 30.0;
    let mid = inner.top() + (inner.height() / 2.0 - 40.0).max(row_h);
    let boxes =
        [("Track", None), ("Send to", Some(OUTPUT)), ("Device", None), ("Return from", Some(INPUT)), ("Track", None)];
    let w = (inner.width() - 4.0 * 18.0) / 5.0;
    let mut centres = Vec::new();
    for (i, (label, id)) in boxes.iter().enumerate() {
        let r = Rect::from_min_size(pos2(inner.left() + i as f32 * (w + 18.0), mid - row_h / 2.0), vec2(w, row_h));
        match id {
            Some(id) => {
                ui.painter().text(
                    r.center_top() - vec2(0.0, 4.0),
                    Align2::CENTER_BOTTOM,
                    *label,
                    theme::regular(10.0),
                    TEXT_DIM,
                );
                value_box(cx, ui, r, *id, "");
            }
            None => {
                ui.painter().rect_filled(r, CornerRadius::same(8), theme::RAISED_HI);
                ui.painter().text(r.center(), Align2::CENTER_CENTER, *label, theme::semibold(11.0), TEXT_MID);
            }
        }
        centres.push(r);
    }
    for pair in centres.windows(2) {
        let (a, b) = (pair[0].right_center(), pair[1].left_center());
        ui.painter().arrow(a + vec2(2.0, 0.0), b - a - vec2(4.0, 0.0), Stroke::new(1.5, TEXT_DIM));
    }
    let listening = cx.text(PING) == "listening";
    let frames = cx.value(0);
    let sr = cx.inst.probe.sample_rate().max(1) as f64;
    let ping = Rect::from_min_size(pos2(inner.left(), mid + row_h / 2.0 + 18.0), vec2(120.0, 28.0));
    let (bg, fg) = if listening { (ACCENT, BG) } else { (theme::RAISED_HI, TEXT) };
    if widgets::text_pill(ui, ping, if listening { "Listening…" } else { "Ping" }, theme::semibold(12.0), bg, fg)
        .on_hover_text("Send a click out and time its return to set the delay compensation")
        .clicked()
    {
        cx.set_once(PING, 1.0);
    }
    let measured = if frames > 0.0 {
        format!("Delay compensation: {frames:.0} frames ({:.2} ms)", frames / sr * 1000.0)
    } else {
        "Delay compensation: none".to_owned()
    };
    ui.painter().text(ping.right_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, measured, theme::regular(12.0), TEXT);
    let delay = Rect::from_min_size(pos2(inner.left(), ping.bottom() + 10.0), vec2(inner.width().min(320.0), 26.0));
    value_box(cx, ui, delay, 0, "Delay (frames)");
    knob_panel(cx, ui, knobs, &[(1, "Send"), (2, "Return")]);
}

const NOTES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

pub fn control_midi(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let scale = cx.read(|c: &mut ControlMidi| c.scale()).unwrap_or([true; 12]);
    let fields: [(u32, &str); 9] = [
        (8, "Channel"),
        (0, "Bank MSB"),
        (1, "Bank LSB"),
        (2, "Program"),
        (3, "Volume"),
        (4, "Pan"),
        (5, "Pitch"),
        (9, "Transpose"),
        (11, "Root"),
    ];
    let (top, keys) = split_top(rect, rect.height() - 110.0, 8.0);
    card(ui, top);
    let inner = top.shrink(10.0);
    let cols = if inner.width() > 520.0 { 3 } else { 2 };
    let w = (inner.width() - (cols - 1) as f32 * 8.0) / cols as f32;
    for (i, (id, label)) in fields.iter().enumerate() {
        let (r, c) = (i / cols, i % cols);
        let rr = Rect::from_min_size(
            pos2(inner.left() + c as f32 * (w + 8.0), inner.top() + r as f32 * 34.0),
            vec2(w, 28.0),
        );
        if rr.bottom() <= inner.bottom() {
            value_box(cx, ui, rr, *id, label);
        }
    }
    let rows = fields.len().div_ceil(cols);
    let sw_top = inner.top() + rows as f32 * 34.0 + 4.0;
    let switches = [(13, "Send bank/program"), (14, "Send controls"), (10, "Snap to scale")];
    if sw_top + 24.0 <= inner.bottom() {
        super::switch_row(
            cx,
            ui,
            Rect::from_min_size(pos2(inner.left(), sw_top), vec2(inner.width(), 24.0)),
            &switches,
        );
    }
    card(ui, keys);
    section_title(ui, keys, "Scale");
    let root = cx.value(11).round().rem_euclid(12.0) as usize;
    let snap = cx.value(10) >= 0.5;
    let kb = Rect::from_min_max(keys.min + vec2(10.0, 30.0), keys.max - vec2(10.0, 10.0));
    let kw = kb.width() / 12.0;
    for (n, name) in NOTES.iter().enumerate() {
        let r = Rect::from_min_size(pos2(kb.left() + n as f32 * kw + 1.0, kb.top()), vec2(kw - 2.0, kb.height()));
        let degree = (n + 12 - root) % 12;
        let in_scale = scale[degree];
        let black = name.contains('#');
        let fill = if n == root {
            ACCENT
        } else if in_scale && snap {
            theme::with_alpha(ACCENT, 90)
        } else if black {
            BG
        } else {
            theme::RAISED_HI
        };
        ui.painter().rect_filled(r, CornerRadius::same(6), fill);
        ui.painter().text(
            r.center_bottom() - vec2(0.0, 8.0),
            Align2::CENTER_BOTTOM,
            *name,
            theme::regular(10.0),
            if n == root { BG } else { TEXT_MID },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_steps_stay_readable() {
        assert_eq!(nice_step(0.5), 0.1);
        assert_eq!(nice_step(3.0), 0.5);
        assert_eq!(nice_step(12.0), 2.0);
    }
}
