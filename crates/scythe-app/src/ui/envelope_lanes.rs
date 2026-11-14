//! The envelope lanes of the selected track, docked under the arrange view on its time axis:
//! one lane per envelope (the track's, its sends' and its effects' parameters) with point
//! editing and shapes, automation items (move, resize, loop, pool, LFO, baseline and amplitude,
//! points inside), arming and bypass, and the global automation override.

use egui::{Align2, Color32, CornerRadius, Id, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use scythe_core::automation_item::Scale;
use scythe_core::model::GlobalAutomation;
use scythe_core::{AutoMode, EnvPoint, Envelope, EnvelopeKind, PointShape, TrackId};
use scythe_session::api::envelope::{self as api, EnvRef};
use scythe_session::api::{Target, TrackRef};

use super::arrange::{HEADER_W, envelope_color, envelope_value, envelope_y_fraction, format_envelope_value};
use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, BG, LANE, PANEL, RAISED, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID, with_alpha};
use crate::ui::widgets::{self, switch, text_pill};

const TOP_H: f32 = 34.0;
const LANE_H: f32 = 64.0;
const LANE_GAP: f32 = 4.0;
const STRIP_H: f32 = 13.0;
const PARAM_COLOR: Color32 = Color32::from_rgb(0xae, 0x81, 0xff);
const TEMPO_COLOR: Color32 = Color32::from_rgb(0xe8, 0x5d, 0x75);

/// One lane: which envelope, its name and colour.
struct Lane {
    at: EnvRef,
    label: String,
    color: Color32,
    kind: Option<EnvelopeKind>,
}

#[derive(Clone, Copy)]
struct Axis {
    left: f32,
    start: f64,
    pps: f64,
}

impl Axis {
    fn x(&self, t: f64) -> f32 {
        self.left + ((t - self.start) * self.pps) as f32
    }

    fn t(&self, x: f32) -> f64 {
        self.start + (x - self.left) as f64 / self.pps
    }
}

/// Where values sit vertically in a lane: the track kinds as the arrange view draws them, FX
/// parameters linearly over their range.
#[derive(Clone, Copy)]
struct Vertical {
    kind: Option<EnvelopeKind>,
    range: (f64, f64),
    area: Rect,
}

impl Vertical {
    fn fraction(&self, v: f64) -> f32 {
        match self.kind {
            Some(k) => envelope_y_fraction(k, v),
            None => {
                let span = self.range.1 - self.range.0;
                if span.abs() > 1e-12 { ((v - self.range.0) / span) as f32 } else { 0.5 }
            }
        }
    }

    fn y(&self, v: f64) -> f32 {
        self.area.bottom() - self.fraction(v).clamp(-0.1, 1.1) * self.area.height()
    }

    fn value(&self, y: f32) -> f64 {
        let f = ((self.area.bottom() - y) / self.area.height()).clamp(0.0, 1.0);
        match self.kind {
            Some(k) => envelope_value(k, f),
            None => self.range.0 + f as f64 * (self.range.1 - self.range.0),
        }
    }

    fn text(&self, v: f64) -> String {
        match self.kind {
            Some(k) => format_envelope_value(k, v),
            None => format!("{v:.3}"),
        }
    }
}

fn track_ref(id: TrackId) -> TrackRef {
    TrackRef::Id(id.0)
}

