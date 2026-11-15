//! Every ScyEQ-Pro this library runs in the host's process, for each to find the others, as
//! instances of one plugin find each other in a host. A CLAP and a VST3 build are libraries of
//! their own, so each sees only its own instances.

use std::sync::{Arc, Mutex, Weak};

use plugin_format::egui;
use scyeq_ui::Instance;

use crate::plugin::ScyEqShared;

/// Each instance's id and number, and the instance.
static INSTANCES: Mutex<Vec<(u64, u32, Weak<ScyEqShared>)>> = Mutex::new(Vec::new());

/// An instance's place in the registry; dropping it takes the instance out.
pub struct Registration(u64);

impl Drop for Registration {
    fn drop(&mut self) {
        instances().retain(|(id, _, _)| *id != self.0);
    }
}

/// Registers `shared` under the lowest number no other instance has.
pub fn register(shared: &Arc<ScyEqShared>) -> Registration {
    let mut all = instances();
    let number = (1..).find(|n| all.iter().all(|(_, taken, _)| taken != n)).unwrap_or(0);
    all.push((shared.id(), number, Arc::downgrade(shared)));
    Registration(shared.id())
}

/// Every registered instance but `me`, in the order they registered.
pub fn others(me: u64) -> Vec<Arc<ScyEqShared>> {
    instances().iter().filter(|(id, _, _)| *id != me).filter_map(|(_, _, shared)| shared.upgrade()).collect()
}

/// [`others`] as the editor lists them.
pub fn listed_others(me: u64) -> Vec<Instance> {
    others(me).iter().map(|s| listed(s)).collect()
}

/// An instance as the editor lists it: by its track's name and colour, or by its number when the
/// host names no track.
pub fn listed(shared: &ScyEqShared) -> Instance {
    let track = shared.track();
    Instance {
        id: id_of(shared),
        label: track.name.unwrap_or_else(|| format!("ScyEQ-Pro {}", number(shared.id()))),
        color: track.color,
    }
}

/// The instance's number: the lowest one free when it registered, kept for its life.
fn number(id: u64) -> u32 {
    instances().iter().find(|(of, _, _)| *of == id).map_or(0, |(_, number, _)| *number)
}

/// Stays the same for the instance's life, as the editor needs.
pub(crate) fn id_of(shared: &ScyEqShared) -> egui::Id {
    egui::Id::unique(("scyeq-plugin-instance", shared.id()))
}

fn instances() -> std::sync::MutexGuard<'static, Vec<(u64, u32, Weak<ScyEqShared>)>> {
    INSTANCES.lock().unwrap_or_else(|e| e.into_inner())
}
