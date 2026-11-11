#!/usr/bin/env bash
# The integration tests (docs/testing.md): the window in egui's test harness, the real
# binaries driven over the control API, MCP and a headless browser, and every corpus project
# opened in the window. `devbox run itest [cargo test filter]`.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/env.sh
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export SCYTHE_ITEST_DIR="${SCYTHE_ITEST_DIR:-$PWD/target/itest}"
# Nothing the tests start reads or writes the user's own settings.
for d in config data cache; do mkdir -p "$SCYTHE_ITEST_DIR/xdg/$d"; done
export XDG_CONFIG_HOME="$SCYTHE_ITEST_DIR/xdg/config" XDG_DATA_HOME="$SCYTHE_ITEST_DIR/xdg/data"
export XDG_CACHE_HOME="$SCYTHE_ITEST_DIR/xdg/cache"
unset SCYTHE_CONTROL SCYTHE_CONTROL_TOKEN SCYTHE_TONE3000_CLIENT_ID SCYTHE_TONE3000_BASE_URL
# Nor the plugins installed on the machine: scanning those makes the tests slow and their
# results depend on what is installed.
export SCYTHE_SYSTEM_PLUGINS=0
unset CLAP_PATH LV2_PATH VST3_PATH VST_PATH SCYTHE_VST_PATH

# Xvfb and Chromium from PATH, else from nixpkgs when nix is there; the tests that need them
# say they are skipped when neither has them.
tool() {
  local name=$1 attr=$2 out
  if command -v "$name" >/dev/null; then
    command -v "$name"
  elif command -v nix-build >/dev/null && out=$(nix-build '<nixpkgs>' -A "$attr" --no-out-link 2>/dev/null); then
    [ -x "$out/bin/$name" ] && echo "$out/bin/$name"
  fi
  return 0
}
export SCYTHE_XVFB="${SCYTHE_XVFB:-$(tool Xvfb xvfb)}"
export SCYTHE_CHROMIUM="${SCYTHE_CHROMIUM:-$(tool chromium chromium)}"
echo "Xvfb: ${SCYTHE_XVFB:-none}; Chromium: ${SCYTHE_CHROMIUM:-none}"
echo "corpus: ${SCYTHE_ITEST_CORPUS:-$HOME/music}; probes: ${SCYTHE_ITEST_PROBES:-none}"

cargo build -p scythe-server -p scythe-app -p scythe-mcp
status=0
cargo test -p scythe-app --features itest -- itest:: "$@" || status=1
cargo test -p scythe-itest --features itest -- "$@" || status=1
exit $status
