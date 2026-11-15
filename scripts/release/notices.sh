#!/usr/bin/env bash
# Puts the licenses and notices every release archive carries into <dir>: LICENSE-MIT,
# LICENSE-APACHE, THIRD_PARTY_NOTICES.md, the fonts' OFL texts, and CRATE_LICENSES.md, which
# cargo-about generates from <crate>'s dependencies on <os> (linux, windows or macos) and which
# fails on a license about.toml does not accept.
# Usage: scripts/release/notices.sh <dir> <crate> <os>
set -euo pipefail

if [ $# -ne 3 ]; then
    echo "usage: $0 <dir> <crate> <os>" >&2
    exit 2
fi
crate=$2
case "$3" in
    linux) targets=(--target x86_64-unknown-linux-gnu) ;;
    windows) targets=(--target x86_64-pc-windows-msvc) ;;
    macos) targets=(--target aarch64-apple-darwin --target x86_64-apple-darwin) ;;
    *) echo "unknown os: $3" >&2; exit 2 ;;
esac
dir=$(mkdir -p "$1" && cd "$1" && pwd)
cd "$(dirname "$0")/../.."

cp LICENSE-MIT LICENSE-APACHE THIRD_PARTY_NOTICES.md crates/scyeq-ui/assets/fonts/*-OFL.txt "$dir/"
cargo about generate --locked --fail -c about.toml -m "crates/$crate/Cargo.toml" "${targets[@]}" \
    -o "$dir/CRATE_LICENSES.md" about.hbs
