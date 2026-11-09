//! The web UI: plain ES modules and styles under `web/`, embedded in the binary so a stage
//! without internet (or without the source tree) still serves them.

use std::sync::OnceLock;

use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

macro_rules! web {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/web/", $path)) as &[u8])),*]
    };
}

macro_rules! font {
    ($($file:literal),* $(,)?) => {
        &[$((
            concat!("fonts/", $file),
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../scythe-app/assets/fonts/", $file)) as &[u8],
        )),*]
    };
}

/// Every file under `web/` but its `package.json` (which only tells Node the files are ES
/// modules, for the tests); a test keeps this list and the directory in step.
pub const FILES: &[(&str, &[u8])] = web![
    "index.html",
    "app.css",
    "icon.svg",
    "manifest.webmanifest",
    "js/main.js",
    "js/api.js",
    "js/domains.js",
    "js/format.js",
    "js/store.js",
    "js/ui.js",
    "js/login.js",
    "js/editor.js",
    "js/arrange.js",
    "js/mixer.js",
    "js/inspector.js",
    "js/monitor.js",
    "js/setlist.js",
    "vendor/preact-htm.js",
    "vendor/LICENSE-htm.txt",
    "vendor/LICENSE-preact.txt",
    "test/index.html",
    "test/run.mjs",
    "test/harness.js",
    "test/format.test.js",
    "test/api.test.js",
    "test/domains.test.js",
];

/// The desktop app's fonts, so the web UI looks the same.
pub const FONTS: &[(&str, &[u8])] = font![
    "Inter-Regular.ttf",
    "Inter-SemiBold.ttf",
    "JetBrainsMono-Medium.ttf",
    "Inter-OFL.txt",
    "JetBrainsMono-OFL.txt",
];

/// Paths the single-page app answers itself.
const PAGES: &[&str] = &["", "monitor", "setlist"];

pub fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("webmanifest") => "application/manifest+json",
        Some("json") => "application/json",
        Some("ttf") => "font/ttf",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn find(path: &str) -> Option<(usize, &'static [u8])> {
    FILES.iter().chain(FONTS).enumerate().find(|(_, (p, _))| *p == path).map(|(i, (_, bytes))| (i, *bytes))
}

/// FNV-1a of each file, computed once: phones on stage wifi revalidate instead of downloading
/// again, and a new binary changes the tags.
fn etag(index: usize) -> &'static str {
    static TAGS: OnceLock<Vec<String>> = OnceLock::new();
    let tags = TAGS.get_or_init(|| {
        FILES
            .iter()
            .chain(FONTS)
            .map(|(_, bytes)| {
                let hash =
                    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3));
                format!("\"{hash:016x}\"")
            })
            .collect()
    });
    &tags[index]
}

/// `SCYTHE_WEB_DIR` serves the web UI from that directory instead, so working on it needs a
/// reload rather than a rebuild.
fn dev_dir() -> Option<&'static std::path::Path> {
    static DIR: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| std::env::var_os("SCYTHE_WEB_DIR").map(Into::into)).as_deref()
}

pub async fn serve(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/').trim_end_matches('/');
    let index_of_dir = format!("{path}/index.html");
    let path = if PAGES.contains(&path) {
        "index.html"
    } else if find(path).is_none() && find(&index_of_dir).is_some() {
        &index_of_dir
    } else {
        path
    };
    let Some((index, bytes)) = find(path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if let Some(dir) = dev_dir()
        && index < FILES.len()
        && let Ok(bytes) = std::fs::read(dir.join(path))
    {
        return ([(header::CACHE_CONTROL, "no-store"), (header::CONTENT_TYPE, content_type(path))], bytes)
            .into_response();
    }
    let tag = etag(index);
    let cache =
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-cache")), (header::ETAG, HeaderValue::from_static(tag))];
    if headers.get(header::IF_NONE_MATCH).is_some_and(|v| v.as_bytes() == tag.as_bytes()) {
        return (StatusCode::NOT_MODIFIED, cache).into_response();
    }
    (cache, [(header::CONTENT_TYPE, content_type(path))], bytes).into_response()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, base, out);
            } else if path.file_name().is_some_and(|n| n != "package.json") {
                out.push(path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }

    #[test]
    fn every_file_under_web_is_embedded() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
        let mut on_disk = Vec::new();
        walk(&base, &base, &mut on_disk);
        on_disk.sort();
        let mut embedded: Vec<String> = FILES.iter().map(|(p, _)| p.to_string()).collect();
        embedded.sort();
        assert_eq!(on_disk, embedded);
    }

    /// The web UI's logic tests, when Node is installed (it is in the devbox shell); they also
    /// run in any browser at `/test/`.
    #[test]
    fn javascript_tests_pass() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
        let output = match std::process::Command::new("node").arg("test/run.mjs").current_dir(&dir).output() {
            Ok(output) => output,
            Err(e) => return eprintln!("skipping the JavaScript tests: node did not run ({e})"),
        };
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{text}{}", String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn every_file_has_a_known_type() {
        for (path, _) in FILES.iter().chain(FONTS) {
            assert_ne!(content_type(path), "application/octet-stream", "{path}");
        }
    }
}
