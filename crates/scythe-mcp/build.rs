#[path = "../scythe-app/winres.rs"]
mod winres;

fn main() {
    winres::embed("scythe-mcp", "Scythe MCP server", None);
}
