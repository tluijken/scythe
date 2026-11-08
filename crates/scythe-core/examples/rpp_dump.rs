//! Prints the in-memory round trip of one `.RPP` file to stdout. Never writes to disk.
fn main() {
    let path = std::env::args().nth(1).expect("usage: rpp_dump <file>");
    let text = std::fs::read_to_string(path).expect("read");
    print!("{}", scythe_core::rpp::project_to_rpp(&scythe_core::rpp::project_from_rpp(&text).expect("parse")));
}
