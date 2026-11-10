//! The audio device's channels as menus and labels name them: every input and output the
//! device opened, by its name or alias (`Input 3`, `Snare`), mono and in stereo pairs.

use egui::Ui;

use crate::app::ScytheApp;

/// One side of the device's channels.
#[derive(Debug, Clone)]
pub struct Channels {
    pub names: Vec<String>,
    pub output: bool,
    /// Stereo pairs start on every channel (`Input 2 / Input 3`), REAPER's default.
    pub all_pairs: bool,
}

/// A channel pick: the first channel and whether it is mono.
pub type Pick = (usize, bool);

impl Channels {
    pub fn inputs(app: &ScytheApp) -> Self {
        Self { names: app.input_names(), output: false, all_pairs: app.prefs.all_stereo_pairs.unwrap_or(true) }
    }

    pub fn outputs(app: &ScytheApp) -> Self {
        Self { names: app.output_names(), output: true, all_pairs: app.prefs.all_stereo_pairs.unwrap_or(true) }
    }

    /// Channel `i`'s name; one past the device's channels gets its number.
    pub fn name(&self, i: usize) -> String {
        self.names
            .get(i)
            .cloned()
            .unwrap_or_else(|| format!("{} {}", if self.output { "Output" } else { "Input" }, i + 1))
    }

    /// `Input 1 / Input 2`.
    pub fn pair(&self, first: usize) -> String {
        format!("{} / {}", self.name(first), self.name(first + 1))
    }

    pub fn label(&self, (first, mono): Pick) -> String {
        if mono { self.name(first) } else { self.pair(first) }
    }

    /// A short label for a narrow pill: `In 3/4` for unnamed channels, else the names.
    pub fn short(&self, (first, mono): Pick) -> String {
        let side = if self.output { "Out" } else { "In" };
        let plain = |i: usize| self.name(i) == format!("{} {}", if self.output { "Output" } else { "Input" }, i + 1);
        match mono {
            true if plain(first) => format!("{side} {}", first + 1),
            true => self.name(first),
            false if plain(first) && plain(first + 1) => format!("{side} {}/{}", first + 1, first + 2),
            false => self.pair(first),
        }
    }

    /// How many channels the menus list: the device's, and any `used` beyond them.
    fn count(&self, used: Option<Pick>) -> usize {
        let past = used.map_or(0, |(first, mono)| first + if mono { 1 } else { 2 });
        self.names.len().max(past)
    }

    fn pair_starts(&self, n: usize) -> impl Iterator<Item = usize> {
        let step = if self.all_pairs { 1 } else { 2 };
        (0..n.saturating_sub(1)).step_by(step)
    }

    /// The stereo pairs, then a "Mono" submenu with each channel, as REAPER's hardware output
    /// menus list them. Returns the pick.
    pub fn output_menu(&self, ui: &mut Ui, current: Option<Pick>) -> Option<Pick> {
        let n = self.count(current);
        let mut picked = None;
        for first in self.pair_starts(n) {
            if ui.selectable_label(current == Some((first, false)), self.pair(first)).clicked() {
                picked = Some((first, false));
            }
        }
        ui.menu_button("Mono", |ui| {
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                for i in 0..n {
                    if ui.selectable_label(current == Some((i, true)), self.name(i)).clicked() {
                        picked = Some((i, true));
                    }
                }
            });
        });
        if n == 0 {
            ui.label("The device has no outputs");
        }
        picked
    }

    /// REAPER's record input menu: "Input: Mono" and "Input: Stereo" submenus.
    pub fn input_menu(&self, ui: &mut Ui, current: Option<Pick>) -> Option<Pick> {
        let n = self.count(current);
        let mut picked = None;
        ui.menu_button("Input: Mono", |ui| {
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                for i in 0..n {
                    if ui.selectable_label(current == Some((i, true)), self.name(i)).clicked() {
                        picked = Some((i, true));
                    }
                }
                if n == 0 {
                    ui.label("The device has no inputs");
                }
            });
        });
        ui.menu_button("Input: Stereo", |ui| {
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                for first in self.pair_starts(n) {
                    if ui.selectable_label(current == Some((first, false)), self.pair(first)).clicked() {
                        picked = Some((first, false));
                    }
                }
                if n < 2 {
                    ui.label("The device has no input pair");
                }
            });
        });
        picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(names: &[&str], output: bool) -> Channels {
        Channels { names: names.iter().map(|s| s.to_string()).collect(), output, all_pairs: true }
    }

    #[test]
    fn labels_use_the_names_and_shorten_plain_ones() {
        let ins = side(&["Input 1", "Snare", "Input 3", "Input 4"], false);
        assert_eq!(ins.label((0, false)), "Input 1 / Snare");
        assert_eq!(ins.short((2, false)), "In 3/4");
        assert_eq!(ins.short((1, true)), "Snare");
        assert_eq!(ins.short((3, true)), "In 4");
        let outs = side(&["Output 1", "Output 2"], true);
        assert_eq!(outs.label((8, false)), "Output 9 / Output 10");
        assert_eq!(outs.count(Some((8, false))), 10);
        assert_eq!(outs.pair_starts(4).collect::<Vec<_>>(), [0, 1, 2]);
        let odd = Channels { all_pairs: false, ..outs };
        assert_eq!(odd.pair_starts(6).collect::<Vec<_>>(), [0, 2, 4]);
    }
}
