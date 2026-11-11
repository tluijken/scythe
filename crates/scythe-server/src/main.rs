fn main() {
    if let Some(code) = scythe_engine::plugin_scan::helper_main(&std::env::args().collect::<Vec<_>>()) {
        std::process::exit(code);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(scythe_server::headless::main_with_args(&args));
}
