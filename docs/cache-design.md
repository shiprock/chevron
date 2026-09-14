# Cache contracts and design review

Status: proposed design, reviewed against the implementation on 2026-09-14.
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

Relevant current code: [shell integration](../src/shell.rs),
[prompt writer](../src/main.rs), [command cache](../src/segments/custom_command.rs),
[daemon state](../src/daemon/state.rs), [daemon compute](../src/daemon/listener.rs),
[weather](../src/weather/mod.rs), and [health](../src/health/cache.rs).

## Findings that change the earlier proposal

| Earlier idea | Gap found in review | Revised decision |
|---|---|---|
| Move the rendered prompt into session memory | The next command can change exit status, jobs, environment or config without changing cwd | Never treat the previous cycle's complete prompt as the new cycle's answer |
| Persist a validated instant-prompt hint | Before `.zshrc` runs, final config/environment are unavailable; secure disk reads and output capture add substantial complexity | Retire the shared rendered startup cache; make v2 startup feedback a neutral, shell-local placeholder |
| Use atomic writes everywhere | Unique temporaries prevent partial writes, but an obsolete computation can still publish last | Specify publication ownership and invalidation tokens separately from atomicity |
| Put every result in one generic cache | Different domains require different freshness, privacy and offline policies | Share storage primitives, not domain policy or a global result map |
| Add cwd to arbitrary command keys | Commands can depend on environment, external files, time, network, credentials or side effects | Caching is explicit bounded staleness, not a claim of dependency completeness |
| Add generations to daemon work | Eviction/recreation can reuse a generation; watcher loss and late completions need handling too | Token includes a unique repository incarnation and invalidation generation |

The two-open prompt read race, literal prompt expansion, custom-cache symlink
write-through, and weather display-option mismatch were reproduced. The daemon
late-insert race and multi-writer publication schedules are code-level findings;
controlled interleaving tests must establish their behavior before implementation.
Existing fixes in PRs #15 and #23 are useful foundations, not proof of these contracts.

## Threat model and assumptions

Protect against accidental concurrent writers, crashes, malformed/oversized local
entries, untrusted project-derived text, and another OS user precreating objects
in shared temporary locations. An attacker already executing as the same UID can
usually modify shell startup/configuration too; cache permissions are not isolation
from a compromised account. Credentials and project paths can still be sensitive
and must not leak through filenames, world-readable files or routine diagnostics.

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

Correctness is relative to captured inputs and observed invalidations. Chevron
cannot detect every filesystem change instantaneously or infer arbitrary command
dependencies. The design must expose these limits rather than imply stronger guarantees.

## Shell presentation and transport

The shell owns `session`, `cycle`, `latest_request`, and the last accepted response.
The session identity is regenerated for each interactive shell initialization;
PID alone is insufficient, and a subshell must not inherit publication authority.

At each `precmd`, capture exit status, duration and job count before other hooks
alter them. Advance the cycle and launch/render using that immutable context.
The worker loads current config once and collects its inputs once; it does not
write a shared final-prompt cache. Explicit `CHEVRON_CACHE_FILE` writes are removed
from the generated v2 path. Shell, config and protocol compatibility are negotiated
rather than guessed from the presence of `%{`.

The default synchronous mode continues to compose fresh prompt output from cheap
inputs and cached probe data. Async mode immediately shows a neutral pending
placeholder on a new cycle, then installs the valid result. It does not display
the previous command's success/failure or another environment as current. This is
an intentional UX change requiring the performance and PTY gates below. A later
optimization may compose current cheap segments with cached typed probes; it must
not duplicate the full Rust renderer in shell code to regain the old fast path.

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
refresh when the stream becomes quiet.

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
The v2 instant snippet therefore uses only a neutral builtin placeholder, no
persisted branch/status/identity and no provider, daemon or config I/O.

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
rendered cache entries as a migration step. Legacy files may remain until safe,
explicit cleanup; packaging must not assume a binary update repairs pasted snippets.
New init must detect an old binary/protocol and fall back to supported synchronous
rendering. Old shells/new binaries retain the public CLI format for a documented
transition period. Never silently re-enable the old shared cache as fallback.

## Domain policies

