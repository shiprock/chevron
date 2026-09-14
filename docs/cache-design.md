# Cache contracts and design review

Status: proposed design, reviewed against `master` at commit 55a2f4f on 2026-09-14.
This document does not claim the architecture is implemented or race-free.
Implementation and release status live in Beads epic `beads_plx-rzh`.

## Purpose and scope

A cache may reduce work or supply explicitly permitted stale data. It must not
change whose data is displayed, execute data as shell code, revive an invalidated
result, corrupt another file, or prevent the terminal from accepting input.

The scope includes shell rendering, startup hints, custom-command output, daemon
Git status, weather/location data, and health probes. Command history, captured
command output, and other durable records are not caches: they need separate
retention, durability and migration rules and must not be deleted by cache GC.
Probe caches themselves are owned by the daemon in memory. The filesystem holds a
runtime root for the socket and lock, a state root for durable records, and nothing
that a prompt process reads as data.

Relevant current code: [shell integration](../src/shell.rs),
[prompt writer](../src/main.rs), [command cache](../src/segments/custom_command.rs),
[daemon state](../src/daemon/state.rs), [daemon compute](../src/daemon/listener.rs),
[daemon client](../src/daemon/client.rs), [daemon protocol](../src/daemon/proto.rs),
[daemon lifecycle](../src/daemon/lifecycle.rs), [daemon paths](../src/daemon/paths.rs),
[weather](../src/weather/mod.rs), and [health](../src/health/cache.rs).

## Design decisions and rationale

| Decision | Rationale |
|---|---|
| Compose current cheap segments synchronously; refresh slow probes asynchronously | Avoid routine placeholder flicker without reusing the previous cycle's exit status, jobs, environment or config; measure the probe-free path before implementation |
| Use a neutral placeholder only when composition exceeds its budget | Preserve responsiveness on slow paths without making every prompt pay the visual cost |
| Retire the shared rendered startup cache; initially emit a no-op snippet | Startup cannot validate final config/environment; close the poisoning path without introducing new startup presentation machinery |
| Separate publication ownership from atomic writes | A complete write can still contain an obsolete result |
| Own every probe cache in the daemon; keep nothing a prompt reads on shared disk | One coordination model replaces cross-process locks, temp files, GC and symlink rules; domain policies stay separate because freshness, privacy and offline requirements differ |
| Treat command TTL as explicit permission for bounded staleness | Cwd alone cannot capture environment, files, network, credentials or side effects |
| Include repository incarnation and generation in daemon tokens | Eviction/recreation must not validate an old completion |
| Add versioned cache-only and compute daemon requests | The existing status request computes on miss and the client computes inline on timeout; neither can serve a probe-free foreground |
| Treat actor death as a daemon fault that exits for respawn | Without the inline fallbacks a dead actor would silently remove the Git segment until a manual restart |
| Verify peer credentials on every socket connection, in both directions | Directory permissions can be defeated by precreation; peer UIDs cannot |
| Stop and status go over the verified socket; the lock holder is the only PID source | A pidfile in a shared directory is attacker-writable |
| Broadcast computed changes with a generation, not filesystem events | Many panes must not spawn many workers for a no-op |
| Bound recompute by measured cost and degrade detail first | A slow repository under churn must not pin a core or lose its segment |
| Mint the session identity at init without a fork and share it with history | One identity, one fewer startup fork, no inherited authority |
| Print one stderr line at init for security findings | Nobody runs doctor unprompted |

Evidence status of the findings, as recorded in this repository: literal prompt
expansion and cross-directory command-output reuse have red/green regression tests
in PR #23. The two-open prompt read race, custom-cache symlink write-through and
weather display-option mismatch are confirmed by inspection of the referenced code
but have no committed reproduction; the slice that fixes each must first land a
failing test that demonstrates it. The daemon late-insert race and multi-writer
publication schedules are code-level findings; controlled interleaving tests must
establish their behavior before implementation. Existing fixes in PRs #15 and #23
are useful foundations, not proof of these contracts.

## Threat model and assumptions

Protect against accidental concurrent writers, crashes, malformed/oversized local
entries, untrusted project-derived text, and another OS user precreating objects
in shared temporary locations. An attacker already executing as the same UID can
usually modify shell startup/configuration too; cache permissions are not isolation
from a compromised account. Credentials and project paths can still be sensitive
and must not leak through filenames, world-readable files or routine diagnostics.

The shared-location exposure is concrete today. The prompt cache directory falls
back to `/tmp` when `XDG_RUNTIME_DIR` is unset, which is the normal state on macOS
and on Linux sessions that did not pass through a systemd login, such as containers
and non-systemd distributions. The instant-prompt spool file and the custom-command
cache use `TMPDIR`, falling back to `/tmp`; macOS sets a private per-user `TMPDIR` by
default and most Linux sessions do not. Wherever one of these resolves to a shared
directory, another local user can precreate the per-UID directory or the PID-named
file. The kill switch and secure storage slices must be verified on both
configurations, not only on a single-user development machine.

