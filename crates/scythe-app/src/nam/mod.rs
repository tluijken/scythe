//! Neural Amp Modeler slots beyond playing them: the model library, TONE3000 and the work
//! behind the NAM panel, done on background threads and picked up each frame.

mod actions;
pub mod library;
pub mod preview;
pub mod snapshots;
pub mod tone3000;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use scythe_core::TrackId;
use scythe_engine::nam::NamModel;

use library::{Entry, Filter, Library};
use preview::{Ending, Preview, Stage};
use tone3000::{ApiError, Authorization, Client, Config, Model, Page, Prompt, TokenStore, Tone, ToneList, User};

/// A NAM slot, by its track and FXID so it is found again after slots move.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    pub track: TrackId,
    pub fx_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserTab {
    Library,
    Recent,
    Tone3000,
}

/// What the TONE3000 tab lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remote {
    List(ToneList),
    Search(String),
}

/// The kinds of capture the TONE3000 list can be narrowed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gear {
    Amp,
    Pedal,
    FullRig,
    Ir,
}

impl Gear {
    pub const ALL: [Gear; 4] = [Self::Amp, Self::Pedal, Self::FullRig, Self::Ir];

    pub fn label(self) -> &'static str {
        match self {
            Self::Amp => "Amp",
            Self::Pedal => "Pedal",
            Self::FullRig => "Full rig",
            Self::Ir => "IR",
        }
    }

    pub fn matches(self, tone: &Tone) -> bool {
        let gear = tone.gear.as_deref().unwrap_or("");
        match self {
            Self::Ir => tone.is_ir(),
            Self::Amp => gear == "amp" && !tone.is_ir(),
            Self::Pedal => gear == "pedal",
            Self::FullRig => matches!(gear, "amp-cab" | "full-rig" | "amp-pedal-cab"),
        }
    }
}

/// The model sizes TONE3000 trains, largest first.
pub const SIZES: [&str; 4] = ["standard", "lite", "feather", "nano"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToneSort {
    /// As TONE3000 lists them.
    #[default]
    Listed,
    Downloads,
    Favourites,
    Title,
}

impl ToneSort {
    pub const ALL: [ToneSort; 4] = [Self::Listed, Self::Downloads, Self::Favourites, Self::Title];

    pub fn label(self) -> &'static str {
        match self {
            Self::Listed => "As listed",
            Self::Downloads => "Most downloaded",
            Self::Favourites => "Most favourited",
            Self::Title => "Title",
        }
    }
}

#[derive(Default)]
pub struct RemoteView {
    pub source: Option<Remote>,
    pub page: u32,
    pub total_pages: u32,
    pub tones: Vec<Tone>,
    pub loading: bool,
    pub error: Option<String>,
    /// The tone whose models are shown.
    pub tone: Option<Tone>,
    pub models: Vec<Model>,
    pub models_loading: bool,
    /// Why the tone's models could not be listed.
    pub models_error: Option<String>,
    pub search: String,
    pub gear: Option<Gear>,
    pub size: Option<&'static str>,
    pub sort: ToneSort,
    /// The tone's whole description is shown instead of its first lines.
    pub description_open: bool,
}

impl RemoteView {
    /// The listed tones that pass the gear and size filters, in the chosen order.
    pub fn shown(&self) -> Vec<&Tone> {
        let mut tones: Vec<&Tone> = self
            .tones
            .iter()
            .filter(|t| self.gear.is_none_or(|g| g.matches(t)))
            .filter(|t| self.size.is_none_or(|s| t.sizes.is_empty() || t.sizes.iter().any(|z| z == s)))
            .collect();
        match self.sort {
            ToneSort::Listed => {}
            ToneSort::Downloads => tones.sort_by_key(|t| std::cmp::Reverse(t.downloads_count.unwrap_or(0))),
            ToneSort::Favourites => tones.sort_by_key(|t| std::cmp::Reverse(t.favorites_count.unwrap_or(0))),
            ToneSort::Title => tones.sort_by_key(|t| t.title.to_lowercase()),
        }
        tones
    }
}

