//! Loads every effect of each project, stores their states back as a save would and reports
//! slots whose stored data changed although nothing was edited.
use scythe_core::rpp::read_project;
use scythe_engine::fx::FxRegistry;

fn main() {
    for path in std::env::args().skip(1) {
        let mut project = read_project(path.as_ref()).unwrap();
        let before = project.clone();
        let mut fx = FxRegistry::new(48000);
        fx.project_dir = project.path.as_deref().and_then(std::path::Path::parent).map(Into::into);
        for t in &project.tracks {
            fx.chain(Some(t.id), &t.fx);
        }
        fx.chain(None, &project.master.fx);
        fx.store_states(&mut project);
        let changed: Vec<String> = project
            .tracks
            .iter()
            .zip(&before.tracks)
            .flat_map(|(a, b)| a.fx.iter().zip(&b.fx).filter(|(x, y)| x != y).map(|(x, _)| x.name.clone()))
            .collect();
        println!(
            "{} changed slot(s){}: {path}",
            changed.len(),
            if changed.is_empty() { String::new() } else { format!(" {changed:?}") }
        );
    }
}
