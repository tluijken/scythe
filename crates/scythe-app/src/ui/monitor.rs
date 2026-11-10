//! The monitor mixes page: one card per musician's in-ear mix, with a fader per source, a
//! "more me" knob, the mix's output and its meter, plus the wizards that create mixes and
//! SWS-style cue buses. The mixes themselves live in `scythe_core::monitor`.

use std::path::PathBuf;

use egui::{Align2, Color32, CornerRadius, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use scythe_core::monitor::{CueBusOptions, MORE_ME_RANGE, MonitorMix, MonitorOutput, MonitorSource, limiter_slot};
use scythe_core::units::format_db;
use scythe_core::{Project, SendMode, TrackId};

use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, BG, GREEN, PANEL, RAISED, RAISED_HI, TEXT, TEXT_DIM, YELLOW, track_color};
use crate::ui::arrange::meter_fraction;
use crate::ui::mixer::{fader_gain, fader_position};
use crate::ui::widgets::{self, switch, text_pill};
use crate::ui::window::{WindowKind, scythe_window};

const CARD_W: f32 = 320.0;
const CARD_GAP: f32 = 12.0;
const ROW_H: f32 = 40.0;
fn pick(o: MonitorOutput) -> crate::ui::channels::Pick {
    match o {
        MonitorOutput::Stereo(n) => (n as usize, false),
        MonitorOutput::Mono(n) => (n as usize, true),
    }
}

fn unpick((first, mono): crate::ui::channels::Pick) -> MonitorOutput {
    if mono { MonitorOutput::Mono(first as u32) } else { MonitorOutput::Stereo(first as u32) }
}

/// State of the "new monitor mix" wizard, which doubles as SWS's cue buss dialog.
#[derive(Debug, Clone)]
pub struct Wizard {
    /// SWS's "Create cue buss from track selection" rather than a musician's mix.
    pub cue_bus: bool,
    /// The musician, or the cue bus name.
    pub name: String,
    pub sources: Vec<TrackId>,
    /// The musician's own sources, which "more me" raises.
    pub own: Vec<TrackId>,
    pub mode: SendMode,
    pub volumes_from_tracks: bool,
    pub pans_from_tracks: bool,
    pub output: Option<MonitorOutput>,
    pub limiter: bool,
    pub template: Option<PathBuf>,
    /// An existing mix to start from instead.
    pub copy_from: Option<TrackId>,
}

impl Wizard {
    pub fn monitor(p: &Project) -> Self {
        Self {
            cue_bus: false,
            name: format!("Musician {}", p.monitor_mixes().len() + 1),
            sources: p.monitor_candidates(),
            own: Vec::new(),
            mode: SendMode::PostFx,
            volumes_from_tracks: false,
            pans_from_tracks: false,
            output: Some(MonitorOutput::Stereo(p.next_free_output(2))),
            limiter: true,
            template: None,
            copy_from: None,
        }
    }

    /// SWS's dialog: the selected track (or every candidate) feeding a bus named "Cue Buss".
    pub fn cue_bus(p: &Project, selected: Option<TrackId>) -> Self {
        Self {
            cue_bus: true,
            name: "Cue Buss".to_owned(),
            sources: selected.map_or_else(|| p.monitor_candidates(), |t| vec![t]),
            limiter: false,
            output: None,
            ..Self::monitor(p)
        }
    }