fn lanes(app: &ScytheApp, id: TrackId) -> Vec<Lane> {
    let Some(t) = app.project.track(id) else { return Vec::new() };
    let mut out = Vec::new();
    for e in &t.envelopes {
        let at = EnvRef { track: track_ref(id), envelope: e.kind.name().into(), send: None, item: None };
        out.push(Lane { at, label: e.kind.label().into(), color: envelope_color(e.kind), kind: Some(e.kind) });
    }
    for (s, send) in t.sends.iter().enumerate() {
        let dest = app.project.track(send.dest).map_or_else(|| format!("send {}", s + 1), |d| d.name.clone());
        for e in &send.envelopes {
            let at = EnvRef { track: track_ref(id), envelope: e.kind.name().into(), send: Some(s), item: None };
            out.push(Lane {
                at,
                label: format!("{} to {dest}", e.kind.label()),
                color: envelope_color(e.kind),
                kind: Some(e.kind),
            });
        }
    }
    for (fx, slot) in t.fx.iter().enumerate() {
        for p in slot.parameter_envelopes() {
            let Some(param) = p.param() else { continue };
            let at = EnvRef { track: track_ref(id), envelope: format!("fx:{fx}:{param}"), send: None, item: None };
            let label = if p.label.is_empty() {
                format!("{} #{param}", scythe_engine::stock::shown_name(&slot.name))
            } else {
                p.label.clone()
            };
            out.push(Lane { at, label, color: PARAM_COLOR, kind: None });
        }
    }
    // The selected item's take envelopes, when it is on this track.
    if let Some((tid, item)) = app.sel.item.and_then(|i| app.project.find_item(i)).filter(|(t, _)| *t == id) {
        let _ = tid;
        let name = if item.name().is_empty() { "item".to_owned() } else { item.name().to_owned() };
        for e in item.take().map(|t| t.envelopes.clone()).unwrap_or_default() {
            let envelope = if e.kind == EnvelopeKind::Pitch { "pitch".to_owned() } else { e.kind.name().to_owned() };
            let at = EnvRef { track: track_ref(id), envelope, send: None, item: Some(item.id.0) };
            let label = format!("Take {} · {name}", e.kind.label().to_lowercase());
            out.push(Lane { at, label, color: envelope_color(e.kind), kind: Some(e.kind) });
        }
    }
    if app.view.tempo_lane || !app.project.tempo.points.is_empty() {
        let at = EnvRef { track: track_ref(id), envelope: "tempo".into(), send: None, item: None };
        out.insert(0, Lane { at, label: "Tempo map".into(), color: TEMPO_COLOR, kind: None });
    }
    out
}

/// Runs an envelope edit through the control API's envelope module, reporting failures.
fn change(
    app: &mut ScytheApp,
    at: &EnvRef,
    label: &str,
    gesture: bool,
    f: impl FnOnce(&mut Envelope, &mut scythe_core::Project),
) {
    if let Err(e) = api::change(&mut app.session, at, label, gesture, |env, p| {
        f(env, p);
        Ok(())
    }) {
        app.session.notify(e.message);
    }
}

fn run(app: &mut ScytheApp, command: api::EnvelopeCommand) {
    if let Err(e) = scythe_session::api::execute(&mut app.session, scythe_session::api::Command::Envelope(command)) {
        app.session.notify(e.message);
    }
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    ui.painter().rect_filled(rect, CornerRadius::same(18), PANEL);
    let inner = rect.shrink(8.0);
    let lanes_left = inner.left() + HEADER_W + 8.0;
    let axis = Axis { left: lanes_left, start: app.view.start, pps: app.view.px_per_sec };
    let top = Rect::from_min_size(inner.min, vec2(inner.width(), TOP_H));
    let Some(track) = app.sel.track.filter(|t| app.project.track(*t).is_some()) else {
        header(app, ui, top, None);
        ui.painter().text(
            inner.center(),
            Align2::CENTER_CENTER,
            "Select a track to see its envelopes",
            theme::regular(12.0),
            TEXT_DIM,
        );
        return;
    };
    header(app, ui, top, Some(track));
    let all = lanes(app, track);
    if all.is_empty() {
        ui.painter().text(
            inner.center(),
            Align2::CENTER_CENTER,
            "No envelopes yet: add one with + Envelope",
            theme::regular(12.0),
            TEXT_DIM,
        );
        return;
    }
    let body = Rect::from_min_max(pos2(inner.left(), top.bottom() + 6.0), inner.max);
    let scroll_id = Id::unique("envelope-lanes-scroll");
    let content = all.len() as f32 * (LANE_H + LANE_GAP);
    let mut scroll: f32 = ui.data(|d| d.get_temp(scroll_id)).unwrap_or(0.0);
    if ui.rect_contains_pointer(body) && !ui.input(|i| i.modifiers.command) {
        scroll -= ui.input(|i| i.smooth_scroll_delta.y);
    }
    scroll = scroll.clamp(0.0, (content - body.height()).max(0.0));
    ui.data_mut(|d| d.insert_temp(scroll_id, scroll));
    let clip = ui.painter().with_clip_rect(body);
    for (i, lane) in all.iter().enumerate() {
        let top = body.top() + i as f32 * (LANE_H + LANE_GAP) - scroll;
        let row = Rect::from_min_size(pos2(body.left(), top), vec2(body.width(), LANE_H));
        if row.bottom() < body.top() || row.top() > body.bottom() {
            continue;
        }
        let Ok(env) = api::envelope(&app.project, &lane.at) else { continue };
        lane_header(app, ui, &clip, Rect::from_min_size(row.min, vec2(HEADER_W, LANE_H)).intersect(body), lane, &env);
        let area = Rect::from_min_max(pos2(lanes_left, row.top()), row.max);
        clip.rect_filled(area, CornerRadius::same(10), LANE);
        let painter = clip.with_clip_rect(area.intersect(body));
        lane_body(app, ui, &painter, area, axis, lane, &env);
    }
}