The daemon shares that boundary and is currently weaker than the cache. Its socket,
lock, pidfile and history database resolve to a per-UID directory under `/tmp` when
`XDG_RUNTIME_DIR` is unset. The daemon creates that directory with a call that follows
existing paths and only then tightens permissions; the client connects to whatever
socket is there with no peer check, sends its handshake, and then sends every command
line and working directory as history events; `chevron daemon stop` signals whatever
PID the pidfile names. Another local user who precreates that directory therefore
receives the victim's command history, controls the victim's prompt content and can
make the victim signal their own processes. This is closed by the peer-credential
check, the shared root primitive and the lock-derived PID described under daemon
ownership, and it is not closed by the cache kill switch.

Do not treat an arbitrary override directory as safe because the user supplied it.
Conversely, paths such as macOS `/tmp` have legitimate platform-owned aliases: the
implementation must distinguish those from untrusted cache-child symlinks.
Filesystem access can stall below userspace even after validation. A deadline around
an ordinary blocking read is not a guarantee that the read will return; the transport
and render coordinator must preserve shell responsiveness independently of workers.

## Invariants and consistency model

1. Only the owning shell may install its rendered prompt or tmux-title result.
   Every response belongs to a shell session, prompt cycle and request.
2. A response is a complete validated snapshot. Metadata and payload cannot come
   from different file opens or different writes.
3. A known invalidation wins over any computation started before it. A rejected
   completion cannot publish to disk, update a title, or become fresh by receiving
   a new completion timestamp.
4. A hit requires the correct namespace, schema, semantic key and freshness
   policy. A parse failure, future timestamp, incompatible schema or unsafe file
   is a miss, never executable instructions or an automatically trusted fallback.
5. Shell expansion, ANSI styling, terminal controls and tmux formatting are
   separate output languages. Cached data remains data until its final renderer.
6. All reads, values, concurrent work, queues and metadata maps have finite
   bounds. Cache failure cannot introduce an unbounded UI wait.
7. File publication is atomic on supported local filesystems. Power-loss
   durability is not promised: lost/corrupt cache entries may be recomputed.
8. Staleness is domain-specific and explicit. A Git snapshot is not an atomic
   filesystem transaction, and a weather observation is not current merely
   because its file was just read.
9. No process trusts a directory, socket, lock or PID it has not verified belongs to
   its own UID. Peer credentials are checked before any byte is sent or served.
10. Subscribers learn about changes in computed state, never about raw filesystem
    events, and every notification carries the generation it describes.

Correctness is relative to captured inputs and observed invalidations. Chevron
cannot detect every filesystem change instantaneously or infer arbitrary command
dependencies. The design must expose these limits rather than imply stronger guarantees.

## Shell presentation and transport

The shell owns `session`, `cycle`, `latest_request`, and the last accepted response.
The binary generates the init script once per interactive shell start and embeds a
fresh random session identity in it as a literal, so no fork is needed to mint one.
PID alone is insufficient. A forked subshell inherits the variable but not the
authority: publication requires `ZSH_SUBSHELL` or `BASH_SUBSHELL` to be zero, and
fish uses its equivalent. The same identity replaces the separate history session
request, removing that startup fork as well.

At each `precmd`, capture exit status, duration and job count before other hooks
alter them. Advance the cycle and launch/render using that immutable context.
The worker loads current config once and collects its inputs once; it does not
write a shared final-prompt cache. Explicit `CHEVRON_CACHE_FILE` writes are removed
from the generated v2 path. Shell, config and protocol compatibility are negotiated
rather than guessed from the presence of `%{`.

The v2 async design composes current cheap segments synchronously in the Rust
renderer, using a cache-only snapshot of typed probe data, then refreshes only slow
probes asynchronously. The foreground phase must not start Git walks, commands,
network requests or health probes on a miss. Missing/invalidated probe data is omitted
or shown as unknown; it is never borrowed from a previous complete prompt. Updated
probe data is composed with the captured current-cycle context and accepted token.
Do not duplicate the Rust renderer in shell code.

This split changes the daemon protocol. The current protocol has one status request,
whose miss path computes in the handler, and the client falls back to an inline
libgit2 walk after a short socket timeout. The foreground therefore uses one
`SNAPSHOT` request that returns every cached typed probe for the cycle in a single
round trip, each as a hit, an explicit miss or busy, and never computes; the
asynchronous refresh uses `LEASE` and `PUT`, defined under daemon-owned probe caches.
Both arrive under a bumped protocol version negotiated in the existing `HELLO`
exchange. A new client that reaches an old daemon, or times out on
the socket, renders the Git segment as unknown in the foreground and triggers the
existing stop and auto-spawn paths off the critical path. Inline compute is not a
foreground fallback in any of these cases; only the explicit daemon-disabled mode
computes in the prompt process, through its own bounded worker.

