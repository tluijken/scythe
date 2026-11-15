#!/usr/bin/env bash
# Builds Scythe (scythe-app, scythe-server and scythe-mcp) in release, without the asio feature,
# and packs it with the licenses (notices.sh) into <out dir>, as release.yml publishes it:
#   Linux    scythe-<version>-linux-<arch>.tar.gz
#   Windows  scythe-<version>-windows-x86_64.msi, made by cargo-wix with WiX 3 from
#            crates/scythe-app/wix/main.wxs, and scythe-<version>-windows-x86_64.zip
#   macOS    scythe-<version>-macos-universal.zip: Scythe.app with universal binaries (Apple
#            silicon and Intel), signed ad hoc
# Usage: scripts/release/package.sh <out dir>
set -euo pipefail

if [ $# -ne 1 ]; then
    echo "usage: $0 <out dir>" >&2
    exit 2
fi
here=$(cd "$(dirname "$0")" && pwd)
out=$(mkdir -p "$1" && cd "$1" && pwd)
cd "$here/../.."
version=$("$here/version.sh")
target_dir=${CARGO_TARGET_DIR:-target}
bins=(scythe-app scythe-server scythe-mcp)
packages=(-p scythe-app -p scythe-server -p scythe-mcp)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

case "$(uname -s)" in
    Linux)
        name=scythe-$version-linux-$(uname -m)
        cargo build --release --locked "${packages[@]}"
        mkdir -p "$stage/$name"
        for bin in "${bins[@]}"; do
            cp "$target_dir/release/$bin" "$stage/$name/"
        done
        "$here/notices.sh" "$stage/$name" scythe-app linux
        tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
        echo "$out/$name.tar.gz"
        ;;
    MINGW* | MSYS* | CYGWIN*)
        name=scythe-$version-windows-x86_64
        cargo build --release --locked "${packages[@]}"
        cargo wix -p scythe-app --no-build --nocapture --output "$out/$name.msi"
        mkdir -p "$stage/$name"
        for bin in "${bins[@]}"; do
            cp "$target_dir/release/$bin.exe" "$stage/$name/"
        done
        "$here/notices.sh" "$stage/$name" scythe-app windows
        (cd "$stage" && 7z a -tzip "$out/$name.zip" "$name")
        echo "$out/$name.msi"
        echo "$out/$name.zip"
        ;;
    Darwin)
        name=scythe-$version-macos-universal
        cargo build --release --locked "${packages[@]}" --target aarch64-apple-darwin --target x86_64-apple-darwin
        app=$stage/$name/Scythe.app
        mkdir -p "$app/Contents/MacOS"
        for bin in "${bins[@]}"; do
            lipo -create -output "$app/Contents/MacOS/$bin" \
                "$target_dir/aarch64-apple-darwin/release/$bin" \
                "$target_dir/x86_64-apple-darwin/release/$bin"
        done
        cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>scythe-app</string>
    <key>CFBundleIdentifier</key>
    <string>com.scythe.scythe</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>Scythe</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Scythe records and monitors the inputs of your audio interface.</string>
</dict>
</plist>
PLIST
        # codesign seals the helpers in Contents/MacOS as nested code, so they are signed first.
        codesign --force --sign - "$app/Contents/MacOS/scythe-server" "$app/Contents/MacOS/scythe-mcp"
        codesign --force --sign - "$app"
        "$here/notices.sh" "$stage/$name" scythe-app macos
        ditto -c -k --keepParent "$stage/$name" "$out/$name.zip"
        echo "$out/$name.zip"
        ;;
    *)
        echo "unsupported system: $(uname -s)" >&2
        exit 1
        ;;
esac
