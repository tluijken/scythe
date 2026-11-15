#!/usr/bin/env bash
# Prints the workspace version. With a tag it fails unless the tag is v<version>.
# Usage: scripts/release/version.sh [<tag>]
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml")
if [ -z "$version" ]; then
    echo "no version under [workspace.package] in Cargo.toml" >&2
    exit 1
fi
if [ $# -gt 0 ] && [ "$1" != "v$version" ]; then
    echo "tag $1 does not match the workspace version $version: expected v$version" >&2
    exit 1
fi
echo "$version"