fn header(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, track: Option<TrackId>) {
    let p = ui.painter().clone();
    let name =
        track.and_then(|t| app.project.track(t)).map_or("Envelopes".to_owned(), |t| format!("Envelopes · {}", t.name));
    p.text(pos2(rect.left() + 6.0, rect.center().y), Align2::LEFT_CENTER, name, theme::semibold(13.0), TEXT);
    // The global override: one mode for every track, or every envelope bypassed.
    let modes: [(Option<GlobalAutomation>, &str, &str); 8] = [
        (None, "Off", "No override: every track uses its own automation mode"),
        (Some(GlobalAutomation::Mode(AutoMode::TrimRead)), "Trim", "All automation in trim/read mode"),
        (Some(GlobalAutomation::Mode(AutoMode::Read)), "Read", "All automation in read mode"),
        (Some(GlobalAutomation::Mode(AutoMode::Touch)), "Touch", "All automation in touch mode"),
        (Some(GlobalAutomation::Mode(AutoMode::Latch)), "Latch", "All automation in latch mode"),
        (Some(GlobalAutomation::Mode(AutoMode::LatchPreview)), "Preview", "All automation in latch preview mode"),
        (Some(GlobalAutomation::Mode(AutoMode::Write)), "Write", "All automation in write mode"),
        (Some(GlobalAutomation::Bypass), "Bypass", "Bypass all automation"),
    ];
    let mut x = rect.right();
    for (over, label, tip) in modes.iter().rev() {
        let w = widgets::text_width(ui, label, theme::semibold(11.0)) + 18.0;
        x -= w + 4.0;
        let r = Rect::from_min_size(pos2(x, rect.center().y - 11.0), vec2(w, 22.0));
        let on = app.project.global_automation == *over;
        let (bg, fg) = match over {
            Some(GlobalAutomation::Bypass) if on => (theme::YELLOW, BG),
            _ if on => (ACCENT, BG),
            _ => (RAISED_HI, TEXT_MID),
        };
        if text_pill(ui, r, label, theme::semibold(11.0), bg, fg).on_hover_text(*tip).clicked() {
            let over = *over;
            app.session.edit("Global automation override", |p| p.global_automation = over);
        }
    }
    p.text(pos2(x - 8.0, rect.center().y), Align2::RIGHT_CENTER, "Override", theme::regular(11.0), TEXT_DIM);
    let Some(track) = track else { return };
    if !app.session.latch_preview().is_empty() {
        let r = Rect::from_min_size(pos2(rect.left() + 200.0, rect.center().y - 11.0), vec2(150.0, 22.0));
        let resp = text_pill(ui, r, "Write preview values", theme::semibold(11.0), theme::PINK, BG)
            .on_hover_text("Write the values the latch preview pass remembered");
        egui::Popup::menu(&resp).show(|ui| {
            for (label, span) in
                [("Time selection", "time_selection"), ("Cursor to end", "to_end"), ("Start to cursor", "to_start")]
            {
                if ui.button(label).clicked() {
                    run(app, api::EnvelopeCommand::LatchPreviewApply { span: span.into() });
                }
            }
        });
    }
    let add = Rect::from_min_size(pos2(rect.left() + 360.0, rect.center().y - 11.0), vec2(96.0, 22.0));
    let resp = text_pill(ui, add, "+ Envelope", theme::semibold(11.0), RAISED_HI, TEXT);
    egui::Popup::menu(&resp).show(|ui| add_menu(app, ui, track));
}