| Domain | Identity | Freshness and failure | Owner |
|---|---|---|---|
| Shell presentation | session, cycle, request, captured context | no cross-cycle reuse of full output; pending placeholder on miss | shell |
| Git status | canonical worktree plus actual gitdir/common-dir identity; daemon incarnation | current 100 ms TTL baseline plus watcher invalidation; invalidated data is not fresh | daemon actor |
| Custom command | command definition, canonical cwd, cache schema, explicitly declared nonsecret context | explicit TTL; never serve expired output after command failure | secure disk namespace |
| Weather observations | provider, provider account identity if semantically relevant, location resolution and units | configured fresh TTL; bounded stale-on-error, proposed maximum 6 hours | secure disk namespace |
| Location lookup | lookup source and declared context | bounded TTL for IP geolocation; explicit coordinates need no lookup | secure disk namespace |
| Health probe | host and actual target identity, probe/parser revision | existing probe TTL; expired results become unknown rather than falsely healthy | secure disk namespace |

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
and provide an uncached mode. A legacy implicit 30-second default must be migrated
visibly, not silently lengthened. Require explicit opt-in for new persistent command
caching; users needing environment-dependent freshness use declared nonsecret keys,
session-only caching, or disable it. Commands requiring fresh side effects should
not be cached. TTL zero neither reads nor writes persistent entries. Failure backoff is separate
from cached success: a short bounded retry delay may prevent repeated failed probes,
but must not make expired command output valid or survive a relevant key change.
Do not cache stderr, authentication failures as successful data, or incomplete output.

Health checks must identify the inspected disk/device or tool context, not just a
fixed label such as `disk_health`. If identity cannot be established, bypass caching.
Likewise, cwd canonicalization failure is a miss; don't collapse unrelated paths
onto an empty key. Symlinked shell PWD may be displayed logically while cache identity
uses the physical worktree. Linked worktrees share some Git metadata but not status:
changes to common refs can invalidate several worktrees without merging their keys.

## Shared disk-cache primitive

Use a small Rust implementation with opaque namespace/key types. Callers supply
policy; the storage layer does not know weather or prompt semantics. The API should
return outcomes such as `Fresh`, `Stale`, `Miss`, `Unsafe` and `Busy`, not collapse
all causes into an empty string. Invalid data is never a stale fallback.

Runtime and persistent roots are distinct. Prefer a valid user runtime directory
for ephemeral locks; use a verified user cache root for persistent probe results.
Do not fall back to the repository working directory when HOME/XDG paths are absent
or relative. If a safe root cannot be obtained, bypass disk caching.

Create the Chevron leaf directory exclusively with mode 0700, or open and validate
an existing directory's owner and permissions. Files use 0600. Use directory-relative
opens and validate the opened descriptors; a `canonicalize`/metadata check followed
by an ordinary reopen is still racy. Reject final-component symlinks, nonregular files,
and unexpected ownership. Handle trusted platform aliases such as macOS `/tmp`
without treating attacker-controlled child paths as trusted. Do not chmod somebody
else's path to "repair" it. Test overrides obey the same rules as normal roots.

A read opens once with no-follow/nonblocking semantics, checks type/owner/size,
reads at most the configured limit, then validates schema, key and timestamps from
that same snapshot. This excludes FIFO/device hangs as well as split metadata/body
reads. Keys cannot contain path traversal; filenames derive from a stable versioned
digest of canonical key bytes. Do not store raw commands, secrets or full environments
in filenames/envelopes. Low-entropy secret values must not be treated as safe merely
because they were hashed; use an opaque context or disable persistence.

Each file contains one entry: schema and producer/parser revision, key digest,
fetch-start time, expiry/stale limits and a bounded typed payload. One file per key
avoids weather's whole-map read/modify/write lost updates. Treat future wall-clock
timestamps as invalid. Use monotonic time for in-process deadlines and freshness;
on disk accept bounded wall-clock freshness, including documented clock-jump limits.
A slow fetch must not receive a brand-new lifetime when it completes.

A writer uses an exclusive unpredictable temporary file in the destination directory,
checks write completion, closes, and atomically renames. Failure preserves the old
entry. Fsync is not needed for disposable cache durability. Never reuse a common
`.tmp` filename, and never open a pre-existing temp as writable. Temporary-file
cleanup only removes verified cache-owned entries; it must not follow links.

Publication needs coordination beyond rename. Use nonblocking cross-process locks
for refresh ownership. A contender uses an allowed fresh/stale value or receives `Busy`, distinct from
`Miss`. Callers must not respond to `Busy` by launching the same uncached fetch;
they omit the segment or use permitted stale data. Lock identity
must remain stable while holders exist: do not delete active lock inodes or steal a
lock merely because its recorded PID/mtime looks old. A fixed, bounded set of lock
stripes is an acceptable first implementation; unrelated-key collisions are handled
as `Busy`. Locks are released by descriptor close/process exit.

Any explicit cache-clear operation must coordinate with writers. With the refresh
lock held, clear the entry; if busy, report busy/retry rather than promise that a
still-running old fetch cannot repopulate it. Do not remove the lock inode. GC only
attempts nonblocking cleanup and cannot remove an active writer's temp. Maintenance
work is budgeted and kept off the synchronous prompt critical path.