    fn options(&self) -> Result<CueBusOptions, String> {
        let template = match &self.template {
            Some(path) => {
                let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
                let tracks = scythe_core::rpp::tracks_from_template(&text).map_err(|e| e.to_string())?;
                Some(tracks.into_iter().next().ok_or("The template holds no track")?)
            }
            None => None,
        };
        Ok(CueBusOptions {
            name: self.name.clone(),
            mode: self.mode,
            volumes_from_tracks: self.volumes_from_tracks,
            pans_from_tracks: self.pans_from_tracks,
            output: self.output,
            send_to_master: self.cue_bus && self.output.is_none(),
            template,
            fx: if self.limiter { vec![limiter_slot()] } else { Vec::new() },
        })
    }
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(18), PANEL);
    let inner = rect.shrink2(vec2(16.0, 14.0));
    let mixes = app.project.monitor_mixes();
    painter.text(inner.left_top() + vec2(0.0, 10.0), Align2::LEFT_CENTER, "Monitor mixes", theme::semibold(15.0), TEXT);
    let sub = match mixes.len() {
        0 => "In-ear mixes for the band, one per musician".to_owned(),
        1 => "1 mix · each musician hears their own".to_owned(),
        n => format!("{n} mixes · each musician hears their own"),
    };
    painter.text(inner.left_top() + vec2(0.0, 30.0), Align2::LEFT_CENTER, sub, theme::regular(12.0), TEXT_DIM);
    let new = Rect::from_min_size(pos2(inner.right() - 120.0, inner.top()), vec2(120.0, 36.0));
    if text_pill(ui, new, "+ New mix", theme::semibold(13.0), ACCENT, BG)
        .on_hover_text("Create a monitor mix")
        .clicked()
    {
        app.view.monitor_wizard = Some(Wizard::monitor(&app.project));
    }
    let cue = Rect::from_min_size(pos2(new.left() - 8.0 - 110.0, inner.top()), vec2(110.0, 36.0));
    if text_pill(ui, cue, "Cue bus…", theme::semibold(13.0), RAISED, TEXT)
        .on_hover_text("Create cue buss from track selection, as SWS does")
        .clicked()
    {
        app.view.cue_dialog = Some(Wizard::cue_bus(&app.project, app.sel.track));
    }

    let area = Rect::from_min_max(pos2(inner.left(), inner.top() + 52.0), inner.max);
    if mixes.is_empty() {
        painter.text(
            area.center() - vec2(0.0, 30.0),
            Align2::CENTER_CENTER,
            "No monitor mixes yet",
            theme::semibold(14.0),
            theme::TEXT_MID,
        );
        let b = Rect::from_center_size(area.center() + vec2(0.0, 10.0), vec2(240.0, 40.0));
        if text_pill(ui, b, "Create the first monitor mix", theme::semibold(13.0), RAISED_HI, TEXT).clicked() {
            app.view.monitor_wizard = Some(Wizard::monitor(&app.project));
        }
        return;
    }
    let content_w = mixes.len() as f32 * (CARD_W + CARD_GAP) - CARD_GAP;
    let max_scroll = (content_w - area.width()).max(0.0);
    let scroll_id = Id::unique("monitor-scroll");
    let mut scroll: f32 = ui.data(|d| d.get_temp(scroll_id)).unwrap_or(0.0);
    if ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|p| area.contains(p)) {
        let d = ui.ctx().input(|i| i.smooth_scroll_delta);
        if d.x != 0.0 || ui.ctx().input(|i| i.modifiers.shift) {
            scroll -= if d.x != 0.0 { d.x } else { d.y };
        }
    }
    scroll = scroll.clamp(0.0, max_scroll);
    ui.data_mut(|d| d.insert_temp(scroll_id, scroll));
    let shown = area.expand(2.0);
    let clip = painter.with_clip_rect(shown);
    // The cards' own buttons and knobs draw through a child clipped to the page too, so a card
    // scrolled partly out of view does not paint past the panel.
    let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(shown));
    inner.set_clip_rect(shown.intersect(ui.clip_rect()));
    for (i, mix) in mixes.into_iter().enumerate() {
        let x = area.left() + i as f32 * (CARD_W + CARD_GAP) - scroll;
        let r = Rect::from_min_size(pos2(x, area.top()), vec2(CARD_W, area.height()));
        if r.right() < area.left() || r.left() > area.right() {
            continue;
        }
        card(app, &mut inner, &clip, r, area, mix);
    }
    if max_scroll > 0.0 {
        let track = Rect::from_min_max(pos2(area.left(), area.bottom() + 3.0), pos2(area.right(), area.bottom() + 6.0));
        painter.rect_filled(track, CornerRadius::same(255), BG);
        let w = track.width() * area.width() / content_w;
        let thumb = Rect::from_min_size(
            pos2(track.left() + (track.width() - w) * scroll / max_scroll, track.top()),
            vec2(w, 3.0),
        );
        painter.rect_filled(thumb, CornerRadius::same(255), RAISED_HI);
    }
}

