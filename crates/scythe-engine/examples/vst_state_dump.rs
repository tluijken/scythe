//! Dumps the decoded plugin state of every VST slot whose name contains a filter string, as
//! the leading C string (if any) followed by f64 values.
use scythe_core::rpp::{Node, read_project};
use scythe_engine::vst2::ReaperVstState;

fn main() {
    let mut args = std::env::args().skip(1);
    let filter = args.next().unwrap();
    for path in args {
        let p = read_project(path.as_ref()).unwrap();
        for t in &p.tracks {
            for fx in t.fx.iter().filter(|f| f.name.contains(&filter)) {
                let Some(Node::Chunk(c)) = fx.raw.iter().find(|n| matches!(n, Node::Chunk(_))) else { continue };
                let lines: Vec<String> = c
                    .children
                    .iter()
                    .filter_map(|n| match n {
                        Node::Line(l) => Some(l.clone()),
                        _ => None,
                    })
                    .collect();
                let Some(s) = ReaperVstState::decode(&lines) else { continue };
                let nul = s.chunk.iter().position(|&b| b == 0).unwrap_or(0);
                println!("{} | {} bytes | {}", fx.name, s.chunk.len(), String::from_utf8_lossy(&s.chunk[..nul]));
                let rest = &s.chunk[nul + 1..];
                let vals: Vec<String> =
                    rest.as_chunks::<8>().0.iter().map(|b| format!("{:.6}", f64::from_le_bytes(*b))).collect();
                println!("  {}", vals.join(" "));
            }
        }
    }
}
