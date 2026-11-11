//! The installed CLAP, LV2 and VST 3 plugins, found on a background thread so asking for them
//! never waits on a scan. What earlier runs found comes from the scan cache straight away.
//! CLAP and VST 3 binaries are loaded in a child process (`<scythe> --scan-plugin …`) with a
//! time limit, so a plugin that hangs or crashes while it is scanned only fails its own
//! entry; failures are kept in the cache like REAPER marks them, until the binary changes or
//! a full rescan.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::clap_host::ClapDescriptor;
use crate::lv2::Lv2Info;
use crate::scan_cache::{Outcome, ScanCache};
use crate::vst3_host::Vst3Info;

/// The first line a scan helper writes, before it loads anything: a program that does not
/// write it does not know `--scan-plugin`, and the scan happens in this process instead.
const MAGIC: &str = "scythe-plugin-scan 1";

/// Whether plugins are looked for in the user and system folders besides the `*_PATH`
/// variables: `SCYTHE_SYSTEM_PLUGINS=0` keeps tests away from what the machine has installed.
pub fn system_paths() -> bool {
    std::env::var_os("SCYTHE_SYSTEM_PLUGINS").is_none_or(|v| v != "0")
}

/// Where and how to scan.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub clap: Vec<PathBuf>,
    pub lv2: Vec<PathBuf>,
    pub vst3: Vec<PathBuf>,
    /// The folder of `clap.json`, `lv2.json` and `vst3.json`; `None` keeps nothing.
    pub cache_dir: Option<PathBuf>,
    /// The program run as `<helper> --scan-plugin <format> <path> …`; `None` loads binaries in
    /// this process.
    pub helper: Option<PathBuf>,
    /// How long one binary may take before it is marked as failed.
    pub timeout: Duration,
}

impl ScanConfig {
    /// The standard folders and `$XDG_CACHE_HOME/scythe`; this program (or
    /// `SCYTHE_SCAN_HELPER`) scans binaries.
    pub fn from_env() -> Self {
        let helper = std::env::var_os("SCYTHE_SCAN_HELPER").map(PathBuf::from).or_else(|| std::env::current_exe().ok());
        Self {
            clap: crate::clap_host::search_paths(),
            lv2: crate::lv2::search_paths(),
            vst3: crate::vst3_host::search_paths(),
            cache_dir: crate::scan_cache::default_path("x").and_then(|p| p.parent().map(Path::to_owned)),
            helper,
            timeout: Duration::from_secs(30),
        }
    }

    fn cache(&self, name: &str) -> Option<PathBuf> {
        self.cache_dir.as_ref().map(|d| d.join(format!("{name}.json")))
    }
}

/// How far a scan has got.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Progress {
    /// Binaries and bundles checked so far.
    pub done: usize,
    pub total: usize,
    /// The one being checked.
    pub current: Option<PathBuf>,
}

/// A binary or bundle that could not be scanned.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScanFailure {
    pub format: &'static str,
    pub path: PathBuf,
    pub reason: String,
}

/// The plugins known at one moment.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub clap: Arc<Vec<ClapDescriptor>>,
    pub lv2: Arc<Vec<Lv2Info>>,
    pub vst3: Arc<Vec<Vst3Info>>,
    pub scanning: bool,
    pub progress: Progress,
    pub failures: Arc<Vec<ScanFailure>>,
}

#[derive(Default)]
struct State {
    snapshot: Snapshot,
    started: bool,
    /// A rescan asked for while one runs, and whether it clears the cache.
    again: Option<bool>,
}

pub struct Catalog {
    config: Option<ScanConfig>,
    state: Mutex<State>,
    changed: Condvar,
    no_helper: AtomicBool,
}

impl Catalog {
    /// A catalog that scans as `config` says, once something asks for it.
    pub fn new(config: ScanConfig) -> Arc<Self> {
        Arc::new(Self {
            config: Some(config),
            state: Default::default(),
            changed: Condvar::new(),
            no_helper: AtomicBool::new(false),
        })
    }