/// Why a model is being read, which decides what happens once it is.
#[derive(Debug, Clone, PartialEq)]
pub enum Purpose {
    /// Loaded for good, from the library, a file or a snapshot.
    Load,
    /// The preview's model with this id.
    Preview(i64),
    /// The slot's model from before a preview (as the slot names it), back for good or to
    /// compare.
    Restore(String),
}

enum Event {
    Scanned(Vec<Entry>),
    Model {
        target: Target,
        path: PathBuf,
        levels: Option<(f64, f64)>,
        purpose: Purpose,
        result: Result<NamModel, String>,
    },
    SignedIn(Result<Option<i64>, ApiError>),
    User(Result<User, ApiError>),
    Tones {
        source: Remote,
        page: u32,
        result: Result<Page<Tone>, ApiError>,
    },
    Picked(Result<Tone, ApiError>),
    Models {
        tone_id: i64,
        result: Result<Vec<Model>, ApiError>,
    },
    Downloaded {
        tone: Box<Tone>,
        model: Box<Model>,
        result: Result<Vec<u8>, ApiError>,
        then: Option<Target>,
    },
    PreviewFetched {
        target: Target,
        model: Box<Model>,
        result: Result<Vec<u8>, ApiError>,
    },
    Favourite {
        tone_id: i64,
        favourite: bool,
        result: Result<(), ApiError>,
    },
}

/// What the app has to do with finished background work.
pub enum Outcome {
    /// A model is read: hand it to the engine and point the slot at it.
    Load {
        target: Target,
        path: PathBuf,
        levels: Option<(f64, f64)>,
        purpose: Purpose,
        model: Box<NamModel>,
    },
    /// A downloaded file to load into the slot (a model, or an impulse response).
    Open {
        target: Target,
        path: PathBuf,
    },
    Notify(String),
}

/// Previewed models kept in the cache, so stepping back to one does not fetch it again.
const PREVIEW_CACHE: usize = 40;

pub struct NamHub {
    pub library: Library,
    pub config: Config,
    pub client: Arc<Client>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    pub scanning: bool,
    scanned: bool,
    pub filter: Filter,
    pub tab: BrowserTab,
    pub remote: RemoteView,
    pub user: Option<User>,
    /// Set to stop a sign-in waiting for the browser.
    pub signing_in: Option<Arc<AtomicBool>>,
    pub downloading: HashSet<i64>,
    /// Slots with a model being read, and the model each will switch to.
    pub loading: HashMap<Target, PathBuf>,
    /// The slot the NAM panel shows.
    pub panel: Option<Target>,
    /// Where the panel was last drawn, so files dropped on it go to it.
    pub panel_rect: Option<egui::Rect>,
    /// Text typed for the publishable key before it is saved.
    pub key_input: String,
    pub library_dirty: bool,
    /// The library entry shown beside the list.
    pub selected: Option<PathBuf>,
    pub preview: Option<Preview>,
    preview_dir: PathBuf,
    /// Models being fetched for a preview.
    pub fetching: HashSet<i64>,
    /// A tone stepped to while previewing; its first model plays once its models are listed.
    preview_next_tone: Option<i64>,
}

impl NamHub {
    pub fn new() -> Self {
        let config = Config::load();
        let (tx, rx) = channel();
        Self {
            client: Arc::new(Self::client(&config)),
            library: Library::open(Library::default_dir()),
            config,
            tx,
            rx,
            scanning: false,
            scanned: false,
            filter: Filter::default(),
            tab: BrowserTab::Library,
            remote: RemoteView::default(),
            user: None,
            signing_in: None,
            downloading: HashSet::new(),
            loading: HashMap::new(),
            panel: None,
            panel_rect: None,
            key_input: String::new(),
            library_dirty: false,
            selected: None,
            preview: None,
            preview_dir: preview::default_cache_dir(),
            fetching: HashSet::new(),
            preview_next_tone: None,
        }
    }

    fn client(config: &Config) -> Client {
        Client::new(Arc::new(tone3000::UreqHttp::default()), config, Some(TokenStore::default_location()))
            .with_debug_log(Library::default_dir().join("tone3000-debug.log"))
    }