fn card(app: &mut ScytheApp, ui: &mut Ui, painter: &egui::Painter, r: Rect, area: Rect, mix: MonitorMix) {
    let visible = r.intersect(area);
    let id = mix.track;
    let Some(track) = app.project.track(id).cloned() else { return };
    let color = track_color(track.color);
    painter.rect_filled(r, CornerRadius::same(16), RAISED);
    painter.rect_filled(
        Rect::from_min_size(r.min, vec2(r.width(), 4.0)).shrink2(vec2(16.0, 0.0)),
        CornerRadius::same(2),
        color,
    );
    let pad = 14.0;
    let left = r.left() + pad;
    let right = r.right() - pad;
    let mut y = r.top() + 24.0;

    // Name (double-click to rename), copy and delete.
    let rename_id = Id::unique(("monitor-rename", id.0));
    let renaming: Option<String> = ui.data(|d| d.get_temp(rename_id));
    let name_rect = Rect::from_min_max(pos2(left, y - 12.0), pos2(right - 90.0, y + 12.0));
    painter.circle_filled(pos2(left + 5.0, y), 5.0, color);
    match renaming {
        Some(mut text) => {
            let edit = ui.put(
                name_rect.with_min_x(left + 16.0).intersect(visible),
                egui::TextEdit::singleline(&mut text).font(theme::semibold(14.0)),
            );
            edit.request_focus();
            if edit.lost_focus() {
                ui.data_mut(|d| d.remove::<String>(rename_id));
                let name = text.trim().to_owned();
                if !name.is_empty() && name != mix.musician {
                    app.edit("Rename monitor mix", |p| {
                        p.rename_monitor_mix(id, &name);
                    });
                }
            } else {
                ui.data_mut(|d| d.insert_temp(rename_id, text));
            }
        }
        None => {
            let label = widgets::truncate(ui, &mix.musician, &theme::semibold(14.0), name_rect.width() - 16.0);
            painter.text(pos2(left + 16.0, y), Align2::LEFT_CENTER, label, theme::semibold(14.0), TEXT);
            let resp = ui
                .interact(name_rect.intersect(visible), Id::unique(("monitor-name", id.0)), Sense::click())
                .on_hover_text("Double-click to rename");
            if resp.double_clicked() {
                ui.data_mut(|d| d.insert_temp(rename_id, mix.musician.clone()));
            }
            if resp.clicked() {
                app.sel = crate::app::Selection { track: Some(id), item: None };
            }
        }
    }
    let copy = Rect::from_min_size(pos2(right - 86.0, y - 13.0), vec2(54.0, 26.0));
    if text_pill(ui, copy.intersect(visible), "Copy", theme::semibold(11.0), RAISED_HI, TEXT_DIM)
        .on_hover_text("Copy this mix as a starting point for another musician")
        .clicked()
    {
        let name = format!("{} copy", mix.musician);
        let mut new = None;
        app.edit("Copy monitor mix", |p| new = p.copy_monitor_mix(id, &name));
        if let Some(new) = new {
            ui.data_mut(|d| d.insert_temp(Id::unique(("monitor-rename", new.0)), name));
        }
    }
    let del = Rect::from_min_size(pos2(right - 28.0, y - 13.0), vec2(28.0, 26.0));
    if text_pill(ui, del.intersect(visible), "×", theme::semibold(15.0), RAISED_HI, TEXT_DIM)
        .on_hover_text("Delete this mix")
        .clicked()
    {
        app.edit("Delete monitor mix", |p| {
            p.delete_monitor_mix(id);
        });
        return;
    }
    y += 26.0;

    // Output, safety limiter.
    let outs = crate::ui::channels::Channels::outputs(app);
    let out_label = match mix.output {
        Some(o) => outs.short(pick(o)),
        None => "No output".to_owned(),
    };
    let out_label = crate::ui::widgets::truncate(ui, &out_label, &theme::mono(11.0), 120.0);
    let out = Rect::from_min_size(pos2(left, y), vec2(130.0, 28.0));
    let resp = text_pill(
        ui,
        out.intersect(visible),
        &out_label,
        theme::mono(11.0),
        BG,
        if mix.output.is_some() { TEXT } else { theme::PINK },
    )
    .on_hover_text(format!(
        "{}: where this mix plays, the in-ear transmitter or headphone amp input",
        mix.output.map_or("No output".to_owned(), |o| outs.label(pick(o)))
    ));
    let mut chosen = None;
    egui::Popup::menu(&resp).show(|ui| {
        if let Some(p) = outs.output_menu(ui, mix.output.map(pick)) {
            chosen = Some(Some(unpick(p)));
        }
        if ui.selectable_label(mix.output.is_none(), "None").clicked() {
            chosen = Some(None);
        }
    });
    if let Some(o) = chosen {
        app.edit("Set monitor output", |p| {
            p.set_monitor_output(id, o);
        });
    }
    let lim_on = app.project.monitor_limiter(id) == Some(true);
    painter.text(pos2(right - 46.0, out.center().y), Align2::RIGHT_CENTER, "Limiter", theme::regular(12.0), TEXT_DIM);
    let sw = Rect::from_min_size(pos2(right - 38.0, out.center().y - 11.0), vec2(38.0, 22.0));
    if switch(ui, sw.intersect(visible), lim_on, ACCENT).on_hover_text("Safety limiter at -0.1 dB").clicked() {
        app.edit("Toggle monitor limiter", |p| {
            p.set_monitor_limiter(id, !lim_on);
        });
    }
    y += 40.0;

    // More me, mix level, meter.
    let knob = Rect::from_min_size(pos2(left, y), vec2(64.0, 64.0));
    more_me_knob(app, ui, painter, knob.intersect(visible), knob, &mix);
    let fx = knob.right() + 14.0;
    let level_bar = Rect::from_min_max(pos2(fx, y + 10.0), pos2(right, y + 20.0));
    painter.text(pos2(fx, y + 2.0), Align2::LEFT_CENTER, "Mix level", theme::regular(11.0), TEXT_DIM);
    painter.text(
        pos2(right, y + 2.0),
        Align2::RIGHT_CENTER,
        format!("{} dB", format_db(track.volume)),
        theme::mono(11.0),
        TEXT,
    );
    if let Some(v) = hfader(ui, painter, level_bar, visible, track.volume, color, Id::unique(("monitor-level", id.0))) {
        app.gesture("Adjust monitor mix level", |p| p.track_mut(id).unwrap().volume = v);
    }
    end_gesture_on_release(app, ui, Id::unique(("monitor-level", id.0)));
    let levels = app.levels.get(&id).copied().unwrap_or_default();
    for (k, level) in [levels.0, levels.1].into_iter().enumerate() {
        let m = Rect::from_min_size(pos2(fx, y + 34.0 + k as f32 * 8.0), vec2(right - fx, 5.0));
        painter.rect_filled(m, CornerRadius::same(255), BG);
        let f = meter_fraction(level);
        if f > 0.0 {
            let c = if level >= 0.89 { YELLOW } else { GREEN };
            painter.rect_filled(
                Rect::from_min_size(m.min, vec2(m.width() * f, m.height())),
                CornerRadius::same(255),
                c,
            );
        }
    }
    y += 78.0;

    // Sources.
    painter.hline(left..=right, y, Stroke::new(1.0, RAISED_HI));
    y += 8.0;
    let sources = app.project.monitor_sources(id);
    let list = Rect::from_min_max(pos2(left, y), pos2(right, r.bottom() - 52.0));
    let scroll_id = Id::unique(("monitor-sources-scroll", id.0));
    let content_h = sources.len() as f32 * ROW_H;
    let mut scroll: f32 = ui.data(|d| d.get_temp(scroll_id)).unwrap_or(0.0);
    if ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|p| list.contains(p) && visible.contains(p))
        && !ui.ctx().input(|i| i.modifiers.shift)
    {
        scroll -= ui.ctx().input(|i| i.smooth_scroll_delta.y);
    }
    scroll = scroll.clamp(0.0, (content_h - list.height()).max(0.0));
    ui.data_mut(|d| d.insert_temp(scroll_id, scroll));
    let list_painter = painter.with_clip_rect(list.intersect(visible));
    for (i, s) in sources.iter().enumerate() {
        let row = Rect::from_min_size(pos2(left, list.top() + i as f32 * ROW_H - scroll), vec2(list.width(), ROW_H));
        if row.bottom() < list.top() || row.top() > list.bottom() {
            continue;
        }
        source_row(app, ui, &list_painter, row, list.intersect(visible), id, s);
    }

    // Add a source.
    let add = Rect::from_min_size(pos2(left, r.bottom() - 44.0), vec2(right - left, 32.0));
    let missing: Vec<(TrackId, String)> = app
        .project
        .tracks
        .iter()
        .filter(|t| t.id != id && !app.project.is_monitor_mix(t.id) && !sources.iter().any(|s| s.track == t.id))
        .map(|t| (t.id, t.name.clone()))
        .collect();
    let resp = text_pill(ui, add.intersect(visible), "+ Add source", theme::semibold(12.0), BG, TEXT_DIM);
    egui::Popup::menu(&resp).show(|ui| {
        if missing.is_empty() {
            ui.label("Every track is in this mix");
        }
        for (t, name) in &missing {
            if ui.button(if name.is_empty() { "(unnamed)" } else { name.as_str() }).clicked() {
                app.edit("Add monitor source", |p| {
                    p.add_monitor_source(id, *t, SendMode::PostFx);
                });
            }
        }
    });
}

