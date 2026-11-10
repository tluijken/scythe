//! The NAM panel's TONE3000 browser against [`MockTone3000`].

use crate::nam::preview::Stage;
use crate::nam::tone3000::{Config, TokenStore, Tokens};
use crate::nam::{NamHub, Target};

use super::tone3000_mock::{MockTone3000, TONE_AMP};
use super::*;

/// The live fixture with a NAM slot on Bass, its panel open on the TONE3000 tab, TONE3000
/// being `mock`.
fn nam_app(name: &str, mock: &MockTone3000, key: bool, signed_in: bool) -> TestApp {
    nam_app_with(name, mock, key, signed_in, "Neural Amp Modeler")
}

/// [`nam_app`] with `plugin` (the NAM slot, or a guitar rig) from the FX browser.
fn nam_app_with(name: &str, mock: &MockTone3000, key: bool, signed_in: bool, plugin: &str) -> TestApp {
    let mut t = TestApp::live(name);
    let dir = t.dir.clone();
    let config = Config {
        client_id: key.then(|| "t3k_pub_itest".to_owned()),
        redirect_port: 0,
        base_url: Some(mock.base.clone()),
    };
    let tokens = TokenStore::new(dir.join("tone3000-session.json"));
    if signed_in {
        let session =
            Tokens { access_token: "mock-access".into(), refresh_token: Some("mock-refresh".into()), expires_at: None };
        tokens.save(&session).unwrap();
    }
    t.app().nam = NamHub::with(config, dir.join("nam"), tokens, dir.join("nam-preview"));
    t.click("Track Bass");
    t.click("Add plugin");
    t.click(plugin);
    assert!(t.app().nam.panel.is_some(), "a new {plugin} opens its panel");
    t.click("TONE3000");
    t
}

fn target(t: &mut TestApp) -> Target {
    t.app().nam.panel.clone().expect("the NAM panel is open")
}

fn wait_for(t: &mut TestApp, label: &str) {
    let label = label.to_owned();
    t.until(&label.clone(), 10.0, move |t| t.has(&label));
}

#[test]
fn without_a_key_the_tab_asks_for_one_and_saves_it() {
    let mock = MockTone3000::start();
    let mut t = nam_app("nam_key", &mock, false, false);
    assert!(!t.app().nam.has_client_id());
    t.h.get_by(|n| n.data().placeholder() == Some("t3k_pub_…")).focus();
    t.frames(2);
    t.type_text("t3k_pub_typed");
    t.click_role(Role::Button, "Save");
    assert!(t.app().nam.has_client_id());
    assert_eq!(t.app().nam.config.client_id.as_deref(), Some("t3k_pub_typed"));
    assert!(t.has("Sign in with TONE3000"), "{:#?}", t.labels());
    assert!(!t.app().nam.client.signed_in(), "saving the key keeps the session file it had: none");
}

#[test]
fn signed_out_offers_to_sign_in_without_asking_the_api() {
    let mock = MockTone3000::start();
    let t = nam_app("nam_signed_out", &mock, true, false);
    assert!(t.has("Sign in with TONE3000") && t.has("Browse TONE3000…"));
    assert!(mock.requests().is_empty(), "{:?}", mock.requests());
}

#[test]
fn signed_in_lists_tones_and_a_tone_shows_its_models() {
    let mock = MockTone3000::start();
    let mut t = nam_app("nam_list", &mock, true, true);
    wait_for(&mut t, "Tone Friedman BE-100 Deluxe");
    assert!(t.has("Tone Mesa OS 4x12 SM57") && t.has("Tone Tone Bender MkII into a Plexi"), "{:#?}", t.labels());
    t.until("the user", 10.0, |t| t.app().nam.user.is_some());
    assert_eq!(t.app().nam.user.as_ref().unwrap().shown_name(), "stagehand");
    assert!(t.app().nam.remote.error.is_none(), "{:?}", t.app().nam.remote.error);
    t.click("Tone Friedman BE-100 Deluxe");
    wait_for(&mut t, "Model BE ch, gain 2 o'clock");
    assert!(t.has("Model BE ch, gain 2 o'clock (lite)") && t.has("Model model 90003"), "{:#?}", t.labels());
    assert!(t.app().nam.remote.models_error.is_none(), "{:?}", t.app().nam.remote.models_error);
    assert!(t.has("more"), "the long description starts folded");
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, SIZE);
    for model in ["Model BE ch, gain 2 o'clock", "Model model 90003"] {
        let r = t.rect(model);
        assert!(r.height() >= 20.0 && screen.contains_rect(r), "{model} is in view: {r:?}");
    }
    assert!(mock.asked(&format!("GET /api/v1/models?tone_id={TONE_AMP}")), "{:?}", mock.requests());
}

