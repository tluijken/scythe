use schemars::JsonSchema;
use scythe_core::Project;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, json, snapshot, time};
use crate::Session;

/// Markers and regions are named by their number; `region` tells a marker and a region with
/// the same number apart (REAPER numbers them separately).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum MarkerCommand {
    /// All markers and regions, by position.
    #[serde(rename = "marker.list")]
    List,
    /// Adds a marker at `position` (the edit cursor when left out), or a region when `end` is
    /// given, and answers with it.
    #[serde(rename = "marker.add")]
    Add {
        /// Seconds; the edit cursor when left out.
        #[serde(default)]
        position: Option<f64>,
        /// Seconds; makes it a region ending here.
        #[serde(default)]
        end: Option<f64>,
        #[serde(default)]
        name: Option<String>,
        /// `#rrggbb`.
        #[serde(default)]
        color: Option<String>,
    },
    /// Changes what is given; the rest stays.
    #[serde(rename = "marker.edit")]
    Edit {
        /// The marker's or region's number.
        marker: u32,
        /// Picks the region (true) or the marker (false) when both use the number.
        #[serde(default)]
        region: Option<bool>,
        /// Seconds.
        #[serde(default)]
        position: Option<f64>,
        /// Seconds; regions only.
        #[serde(default)]
        end: Option<f64>,
        #[serde(default)]
        name: Option<String>,
        /// `#rrggbb`.
        #[serde(default)]
        color: Option<String>,
    },
    /// Deletes the marker or region.
    #[serde(rename = "marker.delete")]
    Delete {
        /// The marker's or region's number.
        marker: u32,
        /// Picks the region (true) or the marker (false) when both use the number.
        #[serde(default)]
        region: Option<bool>,
    },
    /// Moves the edit cursor (and the playhead while playing) to the marker or region start.
    #[serde(rename = "marker.goto")]
    Goto {
        /// The marker's or region's number.
        marker: u32,
        /// Picks the region (true) or the marker (false) when both use the number.
        #[serde(default)]
        region: Option<bool>,
    },
}

fn position_of(project: &Project, number: u32, region: Option<bool>) -> Result<usize, ApiError> {
    project
        .markers
        .iter()
        .position(|m| m.index == number && region.is_none_or(|r| r == m.region_end.is_some()))
        .ok_or_else(|| {
            let what = if region == Some(true) { "region" } else { "marker" };
            ApiError::not_found(format!("no {what} {number}"))
        })
}

fn color(text: Option<&str>) -> Result<Option<scythe_core::Color>, ApiError> {
    text.map(|t| snapshot::parse_color(t).ok_or_else(|| ApiError::invalid("colours are written #rrggbb"))).transpose()
}

fn info(session: &Session, number: u32, region: bool) -> Value {
    let found = session.project.markers.iter().find(|m| m.index == number && m.region_end.is_some() == region);
    found.map_or(Value::Null, |m| json(snapshot::marker(m)))
}

pub fn execute(session: &mut Session, command: MarkerCommand) -> Result<Value, ApiError> {
    match command {
        MarkerCommand::List => Ok(json(session.project.markers.iter().map(snapshot::marker).collect::<Vec<_>>())),
        MarkerCommand::Add { position, end, name, color: c } => {
            let position = time(position.unwrap_or(session.project.cursor), "position")?;
            let end = end.map(|e| time(e, "end")).transpose()?;
            if end.is_some_and(|e| e <= position) {
                return Err(ApiError::invalid("a region ends after it starts"));
            }
            let color = color(c.as_deref())?;
            let number = session.project.markers.iter().map(|m| m.index).max().unwrap_or(0) + 1;
            let label = if end.is_some() { "Insert region" } else { "Insert marker" };
            session.edit(label, |p| {
                p.add_marker(position, name.unwrap_or_default());
                if let Some(m) = p.markers.iter_mut().find(|m| m.index == number) {
                    m.color = color;
                    m.region_end = end;
                }
            });
            Ok(info(session, number, end.is_some()))
        }
        MarkerCommand::Edit { marker, region, position, end, name, color: c } => {
            let at = position_of(&session.project, marker, region)?;
            let is_region = session.project.markers[at].region_end.is_some();
            if end.is_some() && !is_region {
                return Err(ApiError::invalid("markers have no end; only regions do"));
            }
            let position = position.map(|p| time(p, "position")).transpose()?;
            let color = color(c.as_deref())?;
            let m = &session.project.markers[at];
            let (start, stop) = (position.unwrap_or(m.position), end.or(m.region_end));
            if stop.is_some_and(|e| e <= start) {
                return Err(ApiError::invalid("a region ends after it starts"));
            }
            session.edit(if is_region { "Edit region" } else { "Edit marker" }, |p| {
                let m = &mut p.markers[at];
                m.position = start;
                m.region_end = stop;
                if let Some(n) = name {
                    m.name = n;
                }
                if c.is_some() {
                    m.color = color;
                }
                p.markers.sort_by(|a, b| a.position.total_cmp(&b.position));
            });
            Ok(info(session, marker, is_region))
        }
        MarkerCommand::Delete { marker, region } => {
            let at = position_of(&session.project, marker, region)?;
            let label =
                if session.project.markers[at].region_end.is_some() { "Delete region" } else { "Delete marker" };
            session.edit(label, |p| {
                p.markers.remove(at);
            });
            Ok(super::done())
        }
        MarkerCommand::Goto { marker, region } => {
            let at = position_of(&session.project, marker, region)?;
            let position = session.project.markers[at].position;
            session.set_cursor(position);
            Ok(json(snapshot::transport(session)))
        }
    }
}