/// One source: name, level fader with the pan under it, "me" and mute toggles.
#[allow(clippy::too_many_arguments)]
fn source_row(
    app: &mut ScytheApp,
    ui: &mut Ui,
    painter: &egui::Painter,
    row: Rect,
    visible: Rect,
    mix: TrackId,
    s: &MonitorSource,
) {
    let Some(src) = app.project.track(s.track) else { return };
    let name = if src.name.is_empty() { "(unnamed)".to_owned() } else { src.name.clone() };
    let color = track_color(src.color);
    let cy = row.center().y - 4.0;
    painter.circle_filled(pos2(row.left() + 4.0, cy), 3.5, color);
    let label = widgets::truncate(ui, &name, &theme::regular(12.0), 74.0);
    let text_color = if s.muted { TEXT_DIM } else { TEXT };
    painter.text(pos2(row.left() + 13.0, cy), Align2::LEFT_CENTER, label, theme::regular(12.0), text_color);
    let salt = (mix.0, s.track.0);

    let bar = Rect::from_min_max(pos2(row.left() + 92.0, cy - 5.0), pos2(row.right() - 104.0, cy + 5.0));
    let fill = if s.muted { theme::SWITCH_OFF } else { color };
    let fader_id = Id::unique(("monitor-src", salt));
    if let Some(v) = hfader(ui, painter, bar, visible, s.volume, fill, fader_id) {
        app.gesture("Adjust monitor source level", |p| {
            p.set_monitor_level(mix, s.track, v);
        });
    }
    end_gesture_on_release(app, ui, fader_id);

    // Pan: a thin line under the fader.
    let pan = Rect::from_min_max(pos2(bar.left(), bar.bottom() + 6.0), pos2(bar.right(), bar.bottom() + 9.0));
    painter.rect_filled(pan, CornerRadius::same(255), BG);
    painter.vline(pan.center().x, pan.top() - 1.0..=pan.bottom() + 1.0, Stroke::new(1.0, theme::SWITCH_OFF));
    let px = pan.left() + (s.pan as f32 + 1.0) / 2.0 * pan.width();
    painter.circle_filled(pos2(px, pan.center().y), 3.5, TEXT_DIM);
    let pan_hit = ui
        .interact(
            pan.expand2(vec2(3.0, 4.0)).intersect(visible),
            Id::unique(("monitor-pan", salt)),
            Sense::click_and_drag(),
        )
        .on_hover_text("Drag to pan, double-click to centre");
    if pan_hit.dragged() {
        let d = pan_hit.drag_delta().x as f64 / pan.width() as f64 * 2.0;
        app.gesture("Pan monitor source", |p| {
            p.set_monitor_pan(mix, s.track, s.pan + d);
        });
    }
    if pan_hit.drag_stopped() {
        app.end_gesture();
    }
    if pan_hit.double_clicked() {
        app.edit("Centre monitor source", |p| {
            p.set_monitor_pan(mix, s.track, 0.0);
        });
    }

    let db = if s.volume <= 0.0 { "-inf".to_owned() } else { format_db(s.volume) };
    painter.text(pos2(row.right() - 66.0, cy), Align2::RIGHT_CENTER, db, theme::mono(10.0), TEXT_DIM);
    let me = Rect::from_min_size(pos2(row.right() - 60.0, cy - 11.0), vec2(30.0, 22.0));
    let (bg, fg) = if s.own { (ACCENT, BG) } else { (BG, TEXT_DIM) };
    if text_pill(ui, me.intersect(visible), "me", theme::semibold(10.0), bg, fg)
        .on_hover_text("One of the musician's own sources: \"more me\" raises it")
        .clicked()
    {
        let mut own = app.project.monitor_mix(mix).map(|m| m.own).unwrap_or_default();
        if s.own {
            own.retain(|t| *t != s.track);
        } else {
            own.push(s.track);
        }
        app.edit("Set own monitor sources", |p| {
            p.set_monitor_own(mix, &own);
        });
    }
    let mute = Rect::from_min_size(pos2(row.right() - 26.0, cy - 11.0), vec2(26.0, 22.0));
    let (bg, fg) = if s.muted { (theme::PINK, BG) } else { (BG, TEXT_DIM) };
    let resp = text_pill(ui, mute.intersect(visible), "M", theme::semibold(10.0), bg, fg);
    if resp.on_hover_text("Mute this source in the mix").clicked() {
        app.edit("Toggle monitor source mute", |p| {
            p.set_monitor_muted(mix, s.track, !s.muted);
        });
    }
    let row_hit = ui.interact(
        Rect::from_min_max(row.min, pos2(bar.left() - 4.0, row.max.y)).intersect(visible),
        Id::unique(("monitor-row", salt)),
        Sense::click(),
    );
    row_hit.context_menu(|ui| {
        for mode in SendMode::CHOICES {
            if ui.selectable_label(s.mode.label() == mode.label(), mode.label()).clicked() {
                app.edit("Set monitor source mode", |p| {
                    if let Some(send) = p.track_mut(s.track).and_then(|t| t.sends.iter_mut().find(|x| x.dest == mix)) {
                        send.mode = mode;
                    }
                });
            }
        }
        ui.separator();
        if ui.button("Remove from this mix").clicked() {
            app.edit("Remove monitor source", |p| {
                p.remove_monitor_source(mix, s.track);
            });
        }
    });
}

