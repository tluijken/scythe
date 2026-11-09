fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(scythe_server::headless::main_with_args(&args));
}
