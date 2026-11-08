//! Prints how every effect slot of the given projects is played (native, hosted or why not).
use scythe_engine::fx::FxRegistry;

fn main() {
    for path in std::env::args().skip(1) {
        let project = scythe_core::rpp::read_project(path.as_ref()).unwrap();
        let mut fx = FxRegistry::new(48000);
        fx.project_dir = project.path.as_deref().and_then(std::path::Path::parent).map(Into::into);
        let chains = std::iter::once((None, "MASTER", &project.master.fx))
            .chain(project.tracks.iter().map(|t| (Some(t.id), t.name.as_str(), &t.fx)));
        for (owner, name, slots) in chains {
            fx.chain(owner, slots);
            for (i, slot) in slots.iter().enumerate() {
                println!("{path}: {name} #{i} {}: {:?}", slot.name, fx.status(owner, slot, i));
            }
        }
    }
}
