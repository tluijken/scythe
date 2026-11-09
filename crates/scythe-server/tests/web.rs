mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{get_raw, http};
use scythe_core::render_settings::ResampleMode;
use scythe_engine::{Engine, MediaPool};
use scythe_server::{ControlServer, ServerConfig};
use scythe_session::Session;

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(':').map(str::trim))
}

#[test]
fn the_web_ui_is_served_without_a_token() {
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    let mut session = Session::new(Engine::start_offline(pool, 48_000));
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), Some("s3cret".into()));
    let (server, mut pump) = ControlServer::start(config, Arc::new(|| {})).expect("server");
    let addr = server.addr();

    let client = std::thread::spawn(move || {
        for (path, content_type, needle) in [
            ("/", "text/html", "<div id=\"app\">"),
            ("/monitor", "text/html", "/js/main.js"),
            ("/setlist/", "text/html", "/js/main.js"),
            ("/app.css", "text/css", "--accent"),
            ("/js/main.js", "text/javascript", "import"),
            ("/vendor/preact-htm.js", "text/javascript", "useState"),
            ("/manifest.webmanifest", "application/manifest+json", "Scythe"),
            ("/icon.svg", "image/svg+xml", "<svg"),
            ("/test/", "text/html", "harness.js"),
            ("/fonts/Inter-Regular.ttf", "font/ttf", ""),
        ] {
            let (status, head, body) = get_raw(addr, path, "");
            assert_eq!(status, 200, "{path}");
            assert!(header(&head, "content-type").is_some_and(|t| t.starts_with(content_type)), "{path}: {head}");
            assert!(String::from_utf8_lossy(&body).contains(needle), "{path}");
            assert!(!body.is_empty(), "{path}");
        }

        let (_, head, _) = get_raw(addr, "/app.css", "");
        let etag = header(&head, "etag").expect("an etag").to_owned();
        let (status, _, body) = get_raw(addr, "/app.css", &format!("If-None-Match: {etag}\r\n"));
        assert_eq!((status, body.len()), (304, 0));

        assert_eq!(get_raw(addr, "/nope.js", "").0, 404);
        assert_eq!(get_raw(addr, "/api/snapshot", "").0, 401);
        let (status, health) = http(addr, "GET", "/api/health", None, None);
        assert_eq!((status, health["token"].as_bool()), (200, Some(true)));
    });

    while !client.is_finished() {
        pump.wait(&mut session, Duration::from_millis(10));
    }
    client.join().unwrap();
    drop(server);
}
