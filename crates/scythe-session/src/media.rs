use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use scythe_core::model::MIN_ITEM_LENGTH;
use scythe_core::rpp::resolve_media_path;
use scythe_core::smf::read_smf;
use scythe_core::{Item, ItemId, Source, Take, TimeRange, TrackId};
use scythe_engine::warp::Warp;
use scythe_engine::{Media, MediaState};

use crate::{Selection, Session};

pub enum LoaderMsg {
    Loaded {
        path: PathBuf,
        result: Result<Arc<Media>, String>,
        place: Option<(TrackId, f64)>,
    },
    Bounced(Result<PathBuf, String>),
    /// A saved subproject's proxy was rendered again.
    SubprojectRendered(PathBuf, Result<(), String>),
    /// Isolated items were rendered to `path` for gluing or applying track FX.
    ItemsRendered {
        path: PathBuf,
        result: Result<(), String>,
        then: RenderedItems,
    },
    /// A stretched, pitch-shifted or rate-changed copy is ready.
    Stretched(StretchKey),
}

/// What becomes of a render of isolated items.
pub enum RenderedItems {
    /// The items are replaced with one item playing the render.
    Glue { track: TrackId, items: Vec<ItemId>, range: TimeRange },
    /// The render becomes the item's new, active take.
    NewTake { item: ItemId },
}

/// A file and a hash of how a copy of it is processed.
pub(crate) type StretchKey = (PathBuf, u64);

/// A processed copy of a file being prepared.
enum Stretch {
    Processed(scythe_core::Processing),
    Warped(Box<Warp>),
}

fn hash(value: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::hash::DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
}

/// Files waiting to load, in the order they are wanted, and the threads loading them.
#[derive(Default)]
pub(crate) struct LoadQueue {
    jobs: VecDeque<Box<dyn FnOnce() + Send>>,
    threads: usize,
}

impl LoadQueue {
    /// A few files at a time, each converted on every core, so the files queued first are
    /// ready first instead of all of them at the end.
    const THREADS: usize = 4;

    fn push(queue: &Arc<Mutex<Self>>, job: Box<dyn FnOnce() + Send>) {
        let mut q = queue.lock().unwrap();
        q.jobs.push_back(job);
        if q.threads >= Self::THREADS {
            return;
        }
        q.threads += 1;
        let queue = queue.clone();
        std::thread::spawn(move || {
            loop {
                let job = {
                    let mut q = queue.lock().unwrap();
                    let job = q.jobs.pop_front();
                    if job.is_none() {
                        q.threads -= 1;
                    }
                    job
                };
                match job {
                    Some(job) => job(),
                    None => break,
                }
            }
        });
    }
}

