# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Branches

- `unstable` is the integration branch: feature work lands here FIRST.
  The author's dotfiles flake tracks `github:shiprock/chevron/unstable`,
  so pushing it (plus `nix flake update chevron` + `darwin-rebuild` in
  ~/src/dotfiles) updates the day-to-day build.
- `master` is the stable line; tags are cut from it.
- Check which branch is checked out BEFORE starting work. If work does
  land on master, merge it into unstable promptly: the branches diverged
  once (2026-06) and reconciling cost a conflict-heavy shell.rs merge.

## Build & Run

```bash
cargo build --release        # optimized build
cargo run -- prompt 20 0 0 0  # run a subcommand during dev
cargo test                    # unit + CLI + PTY integration tests
cargo clippy --all-targets -- -D warnings   # lint (pedantic enabled in Cargo.toml)
cargo fmt                     # required; pre-commit hook rejects unformatted code
nix build                     # Nix build (outputs to ./result)
```

Git hooks (lefthook): pre-commit runs `cargo fmt --check` + clippy; pre-push
runs `cargo test` five times (flake detection) plus `nix build` and takes
1-3 minutes. CI mirrors this on a macOS/Linux matrix plus `cargo audit`.

## Architecture

chevron is a single-binary CLI (~15k lines) rendering powerline-styled prompt
segments, shell integration, and tmux window titles.

Subcommands (dispatched in `src/main.rs`): `path`, `git`, `nix-shell`, `aws`,
`prompt` (full composed prompt + tmux title line), `tmux-title`, `init
<zsh|bash|fish>`, `status`, `health`, `weather`, `banner`, `daemon
<serve|start|stop|status>`, `version`.

Modules:

- `src/segments/` — individual segments plus `prompt.rs` which composes them.
- `src/shell.rs` — the embedded shell init scripts emitted by `chevron init`
  (zsh/bash/fish). The zsh script carries the transient-prompt machinery and
  is the most subtle code in the repo; see below before touching it.
- `src/daemon/` — chevrond: TTL-cached `RepoStatus` served over a Unix
  socket, auto-spawned on cache miss, FS-watch invalidation (`daemon`
  feature; `CHEVRON_NO_DAEMON=1` opts out).
- `src/health/`, `src/weather/`, `src/banner/`, `src/sysinfo.rs` — auxiliary
  subcommands behind cargo features (`banner`, `weather`).
- `src/config.rs` — TOML config from `~/.config/chevron/config.toml`
  (`[segments]` toggles, `[segment.<name>]` blocks, `[weather]`).
- `src/color.rs` — ANSI 256-color helpers and per-shell escape wrapping
  (`CHEVRON_SHELL` selects `%{...%}` for zsh, `\[...\]` for bash).

Key design choices: git status via the `git2` crate (libgit2 bindings), never
a `git` subprocess, for prompt latency; powerline arrow U+E0B0 separates
segments; `tmux-title` emits tmux color codes (`#[fg=colourXX]`).

## zsh transient prompt (src/shell.rs)

On accept-line the prompt collapses to a neutral `❯ `; preexec saves the
cursor row via a DSR query (`ESC[6n` -> `ESC[row;colR`); precmd queries again
and rewrites the collapsed line in exit-status color via absolute positioning.
Env knobs: `CHEVRON_TRANSIENT`, `CHEVRON_OSC133`, `CHEVRON_ASYNC`,
`CHEVRON_TRANSIENT_DURATION_MS`.

Hard-won invariants, each backed by a regression test that was verified to
fail against the buggy version (do not relax these without re-running that
verification):

- Never query while input is pending (`zselect` probe): `read -d 'R'`
  otherwise swallows typed-ahead commands and a typed `R` truncates the
  exchange.
- Never leave a response in the tty queue: drain trailing duplicates after
  every successful read; on timeout, flag the in-flight response and sweep it
  in precmd. Stale responses reaching ZLE self-insert their tails as literal
  text (`1R`) when they arrive split across KEYTIMEOUT — only reproducible on
  slow machines.
- Erase the full wrapped span (`${(m)#...}` display cells / `COLUMNS`), not
  just one row; skip the rewrite entirely for multi-line (PS2) input, on any
  geometry change between preexec and precmd (resize), and when output
  scrolled the saved row away.
- Parse the response from the LAST CSI in the buffer, validate the row is
  numeric, and re-inject only printable typeahead via `print -z` (never ESC
  sequences or control bytes).
- Async fast path (`CHEVRON_ASYNC=1`): background refreshes carry a
  generation stamp; callbacks from superseded cycles discard their result.
- Never hang error suppression on a bare `exec`: every redirection on an
  `exec` is permanent, so `exec {fd}< /dev/tty 2>/dev/null` pointed the
  whole shell's stderr at /dev/null from the first prompt cycle — error
  messages vanished and ncurses `reset`/`tset` (which reach the terminal
  via fd 2) exited 1 silently. Scope it instead:
  `{ exec {fd}< /dev/tty } 2>/dev/null`.
