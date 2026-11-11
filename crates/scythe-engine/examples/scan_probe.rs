//! Times scanning the plugins in the given folders, in this process and without the cache:
//! `scan_probe lv2|clap|vst3 DIR...`.

use std::path::PathBuf;
use std::time::Instant;

use scythe_engine::{clap_host, lv2, vst3_host};

fn main() {
    let mut args = std::env::args().skip(1);
    let kind = args.next().unwrap_or_default();
    let dirs: Vec<PathBuf> = args.map(PathBuf::from).collect();
    let start = Instant::now();
    let found = match kind.as_str() {
        "lv2" => lv2::scan(&dirs).len(),
        "clap" => {
            clap_host::bundles(&dirs).iter().filter_map(|b| clap_host::scan_bundle(b).ok()).map(|d| d.len()).sum()
        }
        "vst3" => vst3_host::scan(&dirs).len(),
        _ => {
            eprintln!("usage: scan_probe lv2|clap|vst3 DIR...");
            std::process::exit(2);
        }
    };
    println!("{kind}: {found} plugins in {:.2?}", start.elapsed());
}