    /// A catalog that never scans and holds what the `set_*` calls put in.
    pub fn fixed() -> Arc<Self> {
        let state = State { started: true, ..Default::default() };
        Arc::new(Self {
            config: None,
            state: Mutex::new(state),
            changed: Condvar::new(),
            no_helper: AtomicBool::new(true),
        })
    }

    /// The process's catalog of the standard folders.
    pub fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<Catalog>> = OnceLock::new();
        GLOBAL.get_or_init(|| Catalog::new(ScanConfig::from_env())).clone()
    }

    pub fn set_clap(&self, plugins: Vec<ClapDescriptor>) {
        self.state.lock().unwrap().snapshot.clap = Arc::new(plugins);
    }

    pub fn set_lv2(&self, plugins: Vec<Lv2Info>) {
        self.state.lock().unwrap().snapshot.lv2 = Arc::new(plugins);
    }

    pub fn set_vst3(&self, plugins: Vec<Vst3Info>) {
        self.state.lock().unwrap().snapshot.vst3 = Arc::new(plugins);
    }

    /// What is known now; the first call starts the scan and returns what the cache holds.
    pub fn snapshot(self: &Arc<Self>) -> Snapshot {
        let mut state = self.state.lock().unwrap();
        if !state.started {
            state.started = true;
            self.start(&mut state, false);
        }
        state.snapshot.clone()
    }

    /// Scans again (checking every binary anew with `clear`), or after the running scan.
    pub fn rescan(self: &Arc<Self>, clear: bool) {
        let mut state = self.state.lock().unwrap();
        if self.config.is_none() {
            return;
        }
        state.started = true;
        if state.snapshot.scanning {
            state.again = Some(clear || state.again == Some(true));
        } else {
            self.start(&mut state, clear);
        }
    }

    /// Waits up to `limit` for a running scan to end; true when none runs any more.
    pub fn wait(&self, limit: Duration) -> bool {
        let state = self.state.lock().unwrap();
        let (state, _) = self.changed.wait_timeout_while(state, limit, |s| s.snapshot.scanning).unwrap();
        !state.snapshot.scanning
    }

    fn start(self: &Arc<Self>, state: &mut State, clear: bool) {
        let Some(config) = &self.config else { return };
        if state.snapshot.clap.is_empty() && state.snapshot.lv2.is_empty() && state.snapshot.vst3.is_empty() {
            let cached = |name| config.cache(name);
            state.snapshot.clap = Arc::new(ScanCache::<ClapDescriptor>::open(cached("clap")).all().cloned().collect());
            state.snapshot.lv2 = Arc::new(ScanCache::<Lv2Info>::open(cached("lv2")).all().cloned().collect());
            state.snapshot.vst3 = Arc::new(ScanCache::<Vst3Info>::open(cached("vst3")).all().cloned().collect());
        }
        state.snapshot.scanning = true;
        state.snapshot.progress = Progress::default();
        let me = self.clone();
        let spawned = std::thread::Builder::new().name("plugin scan".into()).spawn(move || {
            let mut clear = clear;
            loop {
                me.run(clear);
                let mut state = me.state.lock().unwrap();
                match state.again.take() {
                    Some(c) => clear = c,
                    None => {
                        state.snapshot.scanning = false;
                        state.snapshot.progress.current = None;
                        me.changed.notify_all();
                        return;
                    }
                }
            }
        });
        if spawned.is_err() {
            state.snapshot.scanning = false;
        }
    }

    fn run(&self, clear: bool) {
        let config = self.config.as_ref().expect("only scanning catalogs start");
        let clap = crate::clap_host::bundles(&config.clap);
        let lv2 = crate::lv2::bundles(&config.lv2);
        let vst3 = crate::vst3_host::modules(&config.vst3);
        {
            let mut state = self.state.lock().unwrap();
            state.snapshot.progress = Progress { done: 0, total: clap.len() + lv2.len() + vst3.len(), current: None };
        }
        let mut failures = Vec::new();
        let found = self.scan_format(config.cache("clap"), clear, "CLAP", &clap, &mut failures, |p| {
            self.in_helper(config, &["clap"], p, || crate::clap_host::scan_bundle(p))
        });
        self.publish(|s| s.clap = Arc::new(found));
        let found = self.scan_format(config.cache("lv2"), clear, "LV2", &lv2, &mut failures, crate::lv2::load_bundle);
        self.publish(|s| s.lv2 = Arc::new(found));
        let binaries: Vec<PathBuf> = vst3.iter().map(|m| m.binary.clone()).collect();
        let mut found = self.scan_format(config.cache("vst3"), clear, "VST3", &binaries, &mut failures, |p| {
            let file_name = vst3.iter().find(|m| m.binary == p).map(|m| m.file_name.clone()).unwrap_or_default();
            self.in_helper(config, &["vst3", &file_name], p, || crate::vst3_host::scan_module(p, &file_name))
        });
        found.sort_by_key(|p| p.name.to_lowercase());
        self.publish(|s| {
            s.vst3 = Arc::new(found);
            s.failures = Arc::new(failures);
        });
    }

    fn publish(&self, f: impl FnOnce(&mut Snapshot)) {
        f(&mut self.state.lock().unwrap().snapshot);
        self.changed.notify_all();
    }

    fn scan_format<T: Clone + Serialize + DeserializeOwned>(
        &self,
        cache: Option<PathBuf>,
        clear: bool,
        format: &'static str,
        paths: &[PathBuf],
        failures: &mut Vec<ScanFailure>,
        scan: impl Fn(&Path) -> Outcome<T>,
    ) -> Vec<T> {
        let mut cache = if clear { ScanCache::<T>::empty(cache) } else { ScanCache::<T>::open(cache) };
        let mut out = Vec::new();
        let mut saved = Instant::now();
        for path in paths {
            self.publish(|s| s.progress.current = Some(path.clone()));
            let Some(stamp) = crate::scan_cache::stamp(path) else { continue };
            let outcome = match cache.lookup(path, &stamp) {
                Some(outcome) => outcome,
                None => {
                    let outcome = scan(path);
                    if let Err(e) = &outcome {
                        eprintln!("plugin scan: {}: {e}", path.display());
                    }
                    cache.insert(path, stamp, &outcome);
                    // What was found so far survives Scythe being closed during a long scan.
                    if saved.elapsed() > Duration::from_secs(2) {
                        cache.save();
                        saved = Instant::now();
                    }
                    outcome
                }
            };
            match outcome {
                Ok(mut found) => out.append(&mut found),
                Err(reason) => failures.push(ScanFailure { format, path: path.clone(), reason }),
            }
            self.publish(|s| s.progress.done += 1);
        }
        cache.save();
        out
    }

    /// Scans `path` in a helper process, or with `local` when there is no helper.
    fn in_helper<T: DeserializeOwned>(
        &self,
        config: &ScanConfig,
        args: &[&str],
        path: &Path,
        local: impl FnOnce() -> Outcome<T>,
    ) -> Outcome<T> {
        let Some(helper) = config.helper.as_ref().filter(|_| !self.no_helper.load(Ordering::Relaxed)) else {
            return local();
        };
        match run_helper(helper, args, path, config.timeout) {
            Helper::Answered(json) => serde_json::from_str::<Outcome<T>>(&json)
                .unwrap_or_else(|e| Err(format!("unreadable scan result: {e}"))),
            Helper::Failed(reason) => Err(reason),
            Helper::Unsupported => {
                self.no_helper.store(true, Ordering::Relaxed);
                local()
            }
        }
    }
}