Before implementing this split, benchmark a release-build probe-free render including
process launch, config loading, cache-only reads, formatting and shell installation.
Record p50/p95/p99 for cold and warm runs, representative configs and cache misses,
on macOS and Linux, including a loaded machine. Establish the foreground budget from
these results and interactive evidence. A neutral placeholder is only the over-budget
or failed-composition fallback, not the normal new-cycle path. A supervised render
must enforce the budget even if config/cache I/O stalls. If the measurements cannot
support this design, revisit the split before shipping routine placeholder behavior.
The synchronous integration retains bounded rendering until the split is ready.

Within the same cycle, the last accepted prompt can remain visible during refresh.
An event that arrives while work is running records one pending refresh. Subsequent
events coalesce; the final event cannot disappear. Allow at most one active worker
and one pending refresh per shell. Superseding a worker cancels/reaps it or waits
within a deadline before launching its replacement. Cancellation never sends a
signal to PID zero or a recycled process group.

On `cd`, a new prompt cycle, live-disable or exit, invalidate pending work and
cancel timers as appropriate. Reconnect requests a fresh snapshot; an event is a
resynchronization hint, not a complete journal. Overflow cannot be represented as
"nothing changed." Continuous traffic is rate-limited without starving the final
refresh when the stream becomes quiet. A notification whose generation matches the
last installed frame is dropped without spawning a worker.

Accept a response only if session/cycle/request still match. Preserve the existing
accept-line, transient-collapse and PS2 guards: record a valid response if useful,
but repaint only when it cannot rewrite continuation input or completed commands.
Resize forces re-render/reflow from current terminal geometry. Tmux title updates
also require the accepted token and current pane target; they must respect a user's
priority title. Probe data itself must not be keyed by terminal width or theme.

The internal v2 response is separate from the public `chevron prompt` text output.
Use a fixed-count UTF-8 line frame: versioned ASCII header with validated IDs and
status, one sanitized prompt line, one sanitized title line (possibly empty), and
an end marker. Reject extra records, NUL, oversized fields and incomplete frames.
Do not parse arbitrary TOML/JSON or evaluate escaped content in the shell.