/// Envelopes the track does not have yet: its own kinds and every effect parameter.
fn add_menu(app: &mut ScytheApp, ui: &mut Ui, track: TrackId) {
    let Some(t) = app.project.track(track).cloned() else { return };
    if !app.view.tempo_lane && app.project.tempo.points.is_empty() && ui.button("Tempo map").clicked() {
        app.view.tempo_lane = true;
    }
    if let Some((_, item)) = app.sel.item.and_then(|i| app.project.find_item(i)).filter(|(t, _)| *t == track) {
        let id = item.id;
        let have: Vec<EnvelopeKind> =
            item.take().map(|t| t.envelopes.iter().map(|e| e.kind).collect()).unwrap_or_default();
        for (kind, name) in [
            (EnvelopeKind::Volume, "volume"),
            (EnvelopeKind::Pan, "pan"),
            (EnvelopeKind::Mute, "mute"),
            (EnvelopeKind::Pitch, "pitch"),
        ] {
            if have.contains(&kind) || !ui.button(format!("Take {}", kind.label().to_lowercase())).clicked() {
                continue;
            }
            let at = EnvRef { track: track_ref(track), envelope: name.into(), send: None, item: Some(id.0) };
            let start = app.project.find_item(id).map_or(0.0, |(_, i)| i.position);
            change(app, &at, "Add take envelope", false, |env, _| {
                env.points.push(EnvPoint::new(start, kind.neutral()));
            });
        }
        ui.separator();
    }
    for kind in EnvelopeKind::ALL.into_iter().filter(|k| t.envelope(*k).is_none()) {
        if ui.button(kind.label()).clicked() {
            let at = EnvRef { track: track_ref(track), envelope: kind.name().into(), send: None, item: None };
            let base = if kind == EnvelopeKind::Volume { t.volume } else { kind.neutral() };
            change(app, &at, "Add envelope", false, |env, _| {
                env.points.push(EnvPoint::new(0.0, base));
            });
        }
    }
    for (fx, slot) in t.fx.iter().enumerate() {
        let Some(inst) = app.engine.fx().instance(Some(track), slot, fx) else { continue };
        let params = inst.with(|e| e.params()).unwrap_or_default();
        if params.is_empty() {
            continue;
        }
        ui.menu_button(scythe_engine::stock::shown_name(&slot.name), |ui| {
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for (param, p) in params.iter().enumerate() {
                    if slot.parameter_envelope(&param.to_string()).is_some() || !ui.button(&p.name).clicked() {
                        continue;
                    }
                    let at = EnvRef {
                        track: track_ref(track),
                        envelope: format!("fx:{fx}:{param}"),
                        send: None,
                        item: None,
                    };
                    let value = api::param_info(&app.session, Target::Track(track), fx, param).map_or(0.0, |i| i.3);
                    change(app, &at, "Add FX parameter envelope", false, |env, _| {
                        env.points.push(EnvPoint::new(0.0, value));
                    });
                }
            });
        });
    }
}

fn lane_header(app: &mut ScytheApp, ui: &mut Ui, painter: &egui::Painter, r: Rect, lane: &Lane, env: &Envelope) {
    if r.height() < 20.0 {
        return;
    }
    painter.rect_filled(r, CornerRadius::same(10), RAISED);
    painter.rect_filled(
        Rect::from_min_size(r.min + vec2(8.0, 10.0), vec2(4.0, r.height() - 20.0)),
        CornerRadius::same(2),
        lane.color,
    );
    let name = widgets::truncate(ui, &lane.label, &theme::semibold(12.0), HEADER_W - 70.0);
    painter.text(
        pos2(r.left() + 20.0, r.top() + 16.0),
        Align2::LEFT_CENTER,
        name,
        theme::semibold(12.0),
        if env.active { TEXT } else { TEXT_DIM },
    );
    let sw = Rect::from_min_size(pos2(r.right() - 46.0, r.top() + 6.0), vec2(36.0, 20.0));
    if r.contains_rect(sw)
        && switch(ui, sw, env.active, ACCENT).on_hover_text("Active (off bypasses the envelope)").clicked()
    {
        let active = !env.active;
        change(app, &lane.at, "Bypass envelope", false, |e, _| e.active = active);
    }
    let pill = |k: f32| Rect::from_min_size(pos2(r.left() + 20.0 + k * 52.0, r.bottom() - 28.0), vec2(48.0, 20.0));
    if !r.contains_rect(pill(3.0)) {
        return;
    }
    let tempo = lane.at.envelope == "tempo";
    let (abg, afg) = if env.armed { (theme::PINK, BG) } else { (RAISED_HI, TEXT_MID) };
    if !tempo
        && text_pill(ui, pill(0.0), "Arm", theme::semibold(10.0), abg, afg)
            .on_hover_text("Armed envelopes are written by touch, latch and write")
            .clicked()
    {
        let armed = !env.armed;
        change(app, &lane.at, "Arm envelope", false, |e, _| e.armed = armed);
    }
    if let (Some(send), TrackRef::Id(tid)) = (lane.at.send, &lane.at.track) {
        send_mode_pill(app, ui, pill(1.0), TrackId(*tid), send);
    }
    if lane.kind.is_some() && lane.at.send.is_none() && lane.at.item.is_none() {
        let (vbg, vfg) = if env.visible { (ACCENT, BG) } else { (RAISED_HI, TEXT_MID) };
        if text_pill(ui, pill(1.0), "Show", theme::semibold(10.0), vbg, vfg)
            .on_hover_text("Draw over the track in the arrange view")
            .clicked()
        {
            let visible = !env.visible;
            change(app, &lane.at, "Show envelope", false, |e, _| e.visible = visible);
        }
    }
    let resp = text_pill(ui, pill(2.0), "Shape", theme::semibold(10.0), RAISED_HI, TEXT_MID)
        .on_hover_text("Shape of the points (in the time selection, or all)");
    egui::Popup::menu(&resp).show(|ui| {
        if let Some(shape) = shape_menu(ui, None) {
            let range = app.project.time_selection.map_or((f64::NEG_INFINITY, f64::INFINITY), |s| (s.start, s.end));
            change(app, &lane.at, "Set envelope point shape", false, |e, _| {
                for p in e.points.iter_mut().filter(|p| p.time >= range.0 && p.time <= range.1) {
                    p.shape = shape;
                }
            });
        }
    });
    if tempo {
        return;
    }
    let resp = text_pill(ui, pill(3.0), "Item", theme::semibold(10.0), RAISED_HI, TEXT_MID)
        .on_hover_text("Insert an automation item at the time selection (or a bar at the cursor)");
    egui::Popup::menu(&resp).show(|ui| {
        let (pos, len) = match app.project.time_selection {
            Some(s) => (s.start, s.length()),
            None => (app.position().max(0.0), app.project.tempo.bar_seconds_at(app.position().max(0.0))),
        };
        if ui.button("From the envelope").clicked() {
            run(
                app,
                api::EnvelopeCommand::ItemAdd {
                    at: lane.at.clone(),
                    position: pos,
                    length: len,
                    pool: None,
                    lfo: None,
                },
            );
        }
        lfo_menu(ui, |lfo| {
            run(
                app,
                api::EnvelopeCommand::ItemAdd {
                    at: lane.at.clone(),
                    position: pos,
                    length: len,
                    pool: None,
                    lfo: Some(lfo),
                },
            );
        });
        ui.separator();
        if ui.button("Remove envelope").clicked() {
            run(app, api::EnvelopeCommand::Remove { at: lane.at.clone() });
        }
    });
}

