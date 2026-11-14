//! Every ScyEQ-Pro this library runs in the host's process, for each to find the others, as
//! instances of one plugin find each other in a host. A CLAP and a VST3 build are libraries of
//! their own, so each sees only its own instances.

use std::sync::{Arc, Mutex, Weak};

use crate::plugin::ScyEqShared;

static INSTANCES: Mutex<Vec<(u64, Weak<ScyEqShared>)>> = Mutex::new(Vec::new());

/// An instance's place in the registry; dropping it takes the instance out.
pub struct Registration(u64);

impl Drop for Registration {
    fn drop(&mut self) {
        instances().retain(|(id, _)| *id != self.0);
    }
}

pub fn register(shared: &Arc<ScyEqShared>) -> Registration {
    instances().push((shared.id(), Arc::downgrade(shared)));
    Registration(shared.id())
}

/// Every registered instance but `me`, in the order they registered.
pub fn others(me: u64) -> Vec<Arc<ScyEqShared>> {
    instances().iter().filter(|(id, _)| *id != me).filter_map(|(_, shared)| shared.upgrade()).collect()
}

fn instances() -> std::sync::MutexGuard<'static, Vec<(u64, Weak<ScyEqShared>)>> {
    INSTANCES.lock().unwrap_or_else(|e| e.into_inner())
}