For asynchronous Zsh, accumulate available bytes with a bounded nonblocking reader
(e.g. `zsh/system`'s `sysread`), with a separate response deadline. Readability is
not evidence of EOF: the callback must not call an unbounded `cat`/line read while
a worker or descendant can still hold the pipe. If the required module is absent,
fall back to the bounded synchronous path. Validate this on both supported OSes.
Bash/fish retain their synchronous integrations until they have equivalent tested
async lifecycle support. Encoding and byte limits must survive UTF-8 chunk boundaries.

## Startup behavior and compatibility

A startup hint cannot validate the environment that has not been loaded yet.
The first migration makes the emitted instant snippet a no-op, with no persisted
branch/status/identity and no provider, daemon or config I/O. Any future builtin
startup feedback needs separate UX evidence; it is not part of the kill switch.

The safe default does not redirect stdin/stdout/stderr into the existing predictable
spool file. If startup emits output, preserve it and put the real prompt on a new
line; never erase an assumed startup span after arbitrary `.zshrc` output. This
trades the old seamless startup repaint for simpler correctness. Clean takeover,
startup input/password prompts, errors and Ctrl-C must be demonstrated in PTY tests.
Reintroducing captured startup output would require a separate bounded-buffer design;
a regular file cannot enforce a hard bound on arbitrary redirected shell writes.

Existing pasted v1 snippets do not update when the binary updates. Stop generating
new legacy cache content, use a new snippet marker, and have doctor identify the
old marker with replacement instructions. Do not deserialize or execute legacy
rendered cache entries as a migration step. On first execution of the new binary,
unlink the one known legacy `last-prompt` entry so pasted v1 snippets do not paint a
permanently frozen prompt. Use a validated, owned parent directory and a
directory-relative unlink: never open the payload, follow the final symlink,
recursively remove a directory, or delete arbitrary `CHEVRON_CACHE_FILE` override
targets. Missing is success. Permission errors are reported by doctor with
remediation. An unsafe parent, one not owned by the user or not mode 0700, is
deliberately left alone and reported by doctor as a security finding rather than a
cleanup failure: the pasted v1 snippet keeps reading whatever that directory
contains until the user replaces the snippet.
Retry the idempotent cleanup on later executions if it could not complete. This narrow
migration helper does not depend on the shared root primitive. Test regular files,
symlinks (target survives), missing files, unsafe parents and repeated execution.

A pasted snippet may run before the new binary's first execution; that first startup
cannot be retroactively protected. Old running binaries may also recreate the file.
Doctor must explain reinitializing shells/replacing snippets and retiring old binaries;
packaging must not claim that deletion alone repairs all existing shell processes.
New init must detect an old binary/protocol and fall back to supported synchronous
rendering. Old shells/new binaries retain the public CLI format for a documented
transition period. Never silently re-enable the old shared cache as fallback.

## Domain policies

| Domain | Identity | Freshness and failure | Owner |
|---|---|---|---|
| Shell presentation | session, cycle, request, captured context | fresh synchronous composition; slow probes refresh asynchronously; placeholder only over budget | shell |
| Git status | canonical worktree plus actual gitdir/common-dir identity; daemon incarnation | current 100 ms TTL baseline plus watcher invalidation; invalidated data is not fresh | daemon actor |
| Custom command | command definition, scope path (cwd by default, worktree, global or session), cache schema, digest of declared context values with `PATH` included by default | explicit TTL; never serve expired output after command failure | daemon actor stores; the shell's worker computes |
| Weather observations | provider, provider account identity if semantically relevant, location resolution and units | configured fresh TTL; bounded stale-on-error, proposed maximum 6 hours | daemon actor |
| Location lookup | lookup source and declared context | bounded TTL for IP geolocation; explicit coordinates need no lookup | daemon actor |
| Health probe | host and actual target identity, probe/parser revision | existing probe TTL; expired results become unknown rather than falsely healthy | daemon actor |

Cache weather observations, not the rendered line. Render city/icon/font choices
on every invocation. Store resolved location labels separately from provider data;
location overrides must take effect even when an observation is reused. Coordinate
rounding must be an explicit provider-tolerance policy; it cannot silently merge
location-label identity or imply exact-coordinate equivalence. Cache
lookup must not first incur an unconditional IP-geolocation network request.
Account/API-key changes that alter response identity need an opaque account context
or a miss; secret credential values must not be written into keys or diagnostic logs.

For arbitrary commands, file dependencies cannot be inferred safely. Retain the
explicit TTL model, document it as permission to reuse potentially stale output,
and provide an uncached mode. Preserve the implicit 30-second TTL for existing
unversioned configs with a command but no TTL. Doctor warns that this legacy default
is deprecated and recommends writing an explicit TTL (including zero to disable).
Normal prompts remain silent. Neither shorten nor lengthen the effective TTL silently.
Require explicit opt-in only under a new, explicitly selected config schema; generated
configs state their TTL. Test omitted TTL, explicit zero and explicit nonzero values
across migration.

The key has a scope and a declared context. Scope is `cwd` by default for
compatibility, `worktree` for commands whose output is the same anywhere in a
repository, `global` for commands independent of location, and `session` for
commands whose environment cannot be declared, which the daemon can offer because it
is a resident owner keyed by the session identity. The declared context is a list of
nonsecret environment variable names whose values are digested into the key; `PATH`
is included by default so different toolchain environments never share output.
Undeclared environment dependence is the user's responsibility and is documented as
such. The shell's asynchronous worker runs the command in its own environment after a
`LEASE`, under its deadline, output cap and process-group cleanup, then stores the
result with `PUT`; the foreground only reads. Commands requiring fresh side effects
should not be cached. TTL zero neither reads nor writes entries. Failure backoff is
separate from cached success: a short bounded retry delay may prevent repeated failed
runs, but must not make expired output valid or survive a relevant key change. Do not
cache stderr, authentication failures as successful data, or incomplete output.

Health checks must identify the inspected disk/device or tool context, not just a
fixed label such as `disk_health`. If identity cannot be established, bypass caching.
Likewise, cwd canonicalization failure is a miss; don't collapse unrelated paths
onto an empty key. Symlinked shell PWD may be displayed logically while cache identity
uses the physical worktree. Linked worktrees share some Git metadata but not status:
changes to common refs can invalidate several worktrees without merging their keys.

## Filesystem roots and on-disk state

Chevron uses two filesystem roots and no probe-cache root. The runtime root holds
the daemon socket and lock. The state root holds durable records: the command-history
database, the event spool and the daemon log. Prompt processes never read a file from
either root as data; they talk to the daemon over the socket.

Resolution order for the runtime root: an explicit `CHEVRON_SOCKET_DIR` override,
then `$XDG_RUNTIME_DIR/chevron`, then on macOS the per-user temporary directory the
system provides through `confstr(_CS_DARWIN_USER_TEMP_DIR)`, which the platform creates
and owns, then elsewhere a per-UID directory under `/tmp` that must be created
exclusively or validated. The state root resolves from `CHEVRON_STATE_DIR`, then
`$XDG_STATE_HOME/chevron`, then `$HOME/.local/state/chevron`, then on macOS the
per-user directory from `confstr(_CS_DARWIN_USER_DIR)`. Neither root ever falls back
to the working directory or to a relative path; this includes the daemon log, which
currently falls back to `./chevrond.log`. Socket paths must stay under the platform
limit of roughly one hundred bytes, so leaf names stay short. If a root cannot be
obtained or validated, the component that needs it fails closed and doctor names the
path.

The command-history database and the event spool currently live in the runtime
directory. Systemd removes `/run/user/<uid>` at the last logout and macOS purges
`/tmp`, so history is lost on exactly the systems where the fallback applies. Both
move to the state root; the daemon copies an existing database from the old location
once, verifies it, and leaves the original for the user to remove.

Validation is one primitive shared by both roots. Create the leaf directory with an
exclusive `mkdir` at mode 0700. If it already exists, open it with `O_DIRECTORY` and
`O_NOFOLLOW`, `fstat` the descriptor, and require owner equal to the effective UID,
no group or other permission bits, and a real directory. Every file inside is opened
relative to that descriptor with no-follow semantics. A `canonicalize` or metadata
check followed by an ordinary path open is still racy and is not acceptable. Trusted
platform aliases such as macOS `/tmp` are resolved as path prefixes, never as
child symlinks. Never chmod a path owned by someone else to repair it. Explicit
overrides obey the same rules. A process whose effective UID differs from the
directory owner, such as `sudo` with a preserved HOME or runtime directory, bypasses
writes and never creates entries there.

Automatic maintenance never removes a file it does not own. Foreign-owned entries
inside a validated owned directory are listed by doctor, which offers an explicit
directory-relative unlink; that operation is safe because unlink does not follow the
final component and the directory is ours. The same explicit path removes the legacy
on-disk caches once nothing reads them: the hashed command files in the temporary
directory, the weather map and the health directory. They are never read again after
their replacement ships, and never deleted implicitly.

Durable writes in the state root use an exclusive unpredictable temporary file in the
destination directory, complete writes, close, then rename. Failure preserves the old
entry. The event spool on `unstable` already follows this pattern. History durability
is SQLite's responsibility; spool entries are a best-effort net and are not fsynced,
and nothing else on disk needs fsync.

## Daemon-owned probe caches

The daemon actor is the single owner of every probe cache: Git status, custom-command
output, weather observations, location lookups and health probes. Entries live in
memory as typed values in per-namespace maps keyed by opaque canonical keys, with
per-namespace entry caps and payload limits, least-recently-used eviction and a total
memory budget. Nothing a prompt reads is stored on shared disk, so cross-process
locks, temporary-file protocols, garbage collection, symlink rules and
network-filesystem detection disappear from the design. The socket, verified as
described under daemon ownership, is the only trust boundary. Outcomes remain
explicit: `Fresh`, `Stale`, `Miss`, `Busy` and `Unsafe`, never an empty string that
hides the cause.

Requests, all under a bumped protocol version negotiated in the existing `HELLO`
exchange: `SNAPSHOT` returns every cached typed probe a cycle needs in one round trip
and never computes; `LEASE` asks permission to compute one key and returns a token or
`Busy`; `PUT` completes a lease and is rejected when the token is stale; `SUBSCRIBE`
keeps its semantics and gains generations; `SHUTDOWN` and `VERSION` replace pidfile
handling. An unknown request kind is an error, and a client that meets an old daemon
proceeds as described under shell presentation.

Placement follows the dependency. Daemon workers compute Git status, fetch weather and
location, and run health probes; none of these depend on the caller's environment.
Custom commands depend on the shell's environment, so the shell's asynchronous worker
runs them itself, in its own environment and working directory, after obtaining a
lease, and stores the result with `PUT`. The lease prevents duplicate runs across
shells; the declared context in the key keeps shells with different environments apart.

Freshness starts at compute start, measured on the daemon's injected monotonic clock.
Wall-clock time is used only for provider-declared validity such as an observation
timestamp, and a future wall-clock value is invalid. A slow fetch never receives a new
lifetime at completion. Cache-clear is a request that invalidates a namespace or key
and increments its generation; it needs no coordination with writers because there is
one owner.

The daemon persists no probe cache in v2. A restart or idle exit starts cold: the first
prompt shows the Git segment as unknown until the asynchronous refresh lands, weather
fetches once, and health probes run once. If measurement shows that cold starts matter,
a later slice may write a typed snapshot of weather and health entries to the state
root at exit and reload it with validation; that is a single-writer file and needs
none of the retired multi-writer machinery.

Without the daemon, whether disabled with `CHEVRON_NO_DAEMON=1` or unavailable, there
is no probe caching: Git status computes inline under its bound, commands run on every
prompt under their timeout, and weather prints an empty line, because a status bar
calling it every second must not reach the network every second. Doctor and the
init-time notice explain the state. The `weather` cargo feature therefore depends on
the `daemon` feature, so the documented distribution builds keep their cache.

Initial in-memory limits, to validate before release: 256 KiB per command output,
64 KiB per weather or health entry, 128 KiB per internal prompt frame, 1024 command
entries, 64 weather entries, 32 health entries, the existing watched-repository cap,
and a 32 MiB total budget. These are design budgets, not measurements.

## Daemon invalidation and work ownership

The actor remains the single owner of Git cache state. Move miss coordination there
without performing libgit2 work on the actor thread. A miss leases a token containing
`daemon_incarnation`, `repo_incarnation`, and `generation` to one bounded worker.
Other requests join a bounded waiter set or receive an explicit busy result; they cannot
spawn unbounded duplicate computations. Limit total workers and queued repositories.

Every connection is authenticated by peer credentials before any byte is sent or
served: the client checks the daemon's UID with `getpeereid` on macOS and `SO_PEERCRED`
on Linux, and the daemon checks each accepted peer the same way. A mismatch closes the
connection, yields `Unsafe`, and is never retried with inline compute. This is the
backstop that holds even if a directory check is bypassed. The daemon creates its
runtime directory with the shared root primitive and refuses to start in a directory
it cannot validate; the client refuses to connect through one.

The daemon holds its lock as a POSIX record lock on one descriptor kept open for its
lifetime and never reopens the lock file, so an unrelated close cannot release it.
`chevron daemon stop` and `status` are `SHUTDOWN` and `VERSION` requests over the
verified socket. Only when the socket is unresponsive does `stop` fall back to a
signal, and then the PID comes from `F_GETLK` on the lock inside the validated
directory, which the kernel reports and only our UID can hold. The pidfile is removed;
no code path reads a PID from a file.

The current listener's inline computation on actor timeout or send failure is removed.
Handlers never compute without an actor-issued lease. A missed reply deadline returns
an explicit unavailable/busy response within the caller budget; it cannot start another
Git walk. The daemon client currently performs the costlier inline fallback: after a
short per-syscall socket timeout it runs a full libgit2 walk inside the prompt process.
The foreground cache-only request removes that fallback and renders the segment as
unknown; the asynchronous refresh is the only path that may wait for or trigger
compute. A deliberately daemon-disabled mode uses its own bounded render worker;
neither actor timeout nor socket timeout is a switch into that mode. Bound repository
discovery as well as status computation, outside the actor thread.

Removing the fallbacks makes actor liveness a daemon-level requirement. Today the
lifecycle spawns the state actor and discards its join handle, so a panicked actor
leaves a daemon that accepts connections but can never answer. Under this design that
would silently remove the Git segment until someone restarts the daemon. Actor send
failure or a dead actor thread is therefore a daemon fault: the serve loop must detect
it, stop accepting, remove its socket, release its lock, and exit nonzero so the client's
existing auto-spawn path starts a fresh daemon on the next miss. Restarting the actor
in place is acceptable only if every outstanding lease, waiter and subscriber is
failed explicitly first. Add a regression test that kills the actor and proves the
next client request leads to a respawn rather than an indefinite unavailable stream.

As a prerequisite within this slice, introduce an injected monotonic `Clock` interface
(`now()`), with production `Instant`-backed and manually advanced test implementations.
Route get/insert freshness, lease start, LRU and debounce decisions through it. Pair it
with explicit timer/tick delivery and a controllable compute executor so tests advance
time and complete jobs in chosen orders without real sleeps. Transport integration
may retain real timeout tests; actor correctness must not depend on wall-clock timing.

Invalidation increments generation even if the cached entry is absent. Completion
is accepted only for a matching live token, and freshness starts at compute start.
Rejected work releases its slot; one dirty/pending flag schedules a bounded retry
when subscribers or requests still need the result. Never wait for an obsolete
computation as though it were a fresh hit. Idle subscribers need a completion/resync
notification; TTL expiry on future reads alone is not an eventual-refresh guarantee.

Subscribers receive change notifications, not filesystem events. When a repository
with subscribers is invalidated, the actor schedules a recompute under the backoff
below, compares the summarized result with the last one broadcast, and notifies only
when it differs. The notification carries the repository incarnation and generation;
a shell that already displays that generation drops it without spawning a worker, so
file churn that leaves the summary unchanged costs zero prompt refreshes and a real
change costs one broadcast and one coalesced refresh per pane. A per-subscriber jitter
of up to 50 ms is an optional spreading measure, to be measured.

Recompute cost is bounded per repository. The actor records the last compute duration
and enforces a minimum interval of the larger of the TTL and three times that
duration, coalescing further invalidations into one pending recompute. When a compute
exceeds its per-repository budget, the next compute runs in a reduced-detail mode that
skips the untracked-file scan and the result is marked as reduced, so detail degrades
before the segment disappears. Doctor lists repositories that hit the budget and points
at Git's filesystem monitor and untracked cache as remedies. The factor and budget are
initial values for measurement.

Eviction must retire the repository incarnation, cancel/reject its in-flight work,
and remove its cache, watcher, LRU, debounce, waiter and pending-broadcast metadata.
Re-registering the same path allocates a new incarnation, preventing the ABA problem.
Watcher overflow/errors invalidate affected scopes (all if scope is unknown), request
resync, and fall back to TTL checks. Daemon restart invalidates old response tokens;
client reconnect does not require a new filesystem change to refresh the prompt.

## Deadlines, observability and rollout gates

Use one end-to-end render deadline and pass remaining budget to each probe. A set
of individually bounded segments can otherwise exceed the total budget. Exact
foreground/async budgets require measurement; an initial async target is 250 ms,
with no I/O wait inside a ZLE callback. The existing custom-command default is 100 ms
and weather's provider fetch limit is 3 seconds; namespace work must fit its caller's
budget. A subprocess deadline includes stdout draining, output limits, process-group
cleanup attempts. The coordinator must not wait indefinitely to reap a worker in
uninterruptible kernel I/O: return the safe fallback, retain a bounded quarantine
slot, and suppress further launches until it is reaped. This limits damage but cannot
promise to terminate a kernel-stuck process. The render coordinator must remain able
to enforce its deadline while workers perform filesystem/probe I/O. A worker cannot
inherit a response fd into unrelated descendants.

Doctor should report cache schema/namespace, safe-root status, counts/bytes, age and
miss reasons, plus pending/rejected work counts when available. Default diagnostics
must not print commands, full paths containing secrets, cached payloads or credentials.
Normal prompts remain silent on cache failures. Do not log on every hit/event.

Security findings are the exception to silence. `chevron init` performs a stateless
safety check that costs well under a millisecond: it validates the runtime root and,
if a daemon is listening, its peer UID. On a failure it prints one line to stderr
naming the finding and pointing at doctor, once per shell start; `CHEVRON_NOTICE=0`
silences the line and doctor keeps the detail. Per-prompt behavior stays silent: an
unsafe result renders the affected segment as unknown.

Measurement reuses what the repository has. The hyperfine script in `scripts/bench.sh`
already times whole subcommands including process launch; extend it to export JSON and
report distributions for the probe-free render, and keep the criterion benches for
in-process costs. Property tests with the existing `proptest` dependency cover the
frame decoder, the protocol decoder and key canonicalization, so malformed input is
explored rather than sampled.

Implementation ships in independently gated Beads slices:

| Slice | Scope and gate |
|---|---|
| Immediate kill switch — ships first and alone | Stop precmd cache-file reads and binary writes; emit a no-op instant snippet; safely unlink the known legacy entry on first execution. Prove poisoned bytes are not rendered/executed, deletion is narrow/idempotent, and existing Zsh regressions pass. No storage framework or v2 presentation work in this change. User-visible consequence for release notes: `CHEVRON_ASYNC=1` renders synchronously on every cycle until the composition slice lands; live-event refreshes keep working because they never used the cache file. |
| Socket trust boundary — ships second, also alone | Peer-credential checks in client and daemon; runtime root created and validated with the shared primitive; POSIX record lock on a lifetime descriptor; `SHUTDOWN` and `VERSION` replace the pidfile, with `F_GETLK` as the hung-daemon fallback; history database, spool and log move to the state root with a one-time verified copy. Gate: negative peer tests through an injected credential source plus a two-user manual check on macOS and Linux; a hostile pidfile and a precreated directory are both proven inert; history survives a simulated logout. |
| Measurement prerequisite | Benchmark probe-free composition end to end as above and select its budget before implementing the v2 split. Record distributions and configurations, not a single best-case timing. |
| Shell harness prerequisite | Add Bash and Fish PTY fixtures with hermetic startup, screen assertions, input/interrupt/resize support and Linux/macOS CI execution. Bash must exercise promptvars on/off; Fish must exercise startup and Enter/cancel behavior. This gates corresponding shell behavior changes and cross-shell acceptance claims. |
| Session ownership and sync composition | Depends on measurements, relevant PTY harnesses and the `SNAPSHOT` request; implement the init-minted session identity, current-context foreground composition, slow-probe refresh, bounded framing, generation-aware event handling and over-budget fallback. Preserve lifecycle and literal-rendering regressions. |
| Daemon-owned probe caches and legacy retirement | Add `SNAPSHOT`, `LEASE` and `PUT`; move weather, location, health and command caches into daemon namespaces with their policies, caps and no-daemon behavior; make `weather` depend on `daemon`; stop reading the legacy disk caches and add doctor cleanup; command TTL and scope compatibility tests. |
| Daemon ownership | First inject clock/timer/executor controls; then implement leases/incarnations, change-diff broadcasts with generations and jitter, cost backoff and reduced-detail mode, remove the listener and client inline fallbacks, make actor death a daemon exit, and prove controlled invalidation/eviction/deadline interleavings and resource limits. |

Between the kill switch and legacy retirement, the command, weather and health disk
caches keep their current behavior. The command cache's shared temporary directory is
the remaining known exposure in that window, so retirement should not trail the socket
slice by long.

The current PTY harness covers Zsh only, as recorded in [CLAUDE.md](../CLAUDE.md).
The Bash subprocess unit tests in PR #23 are valuable but are not PTY evidence.
Until the harness slice passes, Bash/Fish end-to-end requirements below are pending,
not satisfied by the current test suite. CI must install the shells and fail the
required gate if coverage is skipped. This prerequisite must not delay the narrow
Zsh legacy-cache kill switch.

Preserve existing red/green regressions. Disabling a new cache must leave bounded
rendering functional; rollback must not restore unsafe legacy reads.

| Scenario | Required evidence |
|---|---|
| Probe-free foreground composition, cold/warm and loaded host | measured latency distributions; normal cycles avoid placeholder flicker; over-budget fallback remains responsive |
| Legacy cache deletion and omitted command TTL migration | narrow idempotent unlink; pasted snippet sees no frozen payload after cleanup; unsafe parent reported as a security finding; legacy 30-second TTL preserved and doctor warns |
| Actor timeout/send failure while compute is running | explicit unavailable/busy response, no handler or client duplicate compute |
| Old daemon with new client; actor thread dies | foreground renders unknown without inline compute; daemon exits and is respawned on the next miss; no indefinite unavailable stream |
| Precreated runtime directory, foreign socket, or peer UID mismatch | daemon refuses to start there; client sends nothing and renders unknown; init prints one notice; doctor names the path |
| Hostile or stale pidfile; hung daemon | stop never signals a PID it did not derive from the held lock inside a validated directory |
| Many panes in one repository; churn that leaves the summary unchanged | one recompute, no broadcast when unchanged, one broadcast and one coalesced refresh per pane when changed |
| Slow repository under sustained churn | recompute interval scales with measured cost; detail degrades before the segment disappears; CPU stays bounded |
| `sudo` with preserved HOME or runtime directory | effective UID mismatch bypasses writes; foreign-owned entries are removed only by explicit doctor cleanup |
| Logout on systemd; `/tmp` purge on macOS | history database and spool survive in the state root |
| Daemon absent or disabled | bounded inline Git status, commands each prompt, weather empty with one notice; no per-second network calls from tmux |
| Custom command with `session`, `worktree` or `global` scope and declared context | no sharing across differing declared values; expected sharing within scope |
| Two shells in different directories/environments/configs | neither can install the other's result or overwrite its presentation state |
| Re-sourced init or nested interactive shell | no duplicate hooks, inherited publication authority or orphaned workers |
| Same cwd, new exit status/jobs/config/env | new cycle cannot present previous-cycle facts as current |
| Old completion after new cycle, cd, resize, disable or exit | response and title rejected; workers/fds/timers bounded and cleaned |
| Burst while render is in flight; then silence | coalesced work eventually displays the final valid state |
| Split response, truncated frame, missing EOF, oversized/invalid UTF-8 | bounded parser/deadline; no blocked editor or partial display |
| `%`, `$()`, backticks, newlines, ESC, OSC, tmux `#{...}` in data | literal display appropriate to each output language, no execution/control injection |
| Startup output, input, password prompt, error, Ctrl-C | no lost output, stolen stdin, wrong erasure or broken terminal descriptors |
| Reader overlaps rename in the state root | old or new complete entry; never header A/body B |
| Two writers or writer crashes at every publication step | old or new valid entry; no shared-temp corruption or unbounded orphan growth |
| Symlink/FIFO/device, wrong owner/mode, relative/missing roots | rejected without traversal, overwrite, blocking or fallback into cwd |
| Expiry, future clock, schema/config/provider/account change | miss or explicitly permitted bounded stale value |
| Weather changes city/icon/font options, offline lookup | fresh formatting of typed data; bounded stale/error policy and no mandatory network hit |
| Command cwd/env changes, failed command, large output, held-open pipe | correct key/policy, deadline and output cap, child cleanup |
| Invalidate during compute; evict/recreate; shared Git refs | obsolete token cannot republish, and all affected worktrees eventually refresh |
| Watcher loss, queue overflow, daemon restart | resync without an unrelated user command or lost-final-event dependency |
| Busy lease, failed probe, stuck filesystem worker | no duplicate-fetch fallback, retry storm or UI wait for uninterruptible reaping |
| Memory caps reached; state root full, read-only or exhausted | eviction within bounds; durable writes fail closed and doctor reports; silent normal rendering |
| Mixed old/new CLI, daemon and pasted snippets | explicit compatibility fallback, no heuristic execution of old cache bytes |

Concurrency tests use barriers/fake clocks and controlled completion order, not lucky
sleep schedules. Unit tests establish storage/actor invariants; executable shell and
PTY tests establish terminal behavior on Linux/macOS, Bash with promptvars on/off,
and Zsh with PROMPT_SUBST on/off. Fish needs display and startup coverage too.

Release decisions still needing measurement are the probe-free composition budget,
over-budget fallback UX, whole-render deadline, in-memory caps, the recompute backoff
factor and budget, broadcast jitter, and whether a typed exit snapshot of weather and
health entries is needed. Measurement precedes v2 presentation implementation.
Security, key identity, nonblocking UI behavior, and rejection of obsolete results
are requirements; measurements may tune budgets but cannot waive those invariants.
