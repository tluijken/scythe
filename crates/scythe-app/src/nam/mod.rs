//! Neural Amp Modeler slots beyond playing them: the model library, TONE3000 and the work
//! behind the NAM panel, done on background threads and picked up each frame.

mod actions;
pub mod library;
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
    pub search: String,
}

enum Event {
    Scanned(Vec<Entry>),
    Model { target: Target, path: PathBuf, levels: Option<(f64, f64)>, result: Result<NamModel, String> },
    SignedIn(Result<Option<i64>, ApiError>),
    User(Result<User, ApiError>),
    Tones { source: Remote, page: u32, result: Result<Page<Tone>, ApiError> },
    Picked(Result<Tone, ApiError>),
    Models { tone_id: i64, result: Result<Vec<Model>, ApiError> },
    Downloaded { tone: Box<Tone>, model: Box<Model>, result: Result<Vec<u8>, ApiError>, then: Option<Target> },
    Favourite { tone_id: i64, favourite: bool, result: Result<(), ApiError> },
}

/// What the app has to do with finished background work.
pub enum Outcome {
    /// A model is read: hand it to the engine and point the slot at it.
    Load {
        target: Target,
        path: PathBuf,
        levels: Option<(f64, f64)>,
        model: NamModel,
    },
    /// A downloaded file to load into the slot (a model, or an impulse response).
    Open {
        target: Target,
        path: PathBuf,
    },
    Notify(String),
}

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
}

impl NamHub {
    pub fn new() -> Self {
        let config = Config::load();
        let client = Arc::new(Client::new(
            Arc::new(tone3000::UreqHttp::default()),
            &config,
            Some(TokenStore::default_location()),
        ));
        let (tx, rx) = channel();
        Self {
            library: Library::open(Library::default_dir()),
            config,
            client,
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
        }
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
        self.loading.insert(target.clone(), path.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = NamModel::load(&path).map_err(|e| e.to_string());
            let _ = tx.send(Event::Model { target, path, levels, result });
        });
    }

    pub fn has_client_id(&self) -> bool {
        self.config.client_id().is_some()
    }

    pub fn save_client_id(&mut self, id: &str) -> std::io::Result<()> {
        let id = id.trim();
        self.config.client_id = (!id.is_empty()).then(|| id.to_owned());
        self.config.save()?;
        self.client = Arc::new(Client::new(
            Arc::new(tone3000::UreqHttp::default()),
            &self.config,
            Some(TokenStore::default_location()),
        ));
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
                Event::Model { target, path, levels, result } => {
                    // Stepping quickly through models asks for several; only the last counts.
                    if self.loading.get(&target) != Some(&path) {
                        continue;
                    }
                    self.loading.remove(&target);
                    match result {
                        Ok(model) => {
                            self.library.touch(&path);
                            self.library_dirty = true;
                            out.push(Outcome::Load { target, path, levels, model });
                        }
                        Err(e) => out.push(Outcome::Notify(format!("Could not load {}: {e}", path.display()))),
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
                    if self.remote.tone.as_ref().map(|t| t.id) != Some(tone_id) {
                        continue;
                    }
                    self.remote.models_loading = false;
                    match result {
                        Ok(models) => self.remote.models = models,
                        Err(e) => self.remote.error = Some(e.to_string()),
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
}

/// Shared with the panel's drop handling and the arrange view.
pub fn is_model_file(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("nam"))
}
