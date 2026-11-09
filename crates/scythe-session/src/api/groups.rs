use schemars::JsonSchema;
use scythe_core::groups::{GROUP_COUNT, GroupParam};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, TrackRef, json};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum GroupCommand {
    /// Every track group in use: its name and which tracks lead or follow what. A lead's
    /// volume, pan, width, mute, solo, arm, polarity or automation mode edits carry over to the
    /// followers of the same parameter; a VCA lead's level scales its followers instead.
    #[serde(rename = "group.list")]
    List,
    /// Names a group, or clears its name with `null`.
    #[serde(rename = "group.name")]
    Name {
        /// Group number, 1-64.
        group: usize,
        name: Option<String>,
    },
    /// Adds a track to a group for some parameters, or takes it out (`on: false`).
    #[serde(rename = "group.set")]
    Set {
        track: TrackRef,
        /// Group number, 1-64.
        group: usize,
        /// REAPER's parameter names, case-insensitive: VOLUME_LEAD, VOLUME_FOLLOW, PAN_LEAD,
        /// PAN_FOLLOW, WIDTH_LEAD, WIDTH_FOLLOW, MUTE_LEAD, MUTE_FOLLOW, SOLO_LEAD, SOLO_FOLLOW,
        /// RECARM_LEAD, RECARM_FOLLOW, POLARITY_LEAD, POLARITY_FOLLOW, AUTOMODE_LEAD,
        /// AUTOMODE_FOLLOW, VOLUME_REVERSE, PAN_REVERSE, WIDTH_REVERSE, NO_LEAD_WHEN_FOLLOW,
        /// VOLUME_VCA_LEAD, VOLUME_VCA_FOLLOW, VOLUME_VCA_FOLLOW_ISPREFX, MEDIA_EDIT_LEAD,
        /// MEDIA_EDIT_FOLLOW.
        params: Vec<String>,
        /// False removes the track from the group for these parameters.
        #[serde(default = "yes")]
        on: bool,
    },
    /// Removes every track from a group and forgets its name.
    #[serde(rename = "group.clear")]
    Clear {
        /// Group number, 1-64.
        group: usize,
    },
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupInfo {
    /// Group number, 1-64.
    pub group: usize,
    pub name: Option<String>,
    pub members: Vec<GroupMember>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMember {
    pub track: u64,
    pub name: String,
    pub params: Vec<String>,
}

fn group_index(group: usize) -> Result<usize, ApiError> {
    if !(1..=GROUP_COUNT).contains(&group) {
        return Err(ApiError::invalid(format!("groups are numbered 1 to {GROUP_COUNT}")));
    }
    Ok(group - 1)
}

pub fn list(session: &Session) -> Vec<GroupInfo> {
    let p = &session.project;
    p.used_groups()
        .into_iter()
        .map(|g| GroupInfo {
            group: g + 1,
            name: p.group_name(g).map(str::to_owned),
            members: p
                .tracks
                .iter()
                .filter_map(|t| {
                    let params: Vec<String> =
                        GroupParam::ALL.iter().filter(|q| t.groups.has(**q, g)).map(|q| q.name().to_owned()).collect();
                    (!params.is_empty()).then(|| GroupMember { track: t.id.0, name: t.name.clone(), params })
                })
                .collect(),
        })
        .collect()
}

pub fn execute(session: &mut Session, command: GroupCommand) -> Result<Value, ApiError> {
    match command {
        GroupCommand::List => Ok(json(list(session))),
        GroupCommand::Name { group, name } => {
            let g = group_index(group)?;
            session.edit("Rename group", |p| match name.filter(|n| !n.is_empty()) {
                Some(n) => {
                    p.group_names.insert(g, n);
                }
                None => {
                    p.group_names.remove(&g);
                }
            });
            Ok(json(list(session)))
        }
        GroupCommand::Set { track, group, params, on } => {
            let g = group_index(group)?;
            let params = params
                .iter()
                .map(|n| {
                    GroupParam::from_name(n).ok_or_else(|| ApiError::invalid(format!("no group parameter \"{n}\"")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let id = track.track(&session.project)?;
            session.edit("Track grouping", |p| {
                if let Some(t) = p.track_mut(id) {
                    for q in &params {
                        t.groups.set(*q, g, on);
                    }
                }
            });
            Ok(json(list(session)))
        }
        GroupCommand::Clear { group } => {
            let g = group_index(group)?;
            session.edit("Clear group", |p| {
                for t in &mut p.tracks {
                    for q in GroupParam::ALL {
                        t.groups.set(q, g, false);
                    }
                }
                p.group_names.remove(&g);
            });
            Ok(json(list(session)))
        }
    }
}
