//! Lists MIDI items with their note counts, source length and item placement.
use scythe_core::Source;
use scythe_core::midi::MidiSource;
use scythe_core::rpp::read_project;

fn main() {
    for path in std::env::args().skip(1) {
        let p = read_project(path.as_ref()).unwrap();
        let (mut empty, mut with) = (0, 0);
        for t in &p.tracks {
            for it in &t.items {
                let Some(Source::Raw(c)) = it.take().map(|t| &t.source) else { continue };
                let Some(m) = MidiSource::parse(c) else { continue };
                let n = m.notes().len();
                if n == 0 {
                    empty += 1
                } else {
                    with += 1
                }
                if empty + with <= 6 {
                    println!(
                        "pos {:.3} len {:.3} loop {} soffs {:.3} rate {} | {} notes, source {:.2} qn, chunk children {}",
                        it.position,
                        it.length,
                        it.looped,
                        it.take().unwrap().source_offset,
                        it.take().unwrap().playrate,
                        n,
                        m.length_qn(),
                        c.children.len()
                    );
                }
            }
        }
        println!("{path}: {with} MIDI items with notes, {empty} without");
    }
}
