//! Marker actions on the audio thread: the `!` markers of the project as frames, and what
//! crossing one reports back to the host.

use scythe_core::Project;
use scythe_core::marker_action::{self, Command, TransportAction};

/// A command in a form the audio thread can copy: named commands index `CueList::names`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CueCommand {
    Id(u32),
    Named(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cue {
    pub frame: i64,
    pub commands: Vec<CueCommand>,
}

/// The action markers of one project, sorted by frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct CueList {
    pub cues: Vec<Cue>,
    pub names: Vec<String>,
    /// Where "go to end of project" goes.
    pub end_frame: i64,
    /// Tells lists apart, so a reported hit is named from the list it came from.
    pub generation: u64,
}

impl CueList {
    pub fn build(project: &Project, sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        let mut list = CueList {
            end_frame: (scythe_core::setlist::project_end(project) * sr).round() as i64,
            ..Default::default()
        };
        for m in &project.markers {
            let Some(commands) = marker_action::commands(&m.name) else { continue };
            let commands = commands
                .into_iter()
                .map(|c| match c {
                    Command::Id(id) => CueCommand::Id(id),
                    Command::Named(name) => {
                        let i = list.names.iter().position(|n| *n == name).unwrap_or_else(|| {
                            list.names.push(name);
                            list.names.len() - 1
                        });
                        CueCommand::Named(i as u32)
                    }
                })
                .collect::<Vec<_>>();
            if !commands.is_empty() {
                list.cues.push(Cue { frame: (m.position.max(0.0) * sr).round() as i64, commands });
            }
        }
        list.cues.sort_by_key(|c| c.frame);
        list
    }

    /// The first cue after `from`, up to and including `to`.
    pub fn first_in(&self, from: i64, to: i64) -> Option<&Cue> {
        let i = self.cues.partition_point(|c| c.frame <= from);
        self.cues.get(i).filter(|c| c.frame <= to)
    }

    pub fn command(&self, c: CueCommand) -> Command {
        match c {
            CueCommand::Id(id) => Command::Id(id),
            CueCommand::Named(i) => Command::Named(self.names.get(i as usize).cloned().unwrap_or_default()),
        }
    }

    pub fn transport_action(c: CueCommand) -> Option<TransportAction> {
        match c {
            CueCommand::Id(id) => TransportAction::of(&Command::Id(id)),
            CueCommand::Named(_) => None,
        }
    }
}

/// A marker action the audio thread ran into.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Hit {
    pub frame: i64,
    pub command: CueCommand,
    pub applied: bool,
    pub generation: u64,
}

/// A command a crossed marker ran, as the engine reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerAction {
    /// The marker's position in seconds.
    pub position: f64,
    pub command: Command,
    /// What the engine did to the transport, `None` when the command is left to the host.
    pub applied: Option<TransportAction>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_sorted_cues_from_action_markers() {
        let mut p = Project::default();
        p.add_marker(2.0, "!1016 _SWS_X 40042");
        p.add_marker(1.0, "!_SWS_X");
        p.add_marker(1.5, "Song");
        p.add_marker(3.0, "!");
        let list = CueList::build(&p, 100);
        assert_eq!(list.cues.len(), 2);
        assert_eq!(list.cues[0], Cue { frame: 100, commands: vec![CueCommand::Named(0)] });
        assert_eq!(list.cues[1].commands, [CueCommand::Id(1016), CueCommand::Named(0), CueCommand::Id(40042)]);
        assert_eq!(list.command(CueCommand::Named(0)), Command::Named("_SWS_X".into()));
        assert_eq!(list.end_frame, 300);
        assert_eq!(list.first_in(100, 199).map(|c| c.frame), None);
        assert_eq!(list.first_in(99, 199).map(|c| c.frame), Some(100));
        assert_eq!(list.first_in(150, 200).map(|c| c.frame), Some(200));
    }
}