/// A horizontal fader on REAPER's fader law; returns the new gain while dragged.
fn hfader(
    ui: &mut Ui,
    painter: &egui::Painter,
    bar: Rect,
    visible: Rect,
    gain: f64,
    color: Color32,
    id: Id,
) -> Option<f64> {
    painter.rect_filled(bar, CornerRadius::same(255), BG);
    let pos = fader_position(gain);
    if pos > 0.0 {
        let fill = Rect::from_min_size(bar.min, vec2(bar.width() * pos, bar.height()));
        painter.rect_filled(fill, CornerRadius::same(255), color.gamma_multiply(0.85));
    }
    let unity = bar.left() + fader_position(1.0) * bar.width();
    painter.vline(unity, bar.top() - 2.0..=bar.bottom() + 2.0, Stroke::new(1.0, TEXT_DIM));
    painter.circle_filled(pos2(bar.left() + pos * bar.width(), bar.center().y), bar.height() * 0.7, theme::TEXT);
    let resp = ui
        .interact(bar.expand2(vec2(4.0, 6.0)).intersect(visible), id, Sense::click_and_drag())
        .on_hover_text("Drag to set the level, double-click for 0 dB, Shift for fine");
    if resp.double_clicked() {
        return Some(1.0);
    }
    if resp.dragged() {
        let fine = ui.ctx().input(|i| i.modifiers.shift);
        let d = resp.drag_delta().x / bar.width() * if fine { 0.1 } else { 1.0 };
        return Some(fader_gain(pos + d));
    }
    None
}

