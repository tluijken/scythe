use std::path::PathBuf;

use schemars::JsonSchema;
use scythe_core::model::RegionRenderTrack;
use scythe_core::render_settings::{RenderSettings, RenderSource, set_project_field};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json as value};

use super::{ApiError, Target, TrackRef, json};
use crate::{LoaderMsg, Session};

/// What a render plays (REAPER's render dialog "Source").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Master,
    /// The master mix and a stem per selected track.
    MasterAndStems,
    /// A stem per selected track: its own output, without folders or the master.
    Stems,
    /// Each selected track soloed through the master.
    TracksViaMaster,
    /// Each selected item alone through its track.
    Items,
    /// Each selected item alone through the master.
    ItemsViaMaster,
    /// The region render matrix: per region, the tracks (and the master) ticked for it.
    Matrix,
}

impl Source {
    fn flags(self) -> i64 {
        match self {
            Self::Master => 0,
            Self::MasterAndStems => 1,
            Self::Stems => 3,
            Self::TracksViaMaster => 128,
            Self::Items => 32,
            Self::ItemsViaMaster => 64,
            Self::Matrix => 8,
        }
    }
}

/// The time span a render covers (REAPER's "Bounds").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Bounds {
    Project,
    TimeSelection,
    Regions,
    SelectedItems,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum RenderCommand {
    /// The files the project's render settings write now, with their time spans, what each
    /// plays and the sample rate; nothing is rendered.
    #[serde(rename = "render.jobs")]
    Jobs,
    /// Changes the project's render settings; what is left out stays.
    #[serde(rename = "render.settings")]
    Settings {
        #[serde(default)]
        source: Option<Source>,
        #[serde(default)]
        bounds: Option<Bounds>,
        /// File name pattern with wildcards such as `$project`, `$track`, `$region`,
        /// `$regionnumber`, `$item`.
        #[serde(default)]
        pattern: Option<String>,
        /// Output folder (or file); relative to the project's folder.
        #[serde(default)]
        file: Option<String>,
        /// Sample rate in Hz (REAPER's `RENDER_SRATE`); 0 renders at the project's rate.
        #[serde(default)]
        sample_rate: Option<u32>,
    },
    /// Ticks (or unticks) a track, or the master, for a region in the region render matrix.
    #[serde(rename = "render.matrix")]
    Matrix {
        /// The region's number.
        region: u32,
        /// A track id, name or "master".
        track: TrackRef,
        on: bool,
    },
    /// Renders with the project's render settings in the background.
    #[serde(rename = "render.project")]
    Project,
    /// Adds the project, as it is now, to the render queue.
    #[serde(rename = "render.queue")]
    Queue {
        /// The queue folder; REAPER's `QueuedRenders` when left out.
        #[serde(default)]
        dir: Option<PathBuf>,
    },
    /// The queued projects, oldest first.
    #[serde(rename = "render.queue_list")]
    QueueList {
        #[serde(default)]
        dir: Option<PathBuf>,
    },
    /// Renders every queued project in the background; each one that renders leaves the queue.
    #[serde(rename = "render.queue_run")]
    QueueRun {
        #[serde(default)]
        dir: Option<PathBuf>,
    },
}

fn queue_dir(dir: Option<PathBuf>) -> Result<PathBuf, ApiError> {
    dir.or_else(scythe_core::render_queue::default_dir)
        .ok_or_else(|| ApiError::failed("no folder for the render queue"))
}

fn source_json(session: &Session, source: RenderSource) -> Value {
    let p = &session.project;
    match source {
        RenderSource::Master => value!({ "kind": "master" }),
        RenderSource::Stem(t) => value!({ "kind": "stem", "track": t.0 }),
        RenderSource::TrackViaMaster(t) => value!({ "kind": "track_via_master", "track": t.0 }),
        RenderSource::Item(i) => value!({ "kind": "item", "track": p.find_item(i).map(|(t, _)| t.0) }),
        RenderSource::ItemViaMaster(i) => {
            value!({ "kind": "item_via_master", "track": p.find_item(i).map(|(t, _)| t.0) })
        }
    }
}