Initial proposed limits, to validate before release: 256 KiB custom-command output,
64 KiB weather/health entry, 128 KiB internal prompt response, and 16 MiB/256 entries
per disk namespace. Cap serialized metadata as well as payload. These are design
budgets, not measurements of current behavior. Evict expired entries first; if limits
cannot be maintained within the maintenance budget, skip new writes. Local filesystems
are the supported concurrency target; uncertain network-filesystem lock/rename semantics
cause disk caching to be disabled, not an assertion of equivalent guarantees.

## Daemon invalidation and work ownership

The actor remains the single owner of Git cache state. Move miss coordination there
without performing libgit2 work on the actor thread. A miss leases a token containing
`daemon_incarnation`, `repo_incarnation`, and `generation` to one bounded worker.
Other requests join a bounded waiter set or receive an explicit busy result; they cannot
spawn unbounded duplicate computations. Limit total workers and queued repositories.

Invalidation increments generation even if the cached entry is absent. Completion
is accepted only for a matching live token, and freshness starts at compute start.
Rejected work releases its slot; one dirty/pending flag schedules a bounded retry
when subscribers or requests still need the result. Never wait for an obsolete
computation as though it were a fresh hit. Idle subscribers need a completion/resync
notification; TTL expiry on future reads alone is not an eventual-refresh guarantee.

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

Implementation ships in Beads-managed slices, with no merge of the full redesign
until each slice meets its tests. First secure file access and stop trusting shared
prompt output; then implement session ownership and v2 startup/protocol migration;
then consolidate data caches and generation-checked daemon work. Preserve existing
red/green regressions from the earlier fixes. Disabling a new cache must leave the
bounded uncached renderer functional; rollback must not restore unsafe legacy reads.

| Scenario | Required evidence |
|---|---|
| Two shells in different directories/environments/configs | neither can install the other's result or overwrite its presentation state |
| Re-sourced init or nested interactive shell | no duplicate hooks, inherited publication authority or orphaned workers |
| Same cwd, new exit status/jobs/config/env | new cycle cannot present previous-cycle facts as current |
| Old completion after new cycle, cd, resize, disable or exit | response and title rejected; workers/fds/timers bounded and cleaned |
| Burst while render is in flight; then silence | coalesced work eventually displays the final valid state |
| Split response, truncated frame, missing EOF, oversized/invalid UTF-8 | bounded parser/deadline; no blocked editor or partial display |
| `%`, `$()`, backticks, newlines, ESC, OSC, tmux `#{...}` in data | literal display appropriate to each output language, no execution/control injection |
| Startup output, input, password prompt, error, Ctrl-C | no lost output, stolen stdin, wrong erasure or broken terminal descriptors |
| Reader overlaps rename | old or new complete entry; never header A/body B |
| Two writers or writer crashes at every publication step | old or new valid entry; no shared-temp corruption or unbounded orphan growth |
| Symlink/FIFO/device, wrong owner/mode, relative/missing roots | rejected without traversal, overwrite, blocking or fallback into cwd |
| Expiry, future clock, schema/config/provider/account change | miss or explicitly permitted bounded stale value |
| Weather changes city/icon/font options, offline lookup | fresh formatting of typed data; bounded stale/error policy and no mandatory network hit |
| Command cwd/env changes, failed command, large output, held-open pipe | correct key/policy, deadline and output cap, child cleanup |
| Invalidate during compute; evict/recreate; shared Git refs | obsolete token cannot republish, and all affected worktrees eventually refresh |
| Watcher loss, queue overflow, daemon restart | resync without an unrelated user command or lost-final-event dependency |
| Busy lock, failed probe, stuck filesystem worker | no duplicate-fetch fallback, retry storm or UI wait for uninterruptible reaping |
| Capacity, inode exhaustion, read-only filesystem, disk full | old usable state preserved, bounded bypass, silent normal rendering |
| Mixed old/new CLI, daemon and pasted snippets | explicit compatibility fallback, no heuristic execution of old cache bytes |

Concurrency tests use barriers/fake clocks and controlled completion order, not lucky
sleep schedules. Unit tests establish storage/actor invariants; executable shell and
PTY tests establish terminal behavior on Linux/macOS, Bash with promptvars on/off,
and Zsh with PROMPT_SUBST on/off. Fish needs display and startup coverage too.

Release decisions still needing measurement are the async-placeholder UX, exact
whole-render deadline, maintenance budgets, and local-filesystem support detection.
Security, key identity, nonblocking UI behavior, and rejection of obsolete results
are requirements; measurements may tune budgets but cannot waive those invariants.