fn end_gesture_on_release(app: &mut ScytheApp, ui: &Ui, id: Id) {
    if ui.ctx().read_response(id).is_some_and(|r| r.drag_stopped() || r.double_clicked()) {
        app.end_gesture();
    }
}

/// The "more me" knob: drag up to hear more of the musician's own sources.
fn more_me_knob(app: &mut ScytheApp, ui: &mut Ui, painter: &egui::Painter, hit: Rect, r: Rect, mix: &MonitorMix) {
    let c = r.center() - vec2(0.0, 6.0);
    let radius = 22.0;
    let (lo, hi) = (*MORE_ME_RANGE.start(), *MORE_ME_RANGE.end());
    let frac = ((mix.more_me - lo) / (hi - lo)) as f32;
    let start = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let arc = |from: f32, to: f32| -> Vec<egui::Pos2> {
        (0..=24).map(|i| from + (to - from) * i as f32 / 24.0).map(|a| c + vec2(a.cos(), a.sin()) * radius).collect()
    };
    painter.add(egui::Shape::line(arc(start, start + sweep), Stroke::new(4.0, BG)));
    let mid = start + sweep * 0.5;
    let at = start + sweep * frac;
    let col = if mix.own.is_empty() { TEXT_DIM } else { ACCENT };
    painter.add(egui::Shape::line(arc(mid.min(at), mid.max(at)), Stroke::new(4.0, col)));
    painter.circle_filled(c, 14.0, RAISED_HI);
    painter.line_segment(
        [c + vec2(at.cos(), at.sin()) * 6.0, c + vec2(at.cos(), at.sin()) * 13.0],
        Stroke::new(2.0, TEXT),
    );
    let label = if mix.more_me == 0.0 { "0".to_owned() } else { format!("{:+.1}", mix.more_me) };
    painter.text(
        pos2(c.x, r.bottom() - 2.0),
        Align2::CENTER_BOTTOM,
        format!("More me {label}"),
        theme::regular(10.0),
        TEXT_DIM,
    );
    let tip = if mix.own.is_empty() {
        "More me: mark the musician's own sources with \"me\" first"
    } else {
        "More me: drag up to raise the musician's own sources against the rest, double-click to reset"
    };
    let id = Id::unique(("more-me", mix.track.0));
    let resp = ui.interact(hit, id, Sense::click_and_drag()).on_hover_text(tip);
    let track = mix.track;
    if resp.dragged() {
        let v = (mix.more_me - resp.drag_delta().y as f64 * 0.2).clamp(lo, hi);
        let v = (v * 10.0).round() / 10.0;
        if v != mix.more_me {
            app.gesture("More me", |p| {
                p.set_more_me(track, v);
            });
        }
    }
    if resp.drag_stopped() {
        app.end_gesture();
    }
    if resp.double_clicked() {
        app.edit("Reset more me", |p| {
            p.set_more_me(track, 0.0);
        });
    }
}