    /// Reads the library's folders again in the background.
    pub fn rescan(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        self.scanned = true;
        let roots = self.library.roots();
        let known = self.library.entries().to_vec();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Event::Scanned(Library::scan(&roots, &known)));
        });
    }

    /// Reads the library once per session, when it is first shown.
    pub fn ensure_scanned(&mut self) {
        if !self.scanned {
            self.rescan();
            if self.client.signed_in() && self.user.is_none() {
                self.fetch_user();
            }
        }
    }

    /// Reads a model off the UI thread; the slot switches to it once it is ready.
    pub fn load_model(&mut self, target: Target, path: PathBuf, levels: Option<(f64, f64)>) {
        self.read_model(target, path, levels, Purpose::Load);
    }

    pub fn read_model(&mut self, target: Target, path: PathBuf, levels: Option<(f64, f64)>, purpose: Purpose) {
        self.loading.insert(target.clone(), path.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = NamModel::load(&path).map_err(|e| e.to_string());
            let _ = tx.send(Event::Model { target, path, levels, purpose, result });
        });
    }

    pub fn has_client_id(&self) -> bool {
        self.config.client_id().is_some()
    }

    pub fn save_client_id(&mut self, id: &str) -> std::io::Result<()> {
        let id = id.trim();
        self.config.client_id = (!id.is_empty()).then(|| id.to_owned());
        self.config.save()?;
        self.client = Arc::new(Self::client(&self.config));
        Ok(())
    }

    /// Opens TONE3000 in the browser to sign in, or to pick a tone there (which signs in too).
    pub fn sign_in(&mut self, prompt: Prompt) -> Result<(), ApiError> {
        if let Some(cancel) = self.signing_in.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let auth = Authorization::begin(&self.config, &prompt)?;
        tone3000::open_browser(&auth.url).map_err(|e| ApiError::Network(format!("opening the browser: {e}")))?;
        let cancel = Arc::new(AtomicBool::new(false));
        self.signing_in = Some(cancel.clone());
        let client = self.client.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = auth.wait(Duration::from_secs(600), &cancel).and_then(|(callback, pkce, redirect)| {
                client.exchange_code(&callback.code, &pkce.verifier, &redirect)?;
                Ok(callback.tone_id)
            });
            let _ = tx.send(Event::SignedIn(result));
        });
        Ok(())
    }

    pub fn cancel_sign_in(&mut self) {
        if let Some(cancel) = self.signing_in.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn sign_out(&mut self) {
        self.client.sign_out();
        self.user = None;
        self.remote = RemoteView::default();
    }

    fn spawn(&self, work: impl FnOnce(&Client) -> Event + Send + 'static) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work(&client));
        });
    }

    fn fetch_user(&self) {
        self.spawn(|c| Event::User(c.user()));
    }

    pub fn browse(&mut self, source: Remote, page: u32) {
        self.remote.loading = true;
        self.remote.error = None;
        self.remote.source = Some(source.clone());
        self.spawn(move |c| {
            let result = match &source {
                Remote::List(list) => c.tones(*list, page),
                Remote::Search(q) => c.search(q, page),
            };
            Event::Tones { source, page, result }
        });
    }

    pub fn open_tone(&mut self, tone: Tone) {
        self.remote.models.clear();
        self.remote.models_loading = true;
        self.remote.models_error = None;
        self.remote.description_open = false;
        let id = tone.id;
        self.remote.tone = Some(tone);
        self.spawn(move |c| Event::Models { tone_id: id, result: c.models(id) });
    }

    /// Downloads a model into the library, then loads it into `then` when given.
    pub fn download(&mut self, tone: &Tone, model: &Model, then: Option<Target>) {
        if !self.downloading.insert(model.id) {
            return;
        }
        let (tone, model) = (Box::new(tone.clone()), Box::new(model.clone()));
        self.spawn(move |c| {
            let result = c.download(&model);
            Event::Downloaded { tone, model, result, then }
        });
    }

    pub fn set_favourite(&mut self, tone_id: i64, favourite: bool) {
        self.spawn(move |c| Event::Favourite { tone_id, favourite, result: c.set_favorite(tone_id, favourite) });
    }

    /// Plays `model_id` of `tone` in the slot until it is kept or reverted. `previous` is the
    /// slot's model now; a preview already running on the slot keeps the one it started from.
    pub fn start_preview(
        &mut self,
        target: Target,
        previous: Option<String>,
        tone: &Tone,
        models: &[Model],
        model_id: i64,
    ) {
        self.preview_next_tone = None;
        let started = match &mut self.preview {
            Some(p) if p.target == target => p.switch(tone.clone(), models.to_vec(), model_id),
            _ => match Preview::new(target, previous, tone.clone(), models.to_vec(), model_id) {
                Some(p) => {
                    self.preview = Some(p);
                    true
                }
                None => false,
            },
        };
        if started {
            self.fetch_preview();
        }
    }

    /// Plays the model `by` places away among the previewed tone's models.
    pub fn step_preview(&mut self, by: isize) {
        if let Some(p) = &mut self.preview {
            p.step(by);
            self.fetch_preview();
        }
    }

    /// Moves the preview to the tone `by` places away in the list, starting with its first
    /// model once the tone's models are listed.
    pub fn step_preview_tone(&mut self, by: isize) {
        let Some(current) = self.preview.as_ref().map(|p| p.tone.id) else { return };
        let shown: Vec<Tone> = self.remote.shown().into_iter().cloned().collect();
        if shown.is_empty() {
            return;
        }
        let n = shown.len() as isize;
        let next = match shown.iter().position(|t| t.id == current) {
            Some(i) => (i as isize + by).rem_euclid(n),
            None if by < 0 => n - 1,
            None => 0,
        };
        let tone = shown[next as usize].clone();
        self.preview_next_tone = Some(tone.id);
        if let Some(p) = &mut self.preview {
            p.stage = Stage::Fetching;
        }
        self.open_tone(tone);
    }

    /// Reads the preview's model, fetching it into the cache first when it is not there yet.
    fn fetch_preview(&mut self) {
        let Some((target, model)) =
            self.preview.as_ref().filter(|p| !p.comparing).map(|p| (p.target.clone(), p.model().clone()))
        else {
            return;
        };
        let local = self.library.downloaded(model.id).map(|e| e.path.clone()).filter(|f| f.exists());
        let cached = preview::cache_file(&self.preview_dir, &model);
        let file = local.or_else(|| cached.exists().then_some(cached));
        if let Some(p) = &mut self.preview {
            p.stage = if file.is_some() { Stage::Loading } else { Stage::Fetching };
        }
        match file {
            Some(path) => self.read_model(target, path, None, Purpose::Preview(model.id)),
            None => {
                if self.fetching.insert(model.id) {
                    let model = Box::new(model);
                    self.spawn(move |c| {
                        let result = c.download(&model);
                        Event::PreviewFetched { target, model, result }
                    });
                }
            }
        }
    }

    /// Switches between the previewed model and the slot's earlier one; `resolve` turns the
    /// slot's model name into a file.
    pub fn toggle_compare(&mut self, resolve: impl Fn(&str) -> PathBuf) {
        let Some(p) = &mut self.preview else { return };
        if p.previous.is_none() {
            return;
        }
        if p.toggle_compare() {
            let target = p.target.clone();
            let previous = p.previous.clone().unwrap_or_default();
            self.read_model(target, resolve(&previous), None, Purpose::Restore(previous));
        } else {
            self.fetch_preview();
        }
    }

    /// Ends the preview, saying what is left to do with the slot.
    pub fn end_preview(&mut self, keep: bool) -> Option<Ending> {
        self.preview_next_tone = None;
        let p = self.preview.take()?;
        self.loading.remove(&p.target);
        Some(if keep { p.keep() } else { p.revert() })
    }

    /// Moves a previewed model from the cache into the library.
    pub fn keep_previewed(&mut self, tone: &Tone, model: &Model) -> Result<PathBuf, String> {
        if let Some(e) = self.library.downloaded(model.id).filter(|e| e.path.exists()) {
            return Ok(e.path.clone());
        }
        let cached = preview::cache_file(&self.preview_dir, model);
        let bytes = std::fs::read(&cached).map_err(|e| format!("the previewed file is gone: {e}"))?;
        let entry = self.library.store_download(tone, model, &bytes).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(cached);
        self.library_dirty = true;
        Ok(entry.path)
    }

    /// Handles finished background work.
    pub fn poll(&mut self) -> Vec<Outcome> {
        let mut out = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Scanned(entries) => {
                    self.scanning = false;
                    self.library.merge(entries);
                    self.library_dirty = true;
                }
                Event::Model { target, path, levels, purpose, result } => {
                    // Stepping quickly through models asks for several; only the last counts.
                    if self.loading.get(&target) != Some(&path) {
                        continue;
                    }
                    self.loading.remove(&target);
                    let preview = self.preview.as_mut().filter(|p| p.target == target);
                    match (&purpose, result) {
                        (Purpose::Preview(id), result) => {
                            let Some(p) = preview.filter(|p| p.wants(&target, *id)) else { continue };
                            match result {
                                Ok(model) => {
                                    p.stage = Stage::Playing;
                                    out.push(Outcome::Load { target, path, levels, purpose, model: Box::new(model) });
                                }
                                Err(e) => p.stage = Stage::Failed(e),
                            }
                        }
                        (Purpose::Restore(_), Ok(model)) => {
                            if preview.is_some_and(|p| !p.comparing) {
                                continue;
                            }
                            out.push(Outcome::Load { target, path, levels, purpose, model: Box::new(model) });
                        }
                        (Purpose::Load, Ok(model)) => {
                            self.library.touch(&path);
                            self.library_dirty = true;
                            out.push(Outcome::Load { target, path, levels, purpose, model: Box::new(model) });
                        }
                        (_, Err(e)) => out.push(Outcome::Notify(format!("Could not load {}: {e}", path.display()))),
                    }
                }
                Event::SignedIn(result) => {
                    self.signing_in = None;
                    match result {
                        Ok(picked) => {
                            out.push(Outcome::Notify("Signed in to TONE3000".into()));
                            self.fetch_user();
                            self.browse(Remote::List(ToneList::Favorited), 1);
                            if let Some(id) = picked {
                                self.spawn(move |c| Event::Picked(c.tone(id)));
                            }
                        }
                        Err(e) => out.push(Outcome::Notify(e.to_string())),
                    }
                }
                Event::User(result) => match result {
                    Ok(user) => self.user = Some(user),
                    Err(ApiError::SessionExpired | ApiError::NotSignedIn) => self.user = None,
                    Err(e) => self.remote.error = Some(e.to_string()),
                },
                Event::Tones { source, page, result } => {
                    if self.remote.source.as_ref() != Some(&source) {
                        continue;
                    }
                    self.remote.loading = false;
                    match result {
                        Ok(p) => {
                            self.remote.page = page;
                            self.remote.total_pages = p.total_pages.max(1);
                            self.remote.tones = p.data;
                        }
                        Err(e) => {
                            self.remote.tones.clear();
                            self.remote.error = Some(e.to_string());
                        }
                    }
                }
                Event::Picked(result) => match result {
                    Ok(tone) => {
                        self.tab = BrowserTab::Tone3000;
                        self.open_tone(tone);
                    }
                    Err(e) => out.push(Outcome::Notify(e.to_string())),
                },
                Event::Models { tone_id, result } => {
                    let Some(tone) = self.remote.tone.clone().filter(|t| t.id == tone_id) else { continue };
                    self.remote.models_loading = false;
                    match result {
                        Ok(models) => self.remote.models = models,
                        Err(e) => self.remote.models_error = Some(e.to_string()),
                    }
                    if self.preview_next_tone == Some(tone_id) {
                        self.preview_next_tone = None;
                        self.preview_first_of(tone);
                    }
                }
                Event::Downloaded { tone, model, result, then } => {
                    self.downloading.remove(&model.id);
                    let stored = result.map_err(|e| e.to_string()).and_then(|bytes| {
                        self.library.store_download(&tone, &model, &bytes).map_err(|e| e.to_string())
                    });
                    match stored {
                        Ok(entry) => {
                            self.library_dirty = true;
                            match then {
                                Some(target) => out.push(Outcome::Open { target, path: entry.path }),
                                None => out.push(Outcome::Notify(format!("Downloaded {}", entry.name))),
                            }
                        }
                        Err(e) => out.push(Outcome::Notify(format!("Could not download {}: {e}", model.name))),
                    }
                }
                Event::PreviewFetched { target, model, result } => {
                    self.fetching.remove(&model.id);
                    let file = preview::cache_file(&self.preview_dir, &model);
                    let stored = result.map_err(|e| e.to_string()).and_then(|bytes| {
                        if bytes.starts_with(b"RIFF") {
                            return Err("this is an impulse response, use Get and load".into());
                        }
                        std::fs::create_dir_all(&self.preview_dir).map_err(|e| e.to_string())?;
                        std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
                        preview::trim_cache(&self.preview_dir, PREVIEW_CACHE);
                        Ok(())
                    });
                    let Some(p) = self.preview.as_mut().filter(|p| p.wants(&target, model.id)) else { continue };
                    match stored {
                        Ok(()) => self.fetch_preview(),
                        Err(e) => p.stage = Stage::Failed(e),
                    }
                }
                Event::Favourite { tone_id, favourite, result } => match result {
                    Ok(()) => {
                        let tones = self.remote.tones.iter_mut().chain(self.remote.tone.as_mut());
                        for t in tones.filter(|t| t.id == tone_id) {
                            t.is_favorite = Some(favourite);
                        }
                    }
                    Err(e) => out.push(Outcome::Notify(e.to_string())),
                },
            }
        }
        if self.library_dirty && !self.scanning {
            self.library_dirty = false;
            if let Err(e) = self.library.save() {
                out.push(Outcome::Notify(format!("Could not save the NAM library: {e}")));
            }
        }
        out
    }

    /// Moves a running preview to the first model of `tone`, whose models are listed now.
    fn preview_first_of(&mut self, tone: Tone) {
        let first = preview::first_model(&self.remote.models, self.remote.size).map(|m| m.id);
        let models = self.remote.models.clone();
        let failure = self.remote.models_error.clone();
        let Some(p) = &mut self.preview else { return };
        match first {
            Some(id) => {
                if p.switch(tone, models, id) {
                    self.fetch_preview();
                }
            }
            None => {
                p.stage = Stage::Failed(failure.unwrap_or_else(|| format!("{} has no models to preview", tone.title)))
            }
        }
    }
}

