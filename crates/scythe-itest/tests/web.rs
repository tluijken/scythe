//! The web UI's pages in headless Chromium against `scythe-server`: each page loads, one action
//! on it reaches the session, and no page throws.

use std::time::Duration;

use scythe_itest::cdp::Browser;
use scythe_itest::{ADMIN, Scythe, live_fixture, test_dir};
use serde_json::json;

#[test]
fn editor_monitor_and_setlist_pages_drive_the_session() {
    let dir = test_dir("web_pages");
    let project = live_fixture(&dir);
    let s = Scythe::server(&dir, Some(&project));
    let Some(mut b) = Browser::start(&dir, "editor_monitor_and_setlist_pages_drive_the_session") else { return };
    s.ok(json!({"cmd": "mix.create", "musician": "Drummer", "own": ["Drums"]}));
    s.ok(json!({"cmd": "setlist.create", "name": "Gig", "songs": ["Opener", "Ballad"]}));
    let drummer = s.ok(json!({"cmd": "access.create", "role": "musician", "musician": "Drummer"}));
    let stage = s.ok(json!({"cmd": "access.create", "role": "stage", "name": "Stage"}));
    let base = format!("http://{}", s.addr);

    b.navigate(&format!("{base}/?token={ADMIN}"));
    b.wait_for(
        "the editor's track list",
        "document.body.innerText.includes('Keys') && !!document.querySelector('button.btn.small')",
    );
    let tracks = || s.snapshot()["tracks"].as_array().unwrap().len();
    let before = tracks();
    let clicked = b.eval(
        "(() => { const b = [...document.querySelectorAll('button.btn.small')].find(b => b.textContent.trim() === 'Track'); \
         if (!b) return false; b.click(); return true; })()",
    );
    assert_eq!(clicked, true, "no Track button on the editor page");
    s.wait_for("the track the page added", Duration::from_secs(5), json!({"cmd": "track.list"}), |l| {
        l.as_array().unwrap().len() == before + 1
    });
    b.wait_for(
        "the new track shown",
        &format!("document.querySelectorAll('button.flag.mute').length >= {}", before + 1),
    );

    b.navigate(&format!("{base}{}", drummer["path"].as_str().unwrap()));
    b.wait_for(
        "the drummer's mix",
        "document.body.innerText.includes('Drummer') && !!document.querySelector('button[title=\"More me\"]')",
    );
    b.eval("document.querySelector('button[title=\"More me\"]').click()");
    s.wait_for("more me from the page", Duration::from_secs(5), json!({"cmd": "mix.get", "mix": "MON Drummer"}), |m| {
        m["more_me"].as_f64().unwrap() > 0.5
    });

    b.navigate(&format!("{base}{}", stage["path"].as_str().unwrap()));
    b.wait_for(
        "the setlist",
        "document.body.innerText.includes('Opener') && !!document.querySelector('button[aria-label=\"Next song\"]')",
    );
    let before = s.ok(json!({"cmd": "setlist.status"}));
    assert_eq!(before["current"]["name"], "Opener", "{before}");
    b.eval("document.querySelector('button[aria-label=\"Next song\"]').click()");
    s.wait_for("the next song cued from the page", Duration::from_secs(5), json!({"cmd": "setlist.status"}), |st| {
        st["current"]["name"] == "Ballad"
    });
    b.eval("document.querySelector('button[aria-label=\"Play\"]').click()");
    s.wait_for("play from the page", Duration::from_secs(5), json!({"cmd": "transport.state"}), |t| {
        t["playing"] == true
    });
    s.ok(json!({"cmd": "transport.stop"}));
    std::thread::sleep(Duration::from_millis(300));
    b.eval("1");
    assert!(b.errors.is_empty(), "the pages logged errors: {:#?}", b.errors);
}
