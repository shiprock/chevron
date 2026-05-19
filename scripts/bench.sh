#!/usr/bin/env bash
# End-to-end timing of chevron subcommands with hyperfine.
#
# Measures the full cost a shell pays per prompt redraw: dynamic linker, crate
# init, libgit2 setup, the actual work, and writing to stdout. Complements the
# in-process criterion benches (`cargo bench`) which isolate individual fns.
#
# Usage: scripts/bench.sh [--export-markdown results.md]

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

if ! command -v hyperfine >/dev/null 2>&1; then
    echo "hyperfine not installed. Try: brew install hyperfine" >&2
    exit 1
fi

echo "Building release binary..."
cargo build --release --quiet

BIN="./target/release/chevron"
EXPORT_ARGS=("$@")

run() {
    local label="$1"
    shift
    echo
    echo "── $label ──"
    hyperfine --warmup 20 --shell=none "${EXPORT_ARGS[@]}" "$@"
}

# `path` is pure string work — establishes a floor for cold-start overhead.
run "path"                   "$BIN path"
run "path (truncated)"       "$BIN path 8"

# `git` exercises libgit2 status against this repo. Real-world workload.
run "git"                    "$BIN git"

# `tmux-title` does Repository::discover + the cheap GitInfo::gather.
run "tmux-title"             "$BIN tmux-title"

# `prompt` is what the shell actually runs every keystroke.
run "prompt"                 "$BIN prompt"