enum Helper {
    /// The helper wrote its result line.
    Answered(String),
    Failed(String),
    /// The program does not know `--scan-plugin`.
    Unsupported,
}

fn run_helper(helper: &Path, args: &[&str], path: &Path, timeout: Duration) -> Helper {
    let mut cmd = Command::new(helper);
    cmd.arg("--scan-plugin").arg(args[0]).arg(path).args(&args[1..]);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return Helper::Unsupported,
    };
    let mut stdout = child.stdout.take().expect("piped");
    let (tx, rx) = std::sync::mpsc::channel();
    // A plugin that forks keeps the pipe open after the helper ends; this thread then
    // lingers instead of the scan.
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break None,
        }
    };
    let out = rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    let mut lines = out.lines();
    let greeted = lines.next() == Some(MAGIC);
    let answer = lines.next().filter(|l| !l.is_empty());
    match (status, greeted, answer) {
        (_, true, Some(json)) => Helper::Answered(json.to_owned()),
        (None, _, _) => Helper::Failed(format!("timed out after {} s", timeout.as_secs_f32())),
        (Some(status), true, None) => Helper::Failed(describe(status)),
        (Some(status), false, _) if std::os::unix::process::ExitStatusExt::signal(&status).is_some() => {
            Helper::Failed(describe(status))
        }
        (Some(_), false, _) => Helper::Unsupported,
    }
}

