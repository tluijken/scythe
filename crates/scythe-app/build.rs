fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile_for("assets/scythe.rc", ["scythe-app"], embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
