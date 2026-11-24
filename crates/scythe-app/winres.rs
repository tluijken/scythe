//! The Windows resources of Scythe's programs, shared by the build scripts of scythe-app,
//! scythe-server and scythe-mcp (`#[path]`): a VERSIONINFO naming the product Scythe and the
//! workspace version, which Explorer, the installer and code signing read, and the icon if any.

use std::path::Path;

/// Embeds the resources into `bin` when building for Windows; elsewhere it does nothing.
pub fn embed(bin: &str, description: &str, icon: Option<&str>) {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let env = |name: &str| std::env::var(name).unwrap();
    let version = env("CARGO_PKG_VERSION");
    let numeric = format!(
        "{},{},{},0",
        env("CARGO_PKG_VERSION_MAJOR"),
        env("CARGO_PKG_VERSION_MINOR"),
        env("CARGO_PKG_VERSION_PATCH")
    );
    let icon = match icon {
        Some(icon) => {
            let path = Path::new(&env("CARGO_MANIFEST_DIR")).join(icon);
            println!("cargo:rerun-if-changed={}", path.display());
            format!("1 ICON \"{}\"\n", path.display().to_string().replace('\\', "/"))
        }
        None => String::new(),
    };
    let rc = format!(
        r#"{icon}1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEFLAGSMASK 0x3f
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "Thomas Luijken"
            VALUE "FileDescription", "{description}"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "{bin}"
            VALUE "LegalCopyright", "Copyright (c) Thomas Luijken"
            VALUE "OriginalFilename", "{bin}.exe"
            VALUE "ProductName", "Scythe"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    );
    let path = Path::new(&env("OUT_DIR")).join(format!("{bin}.rc"));
    std::fs::write(&path, rc).unwrap();
    println!("cargo:rerun-if-changed=build.rs");
    // This file, from each of the three crates (file!() would be relative to the workspace).
    println!("cargo:rerun-if-changed=../scythe-app/winres.rs");
    embed_resource::compile_for(&path, [bin], embed_resource::NONE).manifest_optional().unwrap();
}
