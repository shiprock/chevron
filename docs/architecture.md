# Architecture direction

Status: proposed on 2026-09-14, arising from the cache design review in
[cache-design.md](cache-design.md). These are decisions that reach beyond caches.
Each carries a status and the evidence that gates it. Nothing here is implemented.

## 1. The daemon is the runtime

Status: decided by the cache design; doctrine to be stated in code and docs.

The client module still describes the daemon as "always purely additive." After the
cache design it is required for probe caching, weather, history, live updates and
the security boundary. The principle becomes: the daemon is the runtime, the command
line is a thin client, and no-daemon mode is a documented degraded mode with bounded
inline rendering. Consequences: the `daemon` cargo feature stops being optional in
default and distribution builds, `weather` depends on it, and doctor explains the
degraded mode.

## 2. Process model: a resident client per shell

Status: proposed; gated on the measurement slice.

With default settings inside tmux, the zsh integration spawns these external
processes per command today:

| Source | Spawns per command |
|---|---|
| chevron: history start, history end, prompt render | 3 |
| stty around the two cursor-row queries | 6 |
| tmux: read priority title, set title | 2 |

Each spawn costs more than the socket round trips the cache design tunes. The
proposed model is one resident client per shell, started at init, holding a control
and a relay connection to the daemon and a pipe to the shell. Per-prompt spawns drop
to zero; the client owns the terminal query and raw-mode dance through libc instead
of six stty spawns; cancellation is a message rather than reaping a process; config
is read once and reloaded on change. This replaces the process-substitution transport
in the cache design, so it must be decided before the composition slice. The
measurement slice adds raw launch time of `stty -g` and `chevron version` as the
floor the alternatives are compared against.

## 3. Binary split

Status: proposed; follows from 1 and 2.

The prompt path today links git2, an image codec, an interactive prompt library, TLS
and SQLite, all paged in on every render. A thin `chevron` client and a fat `chevrond`
daemon shipped together in one release keep the hot path small and the security
surface of the hot path minimal. `HELLO` carries version and build identifiers so a
mixed pair is detected, per [protocol.md](protocol.md).

## 4. The shell integration is a kernel, not a script

Status: proposed.

About nine hundred lines of shell live inside Rust string constants, CLAUDE.md lists a
dozen hard-won invariants, and the cache design adds session, cycle, request, frame
parsing, generation checks and notices. The direction is less shell, not more: the
resident client absorbs the new logic; the scripts move to their own files included
at compile time so they can be linted and diffed; the shell-to-binary contract is
versioned so an old pasted init fails safely against a new binary; the PTY harness is
the kernel's test suite and grows to Bash and Fish as the cache design requires.

## 5. Product scope

Status: open; the owner's decision.

The issue tracker points at a session brain with atuin-style history and a sync
spike with end-to-end encryption. The cache design's threat model is local. Sync
makes a network-facing component out of the process that stores every command line,
which needs its own threat model covering keys, transport and server trust. The two
coherent shapes are a prompt with a local daemon that exports history, or a session
brain that syncs. Deciding before the daemon accumulates more data is cheaper than
deciding after.

## 6. Settings model

Status: proposed.

Two sources of truth exist: the TOML config, and environment variables that `chevron
init` bakes into the shell script at startup, plus runtime knobs read directly. With
a resident client, config is read in one place and reloaded on change, and environment
variables remain overrides. The config gains `schema = <n>` now, with `chevron
configure` performing migrations; the cache design's TTL change is the first use.

## 7. The protocol is the stable interface

Status: specified in [protocol.md](protocol.md).

Everything depends on it: the shell client, tmux status calls, doctor, and possibly
other tools. The protocol document is normative, defines negotiation in both
directions and the retirement choreography, and is backed by golden vectors and a
cross-version job in CI.

## 8. Testing and release process

Status: proposed.

Two-UID security tests run in a Linux container in CI and cannot run on the macOS
runner; the design says so and runs them where possible. The daemon end-to-end suite
loses its ignore marker as part of the daemon slice. The kill switch and socket
slices are security fixes and land on both `master` and `unstable` together with a
tag; the current divergence between those branches is a standing risk for a tool
meant to last, and a single trunk with release tags would remove it.

## Sequencing

The kill switch and socket trust slices depend on none of the above and proceed now.
Items 1 through 3 are decided, with measurements, before the composition and daemon
slices, because they change the transport and the binary layout. Item 5 is the only
goal-level question.
