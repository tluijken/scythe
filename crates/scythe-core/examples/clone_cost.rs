//! Times cloning and comparing projects (what every undoable edit costs) and estimates their
//! size through the length of their RPP text.
use std::time::Instant;

use scythe_core::rpp::{project_to_rpp, read_project};

fn main() {
    for path in std::env::args().skip(1) {
        let p = read_project(path.as_ref()).unwrap();
        let t = Instant::now();
        let copies: Vec<_> = (0..20).map(|_| p.clone()).collect();
        let clone_ms = t.elapsed().as_secs_f64() * 1000.0 / 20.0;
        let t = Instant::now();
        let equal = copies.iter().all(|c| *c == p);
        let eq_ms = t.elapsed().as_secs_f64() * 1000.0 / 20.0;
        let bytes = project_to_rpp(&p).len();
        println!(
            "{clone_ms:>6.2} ms clone, {eq_ms:>6.2} ms compare ({equal}), ~{:.1} MB text: {path}",
            bytes as f64 / 1e6
        );
    }
}