#[test]
fn preview_then_revert_and_preview_then_keep() {
    let mock = MockTone3000::start();
    let mut t = nam_app("nam_preview", &mock, true, true);
    let target = target(&mut t);
    wait_for(&mut t, "Tone Friedman BE-100 Deluxe");
    t.click("Tone Friedman BE-100 Deluxe");
    wait_for(&mut t, "Model BE ch, gain 2 o'clock");
    t.click_first("Preview");
    t.until("the preview to play", 10.0, |t| {
        t.app().nam.preview.as_ref().is_some_and(|p| matches!(p.stage, Stage::Playing | Stage::Failed(_)))
    });
    let stage = t.app().nam.preview.as_ref().unwrap().stage.clone();
    assert!(matches!(stage, Stage::Playing), "{stage:?}");
    assert!(t.app().nam_model_path(&target).is_some(), "the slot plays the previewed model");
    t.click_role(Role::Button, "Revert");
    assert!(t.app().nam.preview.is_none());
    t.until("the slot to empty again", 10.0, |t| t.app().nam_model_path(&target).is_none());

    t.click_first("Preview");
    t.until("the preview to play", 10.0, |t| {
        t.app().nam.preview.as_ref().is_some_and(|p| matches!(p.stage, Stage::Playing))
    });
    t.click_role(Role::Button, "Keep");
    assert!(t.app().nam.preview.is_none());
    let kept = t.app().nam.library.downloaded(90001).map(|e| e.path.clone()).expect("kept in the library");
    t.until("the slot to load the kept model", 10.0, |t| t.app().nam_model_path(&target).as_ref() == Some(&kept));
    assert!(kept.starts_with(&t.dir), "the library is the test's own: {}", kept.display());
}

#[test]
fn an_html_error_page_reads_as_a_short_error() {
    let mock = MockTone3000::start();
    let mut t = nam_app("nam_html_error", &mock, true, true);
    wait_for(&mut t, "Tone Friedman BE-100 Deluxe");
    for list in ["Downloaded", "My tones"] {
        t.click(list);
        t.until("the error", 10.0, |t| t.app().nam.remote.error.is_some());
        let error = t.app().nam.remote.error.clone().unwrap();
        assert!(!error.contains('<') && error.len() < 160, "{list}: {error:?}");
        t.frames(2);
    }
    t.click("Trending");
    t.until("the list", 10.0, |t| !t.app().nam.remote.loading);
    assert!(t.app().nam.remote.error.is_none(), "nulls for an empty page: {:?}", t.app().nam.remote.error);
    assert!(t.has("No tones here."), "{:#?}", t.labels());
}

#[test]
fn a_rigs_amp_section_lists_a_tones_models_in_view() {
    let mock = MockTone3000::start();
    let mut t = nam_app_with("nam_rig", &mock, true, true, "Guitar rig");
    wait_for(&mut t, "Tone Friedman BE-100 Deluxe");
    t.click("Tone Friedman BE-100 Deluxe");
    wait_for(&mut t, "Model BE ch, gain 2 o'clock");
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, SIZE);
    let r = t.rect("Model BE ch, gain 2 o'clock");
    assert!(r.height() >= 20.0 && screen.contains_rect(r), "the rig's model list is in view: {r:?}");
}
