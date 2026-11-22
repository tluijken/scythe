#[path = "winres.rs"]
mod winres;

fn main() {
    winres::embed("scythe-app", "Scythe", Some("assets/scythe.ico"));
}