impl Session {
    pub(crate) fn request_media(&mut self) {
        // What plays next loads first, then what lies behind the playhead: a set is playable
        // from where it stands long before its last song has loaded.
        let at = if self.is_playing() { self.engine.transport().position_seconds() } else { self.project.cursor };
        let mut paths: Vec<(bool, f64, PathBuf)> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| &t.items)
            .flat_map(|i| i.takes.iter().map(move |t| (i, t)))
            .filter_map(|(i, t)| {
                let path = resolve_media_path(&self.project, t.source.file()?);
                Some((i.position + i.length <= at, i.position, path))
            })
            .collect();
        paths.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        for (_, _, path) in paths {
            if self.engine.pool().get(&path).is_none() && !self.pending.contains(&path) {
                self.spawn_load(path, None);
            }
        }
        // The engine plays the project at its play rate, which changes how takes are processed.
        let played = self.project.at_playrate();
        let default_mode = played.default_pitch_mode;
        let mut jobs: Vec<(PathBuf, Stretch)> = Vec::new();
        for item in played.tracks.iter().flat_map(|t| &t.items) {
            for take in &item.takes {
                let Some(file) = take.source.file() else { continue };
                let path = resolve_media_path(&played, file);
                if let Some(p) = take.processing(default_mode) {
                    jobs.push((path.clone(), Stretch::Processed(p)));
                }
                if let Some(w) = Warp::of(item, take, default_mode) {
                    jobs.push((path, Stretch::Warped(Box::new(w))));
                }
            }
        }
        for (path, job) in jobs {
            let pool = self.engine.pool().clone();
            let (key, done) = match &job {
                Stretch::Processed(p) => {
                    ((path.clone(), hash(&("processed", p.key()))), pool.processed(&path, p).is_some())
                }
                Stretch::Warped(w) => ((path.clone(), hash(&("warped", w.key()))), pool.warped(&path, w).is_some()),
            };
            if pool.ready(&path).is_none() || done || !self.pending_stretch.insert(key.clone()) {
                continue;
            }
            let tx = self.loader_tx.clone();
            let wake = self.waker.clone();
            std::thread::spawn(move || {
                match &job {
                    Stretch::Processed(p) => {
                        pool.prepare_processed(&path, p);
                    }
                    Stretch::Warped(w) => {
                        pool.prepare_warped(&path, w);
                    }
                }
                let _ = tx.send(LoaderMsg::Stretched(key));
                wake();
            });
        }
    }

    fn spawn_load(&mut self, path: PathBuf, place: Option<(TrackId, f64)>) {
        self.pending.insert(path.clone());
        let pool = self.engine.pool().clone();
        let rate = self.engine.sample_rate();
        let tx = self.loader_tx.clone();
        let wake = self.waker.clone();
        LoadQueue::push(
            &self.load_queue,
            Box::new(move || {
                let result = pool.load(&path, rate).map_err(|e| e.to_string());
                let _ = tx.send(LoaderMsg::Loaded { path, result, place });
                wake();
            }),
        );
    }

    pub fn media(&self, take: &Take) -> Option<MediaState> {
        take.source.file().and_then(|p| self.engine.pool().get(&resolve_media_path(&self.project, p)))
    }

    pub fn media_loading(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Imports audio and MIDI files onto `track` (or a new track) starting at `position`.
    pub fn import_files(&mut self, files: Vec<PathBuf>, track: Option<TrackId>, position: f64) {
        for (i, path) in files.into_iter().enumerate() {
            let track = match (track, i) {
                (Some(t), 0) => t,
                _ => {
                    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let mut id = TrackId(0);
                    self.edit("Add track", |p| id = p.add_track(name));
                    id
                }
            };
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi")) {
                self.import_midi(&path, track, position);
            } else {
                self.spawn_load(path, Some((track, position)));
            }
        }
    }

    /// Places a MIDI file as an in-project MIDI item (REAPER's default for imported MIDI), its
    /// length following the project tempo map.
    fn import_midi(&mut self, path: &Path, track: TrackId, position: f64) {
        let item = match self.midi_item(path, position) {
            Ok(item) => item,
            Err(e) => {
                self.notify(format!("Could not load {}: {e}", path.display()));
                return;
            }
        };
        let id = item.id;
        self.edit("Import MIDI", |p| {
            if let Some(t) = p.track_mut(track) {
                t.items.push(item);
            }
        });
        self.select = Some(Selection { track: Some(track), item: Some(id) });
    }

    /// An in-project MIDI item of the file at `position`, its length following the tempo map.
    pub(crate) fn midi_item(&self, path: &Path, position: f64) -> Result<Item, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let (source, _) = read_smf(&bytes).map_err(|e| e.to_string())?;
        let timeline = self.project.tempo.timeline();
        let end = timeline.time_at_qn(timeline.qn_at(position) + source.length_qn());
        let mut take = Take::from_file(path.to_owned());
        take.source = Source::Raw(source.to_chunk());
        Ok(Item::new(position, (end - position).max(MIN_ITEM_LENGTH), take))
    }

    /// Takes in what background work finished, follows up on crossed marker actions, and
    /// auto-saves when it is time.
    pub fn poll(&mut self) {
        self.watch_device();
        self.autosave();
        self.handle_marker_actions();
        while let Ok(msg) = self.loader_rx.try_recv() {
            match msg {
                LoaderMsg::Loaded { path, result, place } => {
                    self.pending.remove(&path);
                    match result {
                        Ok(media) => {
                            if let Some((track, position)) = place {
                                let length = media.duration();
                                let item = Item::new(position, length, Take::from_file(path.clone()));
                                let id = item.id;
                                self.edit("Import media", |p| {
                                    if let Some(t) = p.track_mut(track) {
                                        t.items.push(item);
                                    }
                                });
                                self.select = Some(Selection { track: Some(track), item: Some(id) });
                            }
                            self.engine.sync(&self.project);
                            self.request_media();
                        }
                        Err(e) => self.notify(format!("Could not load {}: {e}", path.display())),
                    }
                }
                LoaderMsg::Stretched(key) => {
                    self.pending_stretch.remove(&key);
                    self.engine.sync(&self.project);
                }
                LoaderMsg::ItemsRendered { path, result, then } => {
                    self.items_rendering = self.items_rendering.saturating_sub(1);
                    if self.items_rendering == 0 {
                        self.busy = None;
                    }
                    match result {
                        Ok(()) => self.place_rendered_items(path, then),
                        Err(e) => self.notify(format!("Render failed: {e}")),
                    }
                }
                LoaderMsg::SubprojectRendered(path, result) => match result {
                    Ok(()) => {
                        self.notify(format!("Rendered subproject proxy for {}", path.display()));
                        self.sync();
                    }
                    Err(e) => self.notify(format!("Subproject render failed: {e}")),
                },
                LoaderMsg::Bounced(result) => {
                    self.busy = None;
                    match result {
                        Ok(path) => self.notify(format!("Rendered {}", path.display())),
                        Err(e) => self.notify(format!("Render failed: {e}")),
                    }
                }
            }
        }
    }

    fn place_rendered_items(&mut self, path: PathBuf, then: RenderedItems) {
        match then {
            RenderedItems::Glue { track, items, range } => {
                let mut glued = Item::new(range.start, range.length(), Take::from_file(path));
                (glued.fade_in.length, glued.fade_out.length) = (0.0, 0.0);
                let id = glued.id;
                self.edit("Glue items", |p| {
                    if let Some(t) = p.track_mut(track) {
                        t.items.retain(|it| !items.contains(&it.id));
                        t.items.push(glued);
                    }
                });
                self.select = Some(Selection { track: Some(track), item: Some(id) });
            }
            RenderedItems::NewTake { item } => {
                self.edit("Apply track FX to item as new take", |p| {
                    if let Some(it) = p.tracks.iter_mut().find_map(|t| t.item_mut(item)) {
                        it.takes.push(Take::from_file(path));
                        it.active_take = it.takes.len() - 1;
                    }
                });
            }
        }
        self.request_media();
    }
}