/// The shape entries; answers the one picked. `current` marks the point's own.
fn shape_menu(ui: &mut Ui, current: Option<PointShape>) -> Option<PointShape> {
    let labels = ["Linear", "Square", "Slow start/end", "Fast start", "Fast end", "Bezier"];
    let mut picked = None;
    for (code, label) in labels.iter().enumerate() {
        let on = current.is_some_and(|c| c.rpp_code() == code as i64);
        if ui.selectable_label(on, *label).clicked() {
            let tension = match current {
                Some(PointShape::Bezier(t)) => t,
                _ => 0.0,
            };
            picked = Some(PointShape::from_rpp(code as i64, tension));
        }
    }
    picked
}

fn lfo_menu(ui: &mut Ui, mut pick: impl FnMut(api::LfoSpec)) {
    ui.menu_button("LFO", |ui| {
        for (shape, label) in [
            ("sine", "Sine"),
            ("triangle", "Triangle"),
            ("square", "Square"),
            ("saw_up", "Saw up"),
            ("saw_down", "Saw down"),
        ] {
            ui.menu_button(label, |ui| {
                for cycles in [1.0, 2.0, 4.0, 8.0, 16.0] {
                    if ui.button(format!("{cycles} cycle{}", if cycles > 1.0 { "s" } else { "" })).clicked() {
                        pick(api::LfoSpec { shape: shape.into(), cycles, phase: 0.0 });
                    }
                }
            });
        }
    });
}