fn describe(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.signal(), status.code()) {
        (Some(signal), _) => format!("crashed (signal {signal})"),
        (_, Some(code)) => format!("exited with status {code} while scanning"),
        _ => "ended while scanning".into(),
    }
}

/// Runs the scan helper when `args` ask for it (`--scan-plugin clap <path>` or
/// `--scan-plugin vst3 <path> <file name>`), answering with the exit code; `None` otherwise.
/// Programs that host plugins call this first thing in `main`.
pub fn helper_main(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) != Some("--scan-plugin") {
        return None;
    }
    let (Some(format), Some(path)) = (args.get(2), args.get(3).map(PathBuf::from)) else { return Some(2) };
    // Plugins print to stdout; the answer goes to the original stdout, everything else to
    // stderr.
    // SAFETY: plain descriptor duplication on this process's own standard streams.
    let answer = unsafe {
        let fd = libc::dup(1);
        libc::dup2(2, 1);
        fd
    };
    if answer < 0 {
        return Some(2);
    }
    // SAFETY: `answer` is a descriptor this function owns.
    let mut out = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(answer) };
    let _ = writeln!(out, "{MAGIC}");
    let _ = out.flush();
    let json = match format.as_str() {
        "clap" => match crate::clap_host::ClapLibrary::load(&path) {
            Ok(lib) => {
                let json = serde_json::to_string(&Ok::<_, String>(lib.descriptors()));
                // Unloading runs the plugin's teardown, which may crash after the answer is
                // known: the process ends without it.
                std::mem::forget(lib);
                json
            }
            Err(e) => serde_json::to_string(&Err::<Vec<ClapDescriptor>, _>(e.to_string())),
        },
        "vst3" => {
            let file_name = args.get(4).cloned().unwrap_or_default();
            match crate::vst3_host::Vst3Module::load(&path) {
                Ok(module) => {
                    let json = serde_json::to_string(&Ok::<_, String>(module.classes(&file_name)));
                    std::mem::forget(module);
                    json
                }
                Err(e) => serde_json::to_string(&Err::<Vec<Vst3Info>, _>(e)),
            }
        }
        _ => return Some(2),
    };
    let _ = writeln!(out, "{}", json.unwrap_or_default());
    let _ = out.flush();
    // SAFETY: ends the process without running exit handlers the plugin may have installed.
    unsafe { libc::_exit(0) }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A folder of fake `.clap` files and a helper script that answers for them: `ok*.clap`
    /// holds one plugin, `hang*.clap` never answers and `crash*.clap` crashes. The helper
    /// appends each path it is asked about to `calls`.
    pub(crate) fn fixture(name: &str) -> (PathBuf, ScanConfig) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("scythe-plugin-scan-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plugins = dir.join("clap");
        std::fs::create_dir_all(&plugins).unwrap();
        let helper = dir.join("helper.sh");
        let script = format!(
            r#"#!/bin/sh
echo "$3" >> "{calls}"
echo '{MAGIC}'
case "$(basename "$3")" in
  ok*) echo '{{"Ok":[{{"id":"test.'$(basename "$3" .clap)'","name":"Fine","vendor":"Test","features":["audio-effect"],"path":"'$3'"}}]}}' ;;
  hang*) sleep 30 ;;
  crash*) kill -SEGV $$ ;;
