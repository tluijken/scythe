//! `scythe-server --null-audio` as a process, driven over HTTP and the WebSocket.

use std::time::Duration;

use scythe_itest::{ADMIN, Scythe, http, live_fixture, test_dir};
use serde_json::{Value, json};

/// A server with the live fixture built into a show: an extra vocal track with an input, two
/// monitor mixes, a setlist of the project's songs and links for the drummer and the stage.
struct Show {
    s: Scythe,
    drummer: String,
    stage: String,
}

fn show(name: &str) -> Show {
    let dir = test_dir(name);
    let project = live_fixture(&dir);
    let s = Scythe::server(&dir, Some(&project));
    let names: Vec<String> =
        s.snapshot()["tracks"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect();
    assert_eq!(names, ["Drums", "Bass", "Keys"]);

    let vox = s.ok(json!({"cmd": "track.add", "name": "Vox"}));
    assert_eq!(vox["name"], "Vox");
    let vox = s.ok(json!({"cmd": "track.input", "track": "Vox", "input": 2}));
    assert_eq!(vox["input"], 2, "{vox}");
    s.ok(json!({"cmd": "track.arm", "track": "Vox", "armed": true}));
    s.ok(json!({"cmd": "track.monitor", "track": "Vox", "monitor": true}));

    let drums = s.ok(json!({"cmd": "mix.create", "musician": "Drummer", "own": ["Drums"]}));
    assert_eq!(drums["name"], "MON Drummer");
    let singer = s.ok(json!({"cmd": "mix.create", "musician": "Singer", "own": ["Vox"]}));
    assert_eq!(singer["name"], "MON Singer");
    let sources: Vec<&str> = drums["sources"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert!(sources.contains(&"Drums") && sources.contains(&"Vox"), "{sources:?}");
    let mix = s.ok(json!({"cmd": "mix.level", "mix": "MON Drummer", "source": "Bass", "db": -6.0}));
    let bass = mix["sources"].as_array().unwrap().iter().find(|x| x["name"] == "Bass").unwrap();
    assert!((bass["volume_db"].as_f64().unwrap() + 6.0).abs() < 0.01, "{bass}");
    let mix = s.ok(json!({"cmd": "mix.more_me", "mix": "MON Singer", "db": 6.0}));
    assert_eq!(mix["more_me"], 6.0, "{mix}");
    assert_eq!(s.ok(json!({"cmd": "mix.list"})).as_array().unwrap().len(), 2);

    let songs = s.ok(json!({"cmd": "setlist.songs"}));
    let songs = songs.as_array().unwrap();
    let names: Vec<&str> = songs.iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Opener", "Ballad"], "{songs:?}");
    assert!(songs.iter().all(|x| x["ends_at"] == "stop_marker"), "{songs:?}");
    assert!((songs[0]["end"].as_f64().unwrap() - 4.0).abs() < 1e-6, "{songs:?}");
    let list = s.ok(json!({"cmd": "setlist.create", "name": "Gig", "songs": ["Ballad", "Opener"]}));
    let entries: Vec<&str> = list["entries"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(entries, ["Ballad", "Opener"]);

    let drummer = s.ok(json!({"cmd": "access.create", "role": "musician", "musician": "Drummer"}));
    assert_eq!(drummer["role"], "musician");
    assert!(drummer["path"].as_str().unwrap().starts_with("/monitor?token="), "{drummer}");
    let stage = s.ok(json!({"cmd": "access.create", "role": "stage", "name": "Stage tablet"}));
    assert!(stage["path"].as_str().unwrap().starts_with("/setlist?token="), "{stage}");
    let links = s.ok(json!({"cmd": "access.list"}));
    assert_eq!(links["links"].as_array().unwrap().len(), 2);
    Show {
        drummer: drummer["token"].as_str().unwrap().to_owned(),
        stage: stage["token"].as_str().unwrap().to_owned(),
        s,
    }
}

#[test]
fn builds_a_live_session_and_keeps_it_across_save_and_reopen() {
    let Show { s, .. } = show("server_build");
    let path = s.dir.join("saved.RPP");
    s.ok(json!({"cmd": "project.save", "path": path}));
    s.ok(json!({"cmd": "project.new"}));
    assert_eq!(s.snapshot()["tracks"].as_array().unwrap().len(), 0);
    s.ok(json!({"cmd": "project.open", "path": path}));
    let snap = s.snapshot();
    let names: Vec<&str> = snap["tracks"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Drums", "Bass", "Keys", "Vox", "MON Drummer", "MON Singer"]);
    let vox = snap["tracks"].as_array().unwrap().iter().find(|t| t["name"] == "Vox").unwrap();
    assert_eq!(vox["input"], 2);
    assert_eq!(vox["armed"], true);
    assert_eq!(snap["mixes"].as_array().unwrap().len(), 2);
    let list = s.ok(json!({"cmd": "setlist.get", "setlist": "Gig"}));
    assert_eq!(list["entries"].as_array().unwrap().len(), 2, "{list}");
    let links = s.ok(json!({"cmd": "access.list"}));
    assert_eq!(links["links"].as_array().unwrap().len(), 2, "links live beside the project, not in it");
}

#[test]
fn a_setlist_song_stops_at_its_stop_marker() {
    let Show { s, .. } = show("server_setlist_stop");
    s.ok(json!({"cmd": "setlist.select", "setlist": "Gig"}));
    let status = s.ok(json!({"cmd": "setlist.cue", "entry": 1}));
    assert_eq!(status["current"]["name"], "Opener", "{status}");
    // Opener runs from 0 s to its stop marker at 4 s; like REAPER's stop, the playhead then
    // goes back to the edit cursor at the song's start.
    let (played, t) = s.play_until_stopped(1);
    assert!((3.6..=5.0).contains(&played), "Opener played {played:.2} s, its stop marker is 4 s in: {t}");
    assert!(t["position"].as_f64().unwrap().abs() < 0.01, "{t}");
    let status = s.ok(json!({"cmd": "setlist.status"}));
    assert_eq!(status["playing"], false, "{status}");

    let (played, t) = s.play_until_stopped(0);
    assert!((3.6..=5.0).contains(&played), "Ballad played {played:.2} s, from 6 s to its stop at 10 s: {t}");
    assert!((t["position"].as_f64().unwrap() - 6.0).abs() < 0.01, "{t}");
}

#[test]
fn a_websocket_client_reconnects_and_keeps_its_replies_in_order() {
    let Show { s, .. } = show("server_ws_reconnect");
    let mut ws = s.ws(ADMIN);
    assert_eq!(ws.hello["protocol"], 1);
    assert_eq!(ws.hello["access"]["role"], "admin");
    let commands: Vec<Value> =
        (0..10).map(|i| json!({"cmd": "track.volume", "track": "Keys", "db": -(i as f64)})).collect();
    let replies = ws.exchange(&commands);
    let ids: Vec<i64> = replies.iter().map(|r| r["id"].as_i64().unwrap()).collect();
    assert_eq!(ids, (1..=10).collect::<Vec<_>>());
    assert!(replies.iter().all(|r| r["ok"] == true), "{replies:?}");
    ws.listen(Duration::from_millis(500));
    assert!(ws.saw("project_changed"), "no project_changed event: {:?}", ws.events);
    ws.close();

    let clients = http(s.addr, "GET", "/api/health", None, None).1["clients"].as_i64();
    let mut ws = s.ws(ADMIN);
    assert_eq!(ws.hello["access"]["role"], "admin");
    let replies = ws.exchange(&[
        json!({"cmd": "transport.play"}),
        json!({"cmd": "track.get", "track": "Keys"}),
        json!({"cmd": "track.get", "track": "Nobody"}),
        json!({"cmd": "transport.stop"}),
    ]);
    assert_eq!(replies.iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), [1, 2, 3, 4]);
    assert!((replies[1]["result"]["volume_db"].as_f64().unwrap() + 9.0).abs() < 0.01, "{}", replies[1]);
    assert_eq!(replies[2]["ok"], false);
    assert_eq!(replies[2]["error"]["kind"], "not_found");
    ws.events.clear();
    ws.send(&json!({"id": 99, "cmd": "transport.play"}));
    ws.listen(Duration::from_millis(800));
    assert!(ws.saw("transport"), "no transport events after reconnecting: {:?}", ws.events.len());
    assert!(ws.saw("meters"), "no meters after reconnecting");
    s.ok(json!({"cmd": "transport.stop"}));
    let health = http(s.addr, "GET", "/api/health", None, None).1;
    assert!(health["clients"].as_i64() <= clients.map(|c| c + 1), "the closed socket still counts: {health}");
}

#[test]
fn a_musician_link_is_refused_admin_commands() {
    let Show { s, drummer, stage } = show("server_roles");
    let mut ws = s.ws(&drummer);
    assert_eq!(ws.hello["access"]["role"], "musician");
    assert_eq!(ws.hello["access"]["musician"], "Drummer");
    let replies = ws.exchange(&[
        json!({"cmd": "mix.level", "mix": "MON Drummer", "source": "Keys", "db": -3.0}),
        json!({"cmd": "track.add", "name": "Intruder"}),
    ]);
    assert_eq!(replies[0]["ok"], true, "{}", replies[0]);
    assert_eq!(replies[1]["error"]["kind"], "forbidden", "{}", replies[1]);
    ws.close();

    let (status, mixes) = s.send(&drummer, json!({"cmd": "mix.list"}));
    assert_eq!(status, 200);
    assert_eq!(mixes["result"].as_array().unwrap().len(), 1, "a musician sees only their mix: {mixes}");
    for refused in [
        json!({"cmd": "track.add"}),
        json!({"cmd": "project.save", "path": s.dir.join("x.RPP")}),
        json!({"cmd": "mix.level", "mix": "MON Singer", "source": "Keys", "db": 3.0}),
        json!({"cmd": "mix.more_me", "mix": "MON Singer", "db": 3.0}),
        json!({"cmd": "access.list"}),
        json!({"cmd": "access.create", "role": "stage"}),
        json!({"cmd": "transport.play"}),
        json!({"cmd": "track.mute", "track": "Vox"}),
    ] {
        let (status, body) = s.send(&drummer, refused.clone());
        assert_eq!(status, 403, "{refused} answered {body}");
        assert_eq!(body["error"]["kind"], "forbidden");
    }
    let (status, _) =
        http(s.addr, "POST", "/mcp", Some(&drummer), Some(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})));
    assert_eq!(status, 403, "MCP is admin only");
    assert!(!s.dir.join("x.RPP").exists());

    let (status, body) = s.send(&stage, json!({"cmd": "transport.play"}));
    assert_eq!(status, 200, "{body}");
    let (status, _) = s.send(&stage, json!({"cmd": "transport.stop"}));
    assert_eq!(status, 200);
    for refused in [json!({"cmd": "mix.list"}), json!({"cmd": "track.add"}), json!({"cmd": "setlist.delete"})] {
        assert_eq!(s.send(&stage, refused.clone()).0, 403, "{refused}");
    }
    let (status, _) = s.send("not-a-token", json!({"cmd": "transport.state"}));
    assert_eq!(status, 401);
    let names: Vec<String> =
        s.snapshot()["tracks"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect();
    assert!(!names.iter().any(|n| n == "Intruder"));
}