fn lane_body(
    app: &mut ScytheApp,
    ui: &mut Ui,
    painter: &egui::Painter,
    area: Rect,
    axis: Axis,
    lane: &Lane,
    env: &Envelope,
) {
    let inner = area.shrink2(vec2(0.0, 8.0));
    let vert = Vertical { kind: lane.kind, range: env.range.unwrap_or((0.0, 1.0)), area: inner };
    let alpha = if env.active { 255 } else { 110 };
    let key = (lane.at.envelope.clone(), lane.at.send);

    // Automation items under the curve, with their strip for moving and their right edge for
    // resizing.
    let timeline = app.project.tempo.timeline();
    let scale = Scale::of(env);
    for (k, item) in env.items.iter().enumerate() {
        let (x0, x1) = (axis.x(item.position), axis.x(item.end()));
        if x1 < area.left() || x0 > area.right() {
            continue;
        }
        let r = Rect::from_min_max(pos2(x0, area.top() + 2.0), pos2(x1, area.bottom() - 2.0));
        let tint = if item.selected { 60 } else { 32 };
        painter.rect_filled(r, CornerRadius::same(6), with_alpha(lane.color, tint));
        painter.rect_stroke(
            r,
            CornerRadius::same(6),
            Stroke::new(1.0, with_alpha(lane.color, 160)),
            StrokeKind::Inside,
        );
        let pool = app.project.automation_pools.iter().find(|p| p.id == item.pool).cloned();
        let copies = count_pool(app, item.pool);
        let mut label = pool.as_ref().map_or_else(|| format!("pool {}", item.pool), |p| p.name.clone());
        if copies > 1 {
            label.push_str(&format!(" · pooled ×{copies}"));
        }
        if item.looped {
            label.push_str(" · loop");
        }
        painter.text(r.left_top() + vec2(6.0, 7.0), Align2::LEFT_CENTER, label, theme::mono(9.0), TEXT_MID);
        let strip = Rect::from_min_max(r.min, pos2(r.right() - 6.0, r.top() + STRIP_H));
        let strip_resp =
            ui.interact(strip.intersect(area), Id::unique(("ai-strip", key.clone(), k)), Sense::click_and_drag());
        let edge = Rect::from_min_max(pos2(r.right() - 6.0, r.top()), r.max);
        let edge_resp = ui.interact(edge.intersect(area), Id::unique(("ai-edge", key.clone(), k)), Sense::drag());
        if edge_resp.hovered() || edge_resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        let dt = |resp: &egui::Response| resp.drag_delta().x as f64 / axis.pps;
        if strip_resp.dragged() && dt(&strip_resp) != 0.0 {
            let d = dt(&strip_resp);
            change(app, &lane.at, "Move automation item", true, |e, _| {
                if let Some(i) = e.items.get_mut(k) {
                    i.position = (i.position + d).max(0.0);
                }
            });
        }
        if edge_resp.dragged() && dt(&edge_resp) != 0.0 {
            let d = dt(&edge_resp);
            change(app, &lane.at, "Resize automation item", true, |e, _| {
                if let Some(i) = e.items.get_mut(k) {
                    i.length = (i.length + d).max(0.01);
                }
            });
        }
        if strip_resp.drag_stopped() || edge_resp.drag_stopped() {
            app.end_gesture();
        }
        if strip_resp.clicked() {
            let selected = !item.selected;
            change(app, &lane.at, "Select automation item", false, |e, _| {
                if let Some(i) = e.items.get_mut(k) {
                    i.selected = selected;
                }
            });
        }
        strip_resp.context_menu(|ui| item_menu(app, ui, lane, k, item));

        // The pool's points as the item plays them.
        let Some(pool) = pool else { continue };
        for (pi, t, shown) in item.visible_points(&pool, &timeline) {
            let c = pos2(axis.x(t), vert.y(scale.denormalize(shown)));
            if !area.contains(c) {
                continue;
            }
            let id = Id::unique(("ai-point", key.clone(), k, pi, (t * 1e6) as i64));
            let resp = ui.interact(Rect::from_center_size(c, vec2(10.0, 10.0)), id, Sense::click_and_drag());
            let hot = resp.hovered() || resp.dragged();
            painter.circle_stroke(c, if hot { 4.5 } else { 3.0 }, Stroke::new(1.5, with_alpha(lane.color, alpha)));
            if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.dragged()) {
                let value = scale.normalize(vert.value(pos.y));
                let at_t = axis.t(pos.x).clamp(item.position, item.end());
                let item = item.clone();
                pool_edit(app, item.pool, true, |p| {
                    let (qn, n) = item.to_pool(at_t, value, p, &timeline);
                    if let Some(pt) = p.points.get_mut(pi) {
                        (pt.time, pt.value) = (qn, n.clamp(0.0, 1.0));
                    }
                    p.points.sort_by(|a, b| a.time.total_cmp(&b.time));
                });
            }
            if resp.drag_stopped() {
                app.end_gesture();
            }
            if resp.secondary_clicked() {
                pool_edit(app, item.pool, false, |p| {
                    if pi < p.points.len() {
                        p.points.remove(pi);
                    }
                });
            }
        }
    }

    // The curve, items included.
    let mut line = Vec::new();
    let mut x = area.left().max(painter.clip_rect().left());
    let right = area.right().min(painter.clip_rect().right());
    while x <= right {
        if let Some(v) = env.value_at(axis.t(x)) {
            line.push(pos2(x, vert.y(v)));
        }
        x += 2.0;
    }
    painter.add(Shape::line(line, Stroke::new(1.5, with_alpha(lane.color, alpha))));

    // The envelope's own points.
    for (i, p) in env.points.iter().enumerate() {
        let c = pos2(axis.x(p.time), vert.y(p.value));
        if !area.contains(pos2(c.x, area.center().y)) {
            continue;
        }
        let id = Id::unique(("lane-point", key.clone(), i));
        let resp = ui.interact(Rect::from_center_size(c, vec2(12.0, 12.0)), id, Sense::click_and_drag());
        let hot = resp.hovered() || resp.dragged();
        painter.circle_filled(c, if hot { 5.0 } else { 3.5 }, with_alpha(lane.color, alpha));
        if hot {
            let text = format!("{} · {}", vert.text(p.value), p.shape.name().replace('_', " "));
            painter.text(c + vec2(8.0, -10.0), Align2::LEFT_BOTTOM, text, theme::mono(10.0), TEXT);
        }
        if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.dragged()) {
            let bypass = ui.ctx().input(|inp| inp.modifiers.shift);
            let prev = i.checked_sub(1).map(|j| env.points[j].time).unwrap_or(0.0);
            let next = env.points.get(i + 1).map_or(f64::MAX, |q| q.time);
            let t = app.snap(axis.t(pos.x).max(0.0), bypass).clamp(prev, next);
            let v = vert.value(pos.y);
            change(app, &lane.at, "Move envelope point", true, |e, _| {
                if let Some(pt) = e.points.get_mut(i) {
                    (pt.time, pt.value) = (t, v);
                }
            });
        }
        if resp.drag_stopped() {
            app.end_gesture();
        }
        let shape = p.shape;
        resp.context_menu(|ui| {
            if let Some(s) = shape_menu(ui, Some(shape)) {
                change(app, &lane.at, "Set envelope point shape", false, |e, _| {
                    if let Some(pt) = e.points.get_mut(i) {
                        pt.shape = s;
                    }
                });
            }
            if let PointShape::Bezier(mut t) = shape
                && ui.add(egui::Slider::new(&mut t, -1.0..=1.0).text("Tension")).changed()
            {
                change(app, &lane.at, "Bezier tension", true, |e, _| {
                    if let Some(pt) = e.points.get_mut(i) {
                        pt.shape = PointShape::Bezier(t);
                    }
                });
            }
            ui.separator();
            if ui.button("Delete point").clicked() {
                change(app, &lane.at, "Delete envelope point", false, |e, _| {
                    if i < e.points.len() {
                        e.points.remove(i);
                    }
                });
                ui.close();
            }
        });
    }

    // Double-click adds a point: into the pool of the item under the pointer, else to the
    // envelope.
    let double = ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
    if double
        && let Some(pos) = ui.ctx().pointer_hover_pos()
        && area.contains(pos)
        && pos.y > area.top() + STRIP_H
    {
        let bypass = ui.ctx().input(|i| i.modifiers.shift);
        let t = app.snap(axis.t(pos.x).max(0.0), bypass);
        let v = vert.value(pos.y);
        match env.items.iter().find(|i| t >= i.position && t < i.end()).cloned() {
            Some(item) => {
                let n = scale.normalize(v);
                pool_edit(app, item.pool, false, |p| {
                    let (qn, stored) = item.to_pool(t, n, p, &timeline);
                    p.points.push(EnvPoint::new(qn, stored.clamp(0.0, 1.0)));
                    p.points.sort_by(|a, b| a.time.total_cmp(&b.time));
                });
            }
            None => change(app, &lane.at, "Add envelope point", false, |e, _| {
                e.points.retain(|p| (p.time - t).abs() > 1e-9);
                e.points.push(EnvPoint::new(t, v));
                e.sort();
            }),
        }
    }
}