pub fn execute(session: &mut Session, command: RenderCommand) -> Result<Value, ApiError> {
    match command {
        RenderCommand::Jobs => {
            let settings = RenderSettings::from_project(&session.project);
            let jobs = settings.jobs(&session.project, &session.render_selection(), 0).map_err(ApiError::failed)?;
            let rate = crate::render::render_rate(&settings, &session.project);
            Ok(Value::Array(
                jobs.iter()
                    .map(|j| {
                        value!({
                            "path": j.path,
                            "start": j.range.start,
                            "end": j.range.end,
                            "source": source_json(session, j.source),
                            "sample_rate": rate,
                        })
                    })
                    .collect(),
            ))
        }
        RenderCommand::Settings { source, bounds, pattern, file, sample_rate } => {
            if sample_rate.is_some_and(|r| r != 0 && !(8_000..=768_000).contains(&r)) {
                return Err(ApiError::invalid("render sample rates run from 8000 to 768000 Hz (0 for the project's)"));
            }
            session.edit("Render settings", |p| {
                if let Some(s) = source {
                    let keep = RenderSettings::from_project(p).source.0 & !(1 | 2 | 8 | 32 | 64 | 128);
                    set_project_field(p, "RENDER_STEMS", &["0"], 1, &(keep | s.flags()).to_string());
                }
                if let Some(b) = bounds {
                    let code = match b {
                        Bounds::Project => 1,
                        Bounds::TimeSelection => 2,
                        Bounds::Regions => 3,
                        Bounds::SelectedItems => 4,
                    };
                    set_project_field(p, "RENDER_RANGE", &["1", "0", "0", "0", "1000"], 1, &code.to_string());
                }
                if let Some(pat) = &pattern {
                    set_project_field(p, "RENDER_PATTERN", &[""], 1, pat);
                }
                if let Some(f) = &file {
                    set_project_field(p, "RENDER_FILE", &[""], 1, f);
                }
                if let Some(rate) = sample_rate {
                    set_project_field(p, "RENDER_FMT", &["0", "2", "0"], 3, &rate.to_string());
                }
            });
            execute(session, RenderCommand::Jobs).or(Ok(super::done()))
        }
        RenderCommand::Matrix { region, track, on } => {
            let target = track.resolve(&session.project)?;
            let Some(at) = session.project.markers.iter().position(|m| m.index == region && m.region_end.is_some())
            else {
                return Err(ApiError::not_found(format!("no region {region}")));
            };
            session.edit("Region render matrix", |p| {
                let guid = match target {
                    Target::Master => None,
                    Target::Track(t) => p.track(t).map(|t| t.guid.clone()),
                };
                let m = &mut p.markers[at];
                match guid {
                    None => m.set_renders_master(on),
                    Some(g) => {
                        m.render_matrix.retain(|r| !r.guid.eq_ignore_ascii_case(&g));
                        if on {
                            m.render_matrix.push(RegionRenderTrack { guid: g, channels: 0 });
                        }
                    }
                }
            });
            Ok(super::done())
        }
        RenderCommand::Project => {
            let project = session.project.clone();
            let selection = session.render_selection();
            let tx = session.loader_tx.clone();
            session.busy = Some("Rendering…".into());
            std::thread::spawn(move || {
                let result = crate::render::render_project(&project, &selection)
                    .and_then(|files| files.last().cloned().ok_or_else(|| "nothing rendered".into()));
                let _ = tx.send(LoaderMsg::Bounced(result));
            });
            Ok(super::done())
        }
        RenderCommand::Queue { dir } => {
            let dir = queue_dir(dir)?;
            let path = session.queue_render(Some(&dir), &session.render_selection()).map_err(ApiError::failed)?;
            Ok(value!({ "queued": path }))
        }
        RenderCommand::QueueList { dir } => {
            let dir = queue_dir(dir)?;
            Ok(json(scythe_core::render_queue::list(&dir)))
        }
        RenderCommand::QueueRun { dir } => {
            let dir = queue_dir(dir)?;
            let tx = session.loader_tx.clone();
            session.busy = Some("Rendering the queue…".into());
            std::thread::spawn(move || {
                let run = crate::render::run_queue(&dir);
                let result = match run.failed.first() {
                    Some((file, e)) => {
                        Err(format!("{} queued renders failed ({}: {e})", run.failed.len(), file.display()))
                    }
                    None => run.rendered.last().cloned().ok_or_else(|| "the render queue is empty".into()),
                };
                let _ = tx.send(LoaderMsg::Bounced(result));
            });
            Ok(super::done())
        }
    }
}
