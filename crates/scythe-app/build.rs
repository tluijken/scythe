fn main() {
    #[cfg(windows)]
    embed_resource::compile_for("assets/scythe.rc", ["scythe-app"], embed_resource::NONE).manifest_optional().unwrap();
}
