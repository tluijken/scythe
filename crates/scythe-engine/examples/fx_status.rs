//! Prints how every effect slot of the given projects is played (native, hosted or why not).
use scythe_engine::fx::FxRegistry;

fn main() {
    for path in std::env::args().skip(1) {
        let project = scythe_core::rpp::read_project(path.as_ref()).unwrap();
        let mut fx = FxRegistry::new(48000);
        fx.project_dir = project.path.as_deref().and_then(std::path::Path::parent).map(Into::into);
        for t in &project.tracks {
            fx.chain(Some(t.id), &t.fx);
            for (i, slot) in t.fx.iter().enumerate() {
                println!("{path}: {} #{i} {}: {:?}", t.name, slot.name, fx.status(Some(t.id), slot, i));
            }
        }
    }
}