fn item_menu(
    app: &mut ScytheApp,
    ui: &mut Ui,
    lane: &Lane,
    k: usize,
    item: &scythe_core::automation_item::AutomationItem,
) {
    let at = lane.at.clone();
    let edit = |app: &mut ScytheApp, looped: Option<bool>, baseline: Option<f64>, amplitude: Option<f64>| {
        run(
            app,
            api::EnvelopeCommand::ItemEdit {
                at: at.clone(),
                index: k,
                position: None,
                length: None,
                start_offset: None,
                rate: None,
                baseline,
                amplitude,
                looped,
                transition: None,
                selected: None,
            },
        )
    };
    if ui.selectable_label(item.looped, "Loop").clicked() {
        edit(app, Some(!item.looped), None, None);
    }
    let (mut base, mut amp) = (item.baseline, item.amplitude);
    if ui.add(egui::Slider::new(&mut base, 0.0..=1.0).text("Baseline")).changed() {
        edit(app, None, Some(base), None);
    }
    if ui.add(egui::Slider::new(&mut amp, -1.0..=1.0).text("Amplitude")).changed() {
        edit(app, None, None, Some(amp));
    }
    ui.separator();
    if ui.button("Duplicate").clicked() {
        run(app, api::EnvelopeCommand::ItemDuplicate { at: at.clone(), index: k, pooled: false });
    }
    if ui.button("Duplicate pooled").clicked() {
        run(app, api::EnvelopeCommand::ItemDuplicate { at: at.clone(), index: k, pooled: true });
    }
    if ui.button("Unpool").clicked() {
        run(app, api::EnvelopeCommand::ItemUnpool { at: at.clone(), index: k });
    }
    let pool = item.pool;
    lfo_menu(ui, |lfo| {
        run(app, api::EnvelopeCommand::Pool { pool, points: None, lfo: Some(lfo), length_qn: None, name: None });
    });
    ui.separator();
    if ui.button("Delete").clicked() {
        run(app, api::EnvelopeCommand::ItemDelete { at: at.clone(), index: k, keep_points: false });
    }
    if ui.button("Delete, keep points").clicked() {
        run(app, api::EnvelopeCommand::ItemDelete { at, index: k, keep_points: true });
    }
}