/// The monitor mix wizard and the cue buss dialog, when open.
pub fn wizards(app: &mut ScytheApp, ui: &mut Ui) {
    for cue in [false, true] {
        let slot = if cue { &mut app.view.cue_dialog } else { &mut app.view.monitor_wizard };
        let Some(mut w) = slot.take() else { continue };
        let mut open = true;
        let mut done = false;
        let title = if cue { "Create cue buss from track selection" } else { "New monitor mix" };
        let key = if cue { "cue-buss" } else { "monitor-wizard" };
        scythe_window(key, title, WindowKind::Dialog)
            .default_width(420.0)
            .show(ui.ctx(), &mut open, |ui| done = wizard_body(app, ui, &mut w));
        if open && !done {
            let slot = if cue { &mut app.view.cue_dialog } else { &mut app.view.monitor_wizard };
            *slot = Some(w);
        }
    }
}

fn wizard_body(app: &mut ScytheApp, ui: &mut Ui, w: &mut Wizard) -> bool {
    let p = &app.project;
    egui::Grid::new("monitor-wizard").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(if w.cue_bus { "Name" } else { "Musician" });
        ui.text_edit_singleline(&mut w.name);
        ui.end_row();
        if !w.cue_bus {
            ui.label("Start from");
            let mixes = p.monitor_mixes();
            let current = w.copy_from.and_then(|t| mixes.iter().find(|m| m.track == t)).map(|m| m.musician.clone());
            egui::ComboBox::from_id_salt("monitor-copy")
                .selected_text(current.unwrap_or_else(|| "Empty mix".to_owned()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut w.copy_from, None, "Empty mix");
                    for m in &mixes {
                        ui.selectable_value(&mut w.copy_from, Some(m.track), format!("Copy of {}", m.musician));
                    }
                });
            ui.end_row();
        }
        ui.label("Output");
        let outs = crate::ui::channels::Channels::outputs(app);
        let label = w.output.map_or("None".to_owned(), |o| outs.label(pick(o)));
        egui::ComboBox::from_id_salt("monitor-out").selected_text(label).show_ui(ui, |ui| {
            if let Some(p) = outs.output_menu(ui, w.output.map(pick)) {
                w.output = Some(unpick(p));
            }
            ui.selectable_value(&mut w.output, None, "None");
        });
        ui.end_row();
        if w.copy_from.is_none() {
            ui.label("Send type");
            egui::ComboBox::from_id_salt("monitor-mode").selected_text(w.mode.label()).show_ui(ui, |ui| {
                for m in SendMode::CHOICES {
                    ui.selectable_value(&mut w.mode, m, m.label());
                }
            });
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut w.volumes_from_tracks, "Send volumes from the track faders");
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut w.pans_from_tracks, "Send pans from the tracks");
            ui.end_row();
            ui.label("Track template");
            ui.horizontal(|ui| {
                let name = w.template.as_ref().and_then(|t| t.file_name()).map(|n| n.to_string_lossy().into_owned());
                if ui.button(name.unwrap_or_else(|| "None…".to_owned())).clicked()
                    && let Some(path) = crate::app::resource_dialog("TrackTemplates")
                        .add_filter("Track template", &["RTrackTemplate"])
                        .pick_file()
                {
                    w.template = Some(path);
                }
                if w.template.is_some() && ui.small_button("×").clicked() {
                    w.template = None;
                }
            });
            ui.end_row();
            if w.template.is_none() {
                ui.label("");
                ui.checkbox(&mut w.limiter, "Safety limiter on the mix");
                ui.end_row();
            }
        }
    });
    if w.copy_from.is_none() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Sources").strong());
            if ui.small_button("All").clicked() {
                w.sources = p.monitor_candidates();
            }
            if ui.small_button("None").clicked() {
                w.sources.clear();
            }
        });
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            for t in p.tracks.iter().filter(|t| !p.is_monitor_mix(t.id)) {
                ui.horizontal(|ui| {
                    let mut on = w.sources.contains(&t.id);
                    let name = if t.name.is_empty() { "(unnamed)" } else { t.name.as_str() };
                    if ui.checkbox(&mut on, name).changed() {
                        if on {
                            w.sources.push(t.id);
                        } else {
                            w.sources.retain(|x| *x != t.id);
                            w.own.retain(|x| *x != t.id);
                        }
                    }
                    if !w.cue_bus && on {
                        let mut mine = w.own.contains(&t.id);
                        if ui.checkbox(&mut mine, "mine").on_hover_text("One of the musician's own sources").changed() {
                            if mine {
                                w.own.push(t.id);
                            } else {
                                w.own.retain(|x| *x != t.id);
                            }
                        }
                    }
                });
            }
        });
    }
    ui.add_space(8.0);
    let mut done = false;
    ui.horizontal(|ui| {
        let ok = !w.name.trim().is_empty();
        if ui.add_enabled(ok, egui::Button::new(if w.cue_bus { "Create cue buss" } else { "Create mix" })).clicked() {
            done = create(app, w);
        }
        if ui.button("Cancel").clicked() {
            done = true;
        }
    });
    done
}

/// Creates what the wizard describes; false (with a message) when it could not.
fn create(app: &mut ScytheApp, w: &Wizard) -> bool {
    let name = w.name.trim().to_owned();
    if let Some(from) = w.copy_from {
        let mut new = None;
        app.edit("Copy monitor mix", |p| {
            new = p.copy_monitor_mix(from, &name);
            if let Some(id) = new {
                p.set_monitor_output(id, w.output);
            }
        });
        app.view.monitor_page = true;
        return new.is_some();
    }
    let opts = match w.options() {
        Ok(o) => o,
        Err(e) => {
            app.notify(format!("Could not load the track template: {e}"));
            return false;
        }
    };
    let sources: Vec<TrackId> = app.project.tracks.iter().map(|t| t.id).filter(|t| w.sources.contains(t)).collect();
    let mut id = TrackId(0);
    if w.cue_bus {
        app.edit("Create cue buss", |p| id = p.create_cue_bus(&sources, &opts));
    } else {
        app.edit("Create monitor mix", |p| id = p.create_monitor_mix(&name, &sources, &w.own, &opts));
        app.view.monitor_page = true;
    }
    app.sel = crate::app::Selection { track: Some(id), item: None };
    true
}