esac
"#,
            calls = dir.join("calls").display()
        );
        std::fs::write(&helper, script).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = ScanConfig {
            clap: vec![plugins],
            lv2: Vec::new(),
            vst3: Vec::new(),
            cache_dir: Some(dir.join("cache")),
            helper: Some(helper),
            timeout: Duration::from_millis(500),
        };
        (dir, config)
    }

    fn calls(dir: &Path) -> usize {
        std::fs::read_to_string(dir.join("calls")).map(|s| s.lines().count()).unwrap_or(0)
    }

    #[test]
    fn helper_answers_times_out_crashes_or_is_missing() {
        let (dir, config) = fixture("helper");
        let helper = config.helper.as_ref().unwrap();
        let clap = |name: &str| dir.join("clap").join(name);
        let start = Instant::now();
        assert!(
            matches!(run_helper(helper, &["clap"], &clap("hang.clap"), config.timeout), Helper::Failed(r) if r.contains("timed out"))
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(
            matches!(run_helper(helper, &["clap"], &clap("crash.clap"), config.timeout), Helper::Failed(r) if r.contains("crashed"))
        );
        assert!(
            matches!(run_helper(helper, &["clap"], &clap("ok.clap"), config.timeout), Helper::Answered(j) if j.contains("test.ok"))
        );
        assert!(matches!(
            run_helper(Path::new("/bin/false"), &["clap"], &clap("ok.clap"), config.timeout),
            Helper::Unsupported
        ));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn scans_in_the_background_and_caches_failures() {
        let (dir, config) = fixture("catalog");
        for name in ["ok1.clap", "hang.clap", "crash.clap", "ok2.clap"] {
            std::fs::write(dir.join("clap").join(name), name).unwrap();
        }
        let catalog = Catalog::new(config.clone());
        let start = Instant::now();
        let first = catalog.snapshot();
        assert!(start.elapsed() < Duration::from_millis(200), "the snapshot waited for the scan");
        assert!(first.scanning);
        assert!(catalog.wait(Duration::from_secs(20)));
        let done = catalog.snapshot();
        assert!(!done.scanning);
        assert_eq!(done.progress.done, 4);
        let mut ids: Vec<&str> = done.clap.iter().map(|d| d.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["test.ok1", "test.ok2"]);
        let mut failed: Vec<(String, &str)> = done
            .failures
            .iter()
            .map(|f| (f.path.file_name().unwrap().to_string_lossy().into_owned(), f.reason.as_str()))
            .collect();
        failed.sort();
        assert!(failed[0].0 == "crash.clap" && failed[0].1.contains("crashed"), "{failed:?}");
        assert!(failed[1].0 == "hang.clap" && failed[1].1.contains("timed out"), "{failed:?}");
        assert_eq!(calls(&dir), 4);

        // The next run shows the cached plugins at once and loads nothing unchanged, failures
        // included; a full rescan tries everything again.
        let again = Catalog::new(config.clone());
        assert_eq!(again.snapshot().clap.len(), 2);
        assert!(again.wait(Duration::from_secs(20)));
        assert_eq!(calls(&dir), 4);
        assert_eq!(again.snapshot().failures.len(), 2);
        std::fs::write(dir.join("clap").join("ok3.clap"), "new").unwrap();
        again.rescan(false);
        assert!(again.wait(Duration::from_secs(20)));
        assert_eq!(calls(&dir), 5);
        assert_eq!(again.snapshot().clap.len(), 3);
        again.rescan(true);
        assert!(again.wait(Duration::from_secs(20)));
        assert_eq!(calls(&dir), 10);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn scans_in_process_without_a_helper() {
        let (dir, mut config) = fixture("local");
        std::fs::write(dir.join("clap").join("broken.clap"), "not a library").unwrap();
        config.helper = Some(PathBuf::from("/bin/false"));
        let catalog = Catalog::new(config);
        catalog.snapshot();
        assert!(catalog.wait(Duration::from_secs(20)));
        let done = catalog.snapshot();
        assert!(done.clap.is_empty());
        assert_eq!(done.failures.len(), 1);
        assert_eq!(calls(&dir), 0);
        std::fs::remove_dir_all(dir).ok();
    }
}