/// Edits a pool's points (every pooled copy follows).
fn pool_edit(
    app: &mut ScytheApp,
    pool: i64,
    gesture: bool,
    f: impl FnOnce(&mut scythe_core::automation_item::AutomationPool),
) {
    let apply = |p: &mut scythe_core::Project| {
        if let Some(pl) = p.automation_pools.iter_mut().find(|pl| pl.id == pool) {
            f(pl);
            pl.lfo = None;
        }
        p.resolve_automation_items();
    };
    if gesture {
        app.session.gesture("Edit automation item points", apply);
    } else {
        app.session.edit("Edit automation item points", apply);
    }
}

fn count_pool(app: &ScytheApp, pool: i64) -> usize {
    let p = &app.project;
    std::iter::once(&p.master)
        .chain(&p.tracks)
        .map(|t| {
            let own = t
                .envelopes
                .iter()
                .chain(t.sends.iter().flat_map(|s| &s.envelopes))
                .flat_map(|e| &e.items)
                .filter(|i| i.pool == pool)
                .count();
            let fx =
                t.fx.iter()
                    .flat_map(|s| s.parameter_envelopes())
                    .flat_map(|e| e.envelope.items)
                    .filter(|i| i.pool == pool)
                    .count();
            own + fx
        })
        .sum()
}

/// The automation mode of a send's envelopes: the track's, or one of its own (REAPER's per-send
/// mode, `AUXRECV` field 12).
fn send_mode_pill(app: &mut ScytheApp, ui: &mut Ui, r: Rect, track: TrackId, send: usize) {
    let Some(t) = app.project.track(track) else { return };
    let Some(own) = t.sends.get(send).map(|s| s.auto_mode()) else { return };
    let label = own.map_or("Track", |m| match m {
        AutoMode::TrimRead => "Trim",
        AutoMode::Read => "Read",
        AutoMode::Touch => "Touch",
        AutoMode::Latch => "Latch",
        AutoMode::LatchPreview => "Preview",
        AutoMode::Write => "Write",
    });
    let (bg, fg) = if own.is_some() { (ACCENT, BG) } else { (RAISED_HI, TEXT_MID) };
    let effective = app.project.send_auto_mode(t, send).label();
    let resp = text_pill(ui, r, label, theme::semibold(10.0), bg, fg)
        .on_hover_text(format!("The send's automation mode, now {effective}: the track's, or one of its own"));
    egui::Popup::menu(&resp).show(|ui| {
        let mut pick = None;
        if ui.selectable_label(own.is_none(), "Use the track's mode").clicked() {
            pick = Some(None);
        }
        for m in AutoMode::ALL {
            if ui.selectable_label(own == Some(m), m.label()).clicked() {
                pick = Some(Some(m));
            }
        }
        if let Some(mode) = pick {
            let name = mode.map_or("track".to_owned(), |m| m.name().to_owned());
            run(app, api::EnvelopeCommand::Mode { track: Some(track_ref(track)), mode: Some(name), send: Some(send) });
            ui.close();
        }
    });
}
