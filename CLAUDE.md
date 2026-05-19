# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```bash
cargo build --release        # optimized build
cargo run -- path             # run a subcommand during dev
cargo test                    # run unit tests
cargo clippy -- -D warnings   # lint (pedantic enabled in Cargo.toml)
nix build                     # Nix build (outputs to ./result)
nix run . -- git              # Nix run directly
```

## Architecture

chevron is a single-binary CLI that renders powerline-styled terminal segments for shell prompts (Starship) and tmux window titles. All code lives in `src/main.rs` (~370 lines).

**Three subcommands** (selected by first CLI arg):
- `path` — ANSI-colored powerline path with home collapsing and truncation
- `git` — Git status segment (branch, staged/modified/untracked/conflicted counts, ahead/behind, stash, rebase/merge state)
- `tmux-title` — Compact tmux window title with repo name, branch, dirty indicator

**Key design choice:** Uses the `git2` crate (libgit2 bindings) directly instead of shelling out to git, for performance in prompt rendering.

**Output formats:** `path` and `git` emit raw ANSI escape sequences; `tmux-title` emits tmux color codes (`#[fg=colorXX]`).

**Color helpers:** `fg()`/`bg()` wrap ANSI 256-color codes. Powerline arrow U+E0B0 separates segments.