/// Shared with the panel's drop handling and the arrange view.
pub fn is_model_file(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("nam"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(id: i64, gear: &str, sizes: &[&str], downloads: i64) -> Tone {
        Tone {
            id,
            title: format!("t{id}"),
            gear: Some(gear.into()),
            sizes: sizes.iter().map(|s| s.to_string()).collect(),
            downloads_count: Some(downloads),
            ..Tone::default()
        }
    }

    #[test]
    fn the_tone_list_filters_by_gear_and_size_and_sorts() {
        let mut view = RemoteView {
            tones: vec![
                tone(1, "amp", &["standard", "nano"], 5),
                tone(2, "pedal", &["lite"], 50),
                tone(3, "amp-cab", &[], 20),
                tone(4, "ir", &[], 1),
            ],
            ..RemoteView::default()
        };
        let ids = |v: &RemoteView| v.shown().iter().map(|t| t.id).collect::<Vec<_>>();
        assert_eq!(ids(&view), [1, 2, 3, 4]);
        view.sort = ToneSort::Downloads;
        assert_eq!(ids(&view), [2, 3, 1, 4]);
        view.gear = Some(Gear::FullRig);
        assert_eq!(ids(&view), [3]);
        view.gear = Some(Gear::Ir);
        assert_eq!(ids(&view), [4]);
        view.gear = None;
        view.size = Some("nano");
        assert_eq!(ids(&view), [3, 1, 4], "tones that do not say their sizes stay");
    }
}
