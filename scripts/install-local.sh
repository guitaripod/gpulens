#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
cargo install --path . --locked --force
expected="gpulens $(cargo pkgid | sed 's/.*[#@]//')"
installed="$("$HOME/.cargo/bin/gpulens" --version)"
if [ "$installed" != "$expected" ]; then
    echo "installed binary reports '$installed', expected '$expected'" >&2
    exit 1
fi
echo "installed: $installed ($HOME/.cargo/bin/gpulens)"