- Flip raw mode BEFORE the pending-input probe: select on a canonical
  tty reports readability only at line boundaries, so a cooked probe is
  blind to partial-line input (paste leftovers after `read -rs`) and
  the query races it. A CSI-less read buffer is typeahead truncated at
  a typed `R` — re-inject it, never eat it. precmd's rewrite query must
  linger (drain in its own raw window) on timeout: unlike preexec's
  query there is no sweep behind it, and an unabsorbed straggler
  kernel-echoes at the cursor as literal `^[[68;1R`.
- Every tty dance (raw flip through restore) sits in a `{ try } always
  { restore }` block: ^C during the exchange or sweep unwinds the hook
  chain mid-function, and without the always-list the stty restore is
  skipped — terminal left raw with echo off, which the next exchange's
  `stty -g` save then makes permanent.
- Rewrite only after a real collapse: alternate accept widgets
  (accept-and-hold, custom widgets calling `.accept-line`) bypass the
  override, leaving the FULL prompt on screen — a rewrite sized for
  `❯ cmd` erases the wrong span over it. The row save is gated on a
  flag the accept-line override sets.
- Skip the rewrite when the transient's width is an exact multiple of
  COLUMNS: the cursor ends in auto-margin pending-wrap, from which
  emulators disagree on how the next newline advances (ble.sh's "xenl"
  trap) — the saved row may be off by one (duplicated-chevron glitch).
- Never `zle reset-prompt` from the async callback during a PS2
  continuation — it repaints PS2 re-expanded mid-handler, so `%_`
  picks up the callback's own parser stack and the user's `quote>`
  visibly mutates into `quote then cmdand>` (pure guards the same
  hazard). And `$CONTEXT` alone CANNOT stand guard: ZLE maps its
  widget specials (CONTEXT, PREBUFFER) only inside widgets, so in a
  `zle -F` fd handler CONTEXT expands empty and `!= cont` always
  passes. Capture `${(%):-%_}` as the callback's FIRST statement
  (bare stack = interactive state only), gate on CONTEXT-or-captured-%_,
  and execute the surviving `reset-prompt` as a plain top-level
  statement so flavors the probe can't see (backslash-newline opens
  no construct) still repaint with correct text.
- fish: collapse only when the line will execute — gate Enter on
  `commandline --is-valid` / empty buffer and skip when
  `--paging-mode` is active (Enter selects a completion); clear the
  armed flag on `fish_cancel`. Ported from starship/oh-my-posh.

## Testing

- Unit tests live inline; `tests/cli.rs` is `assert_cmd`-based CLI coverage.
- `tests/shell_pty.rs` is the PTY harness: it spawns a real interactive zsh
  in a pseudo-terminal with a hermetic `$HOME` and plays the terminal's role
  — a vt100 screen model interprets all output, and a responder answers DSR
  queries per a configurable mode (`Dsr::Immediate/Delayed/Silent/Fragmented/
  FocusNoise/DoubleResponse/AlternateDelayed`;
  `Render::Sync/SyncDelayed/Async/AsyncDelayed` controls `CHEVRON_ASYNC`
  and an optional render-latency wrapper). Assertions run
  against the rendered grid (rows containing `❯`, glyph counts, cell colors);
  failures dump the numbered screen plus an ESC-escaped raw byte tail.
- The harness requires `zsh` on PATH and skips loudly when absent (the nix
  sandbox has no zsh, which keeps `nix build` green).
- Adding a regression test for a shell.rs fix? Verify it bites:
  `git stash push src/shell.rs && cargo test --test shell_pty <name>; git stash pop`
  — it must fail pre-fix.
- Timing-sensitive PTY tests: prefer `wait_for(predicate)` over fixed sleeps.
  CI's loaded macOS runners reproduce scheduling races a fast local machine
  cannot (that is how the KEYTIMEOUT self-insert leak was found); treat
  shell_pty failures in CI as signal, not flakes, and read the screen dump in
  the panic message.

## Known gaps

- The PTY harness covers zsh only; bash (duration tag, OSC 133) and fish
  (Enter-binding transient) integrations are untested end-to-end.
- The *default* PTY suite sets `CHEVRON_NO_DAEMON=1` outside a git repo. The
  daemon path, git segment, and live prompt now have a separate `#[ignore]`d,
  daemon-backed e2e suite (`LiveFixture` in `tests/shell_pty.rs`): an
  in-process chevrond in a real repo, events injected via `state_tx`, redraws
  asserted against the grid. Run with `cargo test --test shell_pty --
  --ignored`; wiring it into CI and dropping the `#[ignore]` is the live-prompt
  graduation gate (chevron-ffu).
- A preexec DSR response that arrives after the 300 ms budget while an
  interactive `read` builtin owns the terminal is consumed as that
  read's input (a pasted secret gets the response prepended). Chevron
  cannot claw it back once the query is out; only skipping the preexec
  query entirely would close this, at the cost of the transient
  rewrite.
