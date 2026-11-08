#!/usr/bin/env bash
# Runs cargo with the devbox library paths: `devbox run -- scripts/cargo.sh test`.
. "$(dirname "$0")/env.sh"
exec cargo "$@"
