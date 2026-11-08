//! The modal dialogs scripts open (`ShowMessageBox`, `GetUserInputs`). A script waits for
//! the answer in the middle of a call, while the app's own window can only draw between
//! frames, so each dialog is a window of its own in a child process of Scythe
//! (`scythe --script-dialog`), which reads the request on stdin and writes the answer on
//! stdout.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// Shows a dialog and waits for it: a message box's button, as REAPER numbers them, or the
/// values entered (`null` when cancelled).
pub fn ask(request: &Value) -> Option<Value> {
    let exe = std::env::current_exe().ok()?;
    let mut child =
        Command::new(exe).arg("--script-dialog").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().ok()?;
    child.stdin.take()?.write_all(request.to_string().as_bytes()).ok()?;
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let _ = child.wait();
    serde_json::from_str(out.trim()).ok()
}

/// The buttons of `ShowMessageBox`'s types, with the numbers REAPER returns for them.
fn buttons(kind: i64) -> &'static [(&'static str, i64)] {
    match kind {
        1 => &[("OK", 1), ("Cancel", 2)],
        2 => &[("Abort", 3), ("Retry", 4), ("Ignore", 5)],
        3 => &[("Yes", 6), ("No", 7), ("Cancel", 2)],
        4 => &[("Yes", 6), ("No", 7)],
        5 => &[("Retry", 4), ("Cancel", 2)],
        _ => &[("OK", 1)],
    }
}

/// What closing the window without a button gives.
fn dismissed(kind: i64) -> i64 {
    match kind {
        1 | 3 | 5 => 2,
        2 => 3,
        4 => 7,
        _ => 1,
    }
}

struct Dialog {
    request: Value,
    values: Vec<String>,
    answer: Option<Value>,
}

impl Dialog {
    fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let kind = self.request["kind"].as_i64().unwrap_or(0);
        let inputs = self.request["type"] == "inputs";
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            if inputs {
                let captions: Vec<String> = self.request["captions"]
                    .as_array()
                    .map(|a| a.iter().map(|c| c.as_str().unwrap_or("").to_owned()).collect())
                    .unwrap_or_default();
                egui::Grid::new("inputs").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    for (i, caption) in captions.iter().enumerate() {
                        let (label, hidden) = match caption.strip_prefix('*') {
                            Some(c) => (c, true),
                            None => (caption.as_str(), false),
                        };
                        ui.label(label);
                        let field =
                            egui::TextEdit::singleline(&mut self.values[i]).password(hidden).desired_width(260.0);
                        let resp = ui.add(field);
                        if i == 0 && !resp.has_focus() && ctx.cumulative_pass_nr() < 2 {
                            resp.request_focus();
                        }
                        ui.end_row();
                    }
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.answer = Some(json!(self.values));
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.answer = Some(Value::Null);
                    }
                });
            } else {
                ui.label(self.request["text"].as_str().unwrap_or(""));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    for (label, id) in buttons(kind) {
                        if ui.button(*label).clicked() {
                            self.answer = Some(json!(id));
                        }
                    }
                });
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.answer = Some(json!(buttons(kind)[0].1));
                }
            }
        });
        if ctx.input(|i| i.viewport().close_requested()) && self.answer.is_none() {
            self.answer = Some(if inputs { Value::Null } else { json!(dismissed(kind)) });
        }
        if self.answer.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// `scythe --script-dialog`: shows the dialog read from stdin and writes the answer.
pub fn run_child() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let request: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let inputs = request["type"] == "inputs";
    let values: Vec<String> = request["values"]
        .as_array()
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_owned()).collect())
        .unwrap_or_default();
    let rows = values.len().max(1) as f32;
    let size = if inputs { [420.0, 90.0 + rows * 30.0] } else { [420.0, 150.0] };
    let title = request["title"].as_str().unwrap_or("Scythe").to_owned();
    let answer = std::rc::Rc::new(std::cell::RefCell::new(None));
    let kept = answer.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title(title).with_inner_size(size).with_resizable(false),
        ..Default::default()
    };
    let _ = eframe::run_native(
        "Scythe script dialog",
        options,
        Box::new(move |cc| {
            crate::theme::install(&cc.egui_ctx);
            Ok(Box::new(Shown { dialog: Dialog { request, values, answer: None }, out: kept }))
        }),
    );
    let answer = answer.borrow_mut().take().unwrap_or(Value::Null);
    println!("{answer}");
}

/// The dialog and where its answer goes once the window closes.
struct Shown {
    dialog: Dialog,
    out: std::rc::Rc<std::cell::RefCell<Option<Value>>>,
}

impl eframe::App for Shown {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.dialog.show(ui);
        if let Some(a) = &self.dialog.answer {
            *self.out.borrow_mut() = Some(a.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_are_numbered_as_reaper_numbers_them() {
        assert_eq!(buttons(4), &[("Yes", 6), ("No", 7)]);
        assert_eq!(buttons(1)[1], ("Cancel", 2));
        assert_eq!((dismissed(0), dismissed(3), dismissed(4)), (1, 2, 7));
    }
}
