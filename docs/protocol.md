# chevrond wire protocol

Status: version 1 is what ships today in [proto.rs](../src/daemon/proto.rs).
Version 2 is proposed here. It changes the handshake, the value encoding and the
framing rules once, so that every later change can be additive. Implementation
tracking lives in the Beads epic named in [cache-design.md](cache-design.md).

## What version 1 gets right

Keep these properties for the life of the project.

- One message per line of text over a Unix stream socket. Every exchange can be
  read and reproduced with `socat`, and a new implementation in any language is an
  afternoon's work.
- `key=value` fields whose decoders ignore unknown keys. New fields already land
  without breaking older peers.
- Client-generated ULIDs and idempotent history writes, so a replayed or duplicated
  event lands once.
- A `VERSION` reply with three independent dimensions: binary, protocol, schema.
  A fourth, `build`, the compile-time commit identifier, was added as an optional
  key ahead of version 2; a reply without it decodes as `unknown`, which the client
  reports as a stale daemon.
- A line-length cap, per-connection timeouts and a bounded subscriber mailbox.
- A subscriber that reconnects with exponential backoff and a failure budget.

## What version 1 gets wrong for a long-lived tool

Each item names the code that shows it.

1. **Exact-match versioning.** Both ends require `HELLO` to carry exactly their own
   number and treat anything else as failure. Every change, even an added request,
   makes every mixed pair incompatible.
2. **Lossy encoding.** Paths are encoded through `to_string_lossy`, the daemon reads
   lines through `from_utf8_lossy`, and bytes at or above 0x80 pass through raw. A
   repository path that is not valid UTF-8 cannot round-trip and can be answered with
   another path's status.
3. **Mixed positional and keyed grammar.** `HELLO`, `STATUS`, `PING` and `ERR` carry
   positional arguments, so the handshake in particular cannot grow a field.
4. **Silent loss on the event stream.** The subscriber mailbox sheds events past its
   cap with no marker and no sequence numbers, so a subscriber cannot tell a quiet
   period from a gap. The cache design forbids representing overflow as no change.
5. **Unknown enumeration values are decode errors.** A new operation-state token from
   a newer daemon makes an older client reject the whole status line.
6. **No sender-side field caps.** The line cap is 8 KiB and a `CMD_START` line
   carries the full command text untruncated. A long pasted command exceeds the cap,
   the daemon closes the connection, and the history event is lost; a spooled copy
   would fail on every replay.
7. **Schema is not forward-safe.** A database whose schema version is newer than the
   binary knows is treated as version 1 and the migration is attempted; it fails on a
   duplicate column and the daemon exits with its stderr discarded, so a downgrade
   turns into an unexplained outage.
8. **No connection or subscriber caps.** The accept loop spawns a thread per
   connection with no limit, and I found no limit on the number of subscribers.
9. **No upgrade choreography.** Nothing tells a running daemon that its binary has
   been replaced, and a newer client can only give up.

## Principles

1. **Text, one message per line, ASCII on the wire.** Everything outside the
   unreserved set is percent-encoded, so lines are lossless for arbitrary bytes and
   cannot contain a stray newline.
2. **Keyed fields everywhere.** A message is `OPCODE key=value ...`. No positional
   arguments. Unknown keys are ignored; an unknown opcode after the handshake is an
   `ERR` reply, not a disconnect.
3. **Version for framing and semantics, capabilities for features.** Adding a request,
   a field or a topic never bumps the version. A capability token gates each optional
   feature, and a client never sends a request the daemon did not advertise.
4. **The daemon speaks the current and the previous version; the client speaks only
   the current.** Client and daemon ship in the same release, so a mixed pair exists
   only during an upgrade window or when two installs coexist.
5. **A daemon that meets a newer client finishes the request and retires.** It never
   retires for an older client, which it can still serve. This ordering cannot
   flip-flop.
6. **Every enumeration decodes unknown tokens to an explicit unknown value.**
7. **Every message and field has a cap, and the sender truncates and flags.** The
   receiver never has to choose between failing and guessing.
8. **Required fields are frozen when published.** Fields are added as optional with
   defaults, deprecated for at least one major version, and removed only at a major
   bump.
9. **Peer credentials are verified before any byte is sent or served.** See the
   threat model in [cache-design.md](cache-design.md).
10. **The specification is normative and executable.** Golden vectors per version,
    property tests over the codec, and a cross-version job in CI.

## Transport

The socket lives in the runtime root defined in the cache design and is opened only
after the client has verified the peer UID; the daemon verifies each accepted peer
before reading. Connection limits and deadlines:

| Bound | Value |
|---|---|
| Pre-handshake deadline | 2 s |
| Request read timeout | 5 s |
| Relay write timeout | 5 s |
| Heartbeat interval in relay mode | 60 s |
| Maximum line length | 16 KiB |
| Maximum fields per line | 64 |
| Concurrent connections | 256, then accept, reply `ERR code=busy`, close |
| Subscribers | 128, oldest idle evicted with a resync marker |
| Outstanding leases per connection | 4 |

All values are initial and are tuned by measurement; the existence of each bound is
not negotiable.

## Encoding

A line is the opcode, then zero or more `key=value` fields separated by single
spaces, then `\n`. Keys match `[a-z][a-z0-9_.]*`. Values are byte strings encoded
so that every byte outside `A-Z a-z 0-9 - . _ ~ / : @ + ,` becomes `%XX` with
uppercase hex. That covers `%`, space, `=`, all control bytes, DEL and every byte at
or above 0x80, so a line is always printable ASCII, splits unambiguously on spaces
and on the first `=`, and reproduces arbitrary paths and command text exactly.
Decoders accept any well-formed `%XX` pair. Integers are decimal ASCII; booleans are
`true` or `false`; timestamps are Unix milliseconds; durations are milliseconds;
enumerations are lowercase tokens with an unknown catch-all; identifiers are at most
64 bytes.

Multi-line replies use a header line carrying `n=<count>`, exactly that many body
lines, then a line consisting of `END`. A reader that does not see `END` within the
read deadline discards the whole reply. Version 1 connections keep the version 1
encodings for the duration of the compatibility window.

## Handshake and negotiation

```text
client: HELLO proto=2 min=2 client=<semver> build=<id> caps=<a,b,c> session=<id>
daemon: HELLO proto=2 daemon=<semver> build=<id> caps=<a,b,c> retire=false
```

The daemon selects the highest version in the client's `[min, proto]` range that it
supports; with no overlap it replies `ERR code=version supported=<list>` and closes.
`caps` is the intersection the client may use. `session` is the shell session
identity from the cache design and lets the daemon release that session's leases and
subscriptions when its connection closes. `build` is a build identifier embedded at
compile time; it breaks ties between equal semantic versions and makes development
builds distinguishable.

Retirement: when the client's version is newer than the daemon's, or the versions are
equal and the build identifiers differ, the daemon sets `retire=true`, serves this
connection normally, then drains: it stops accepting, sends
`EVENT type=resync reason=retire` to subscribers, closes, and exits. The client, on
seeing `retire=true`, completes its request, waits a bounded time for the socket to
disappear, then spawns the daemon from its own binary. A daemon never retires for an
older client. Equal-version retirement means a development build takes effect on the
next prompt without manual restarts; document it as intended.

Version 1 window: a version 2 daemon accepts the positional `HELLO 1` and speaks
version 1 encodings on that connection for one release. A version 2 client that
receives `ERR` to its `HELLO` treats the daemon as version 1 and uses the lock-derived
stop path from the socket trust slice once; afterwards only `SHUTDOWN` is used.

Initial capability tokens: `status`, `snapshot`, `lease`, `subscribe.seq`,
`shutdown`, `history`, `capture`. Each new optional feature adds a token.

## Requests and replies

`STATUS path=<bytes>` answers `STATUS found=<bool> ...fields` with the current
repository fields plus `gen=<incarnation:generation>` and `age_ms`. On a miss it
computes, as today, only when the `snapshot` capability is absent; a version 2 client
uses `SNAPSHOT` for the foreground.

`SNAPSHOT cycle=<id> probes=<ns:key,...>` answers a header `SNAPSHOT cycle=<id>
n=<k>`, then one `PROBE ns=<ns> key=<key> state=fresh|stale|miss|busy|unsafe
gen=<g> age_ms=<n> schema=<n> ...fields` line per requested probe, then `END`. It
never computes. Each probe namespace carries its own `schema` number so payloads
evolve independently of the protocol version. At most 16 probes per request.

`LEASE ns=<ns> key=<key> deadline_ms=<n>` answers `LEASE granted=<bool>
token=<t> gen=<g> reason=busy|unavailable`. A lease expires at its deadline and is
released when its connection closes, whichever comes first.

`PUT token=<t> ns=<ns> key=<key> computed_at_ms=<n> ...payload` answers `PUT
accepted=<bool> reason=stale|expired|toobig`. Payload caps come from the cache
design's per-namespace limits.

`INVALIDATE ns=<ns> [key=<key>]` answers `INVALIDATE gen=<g>`. This is cache-clear.

`SUBSCRIBE cwd=<bytes> shell_cwd=<bytes> topics=<list> since=<seq>` answers
`SUBSCRIBE ok=true seq=<current>` and enters relay mode, which remains terminal for
the connection; a client that also needs requests holds a second connection. Relay
lines are `EVENT seq=<n> type=<topic> [cwd=] [id=] [gen=]`, `PING ts=<ms> seq=<n>`,
and `EVENT seq=<n> type=resync reason=overflow|retire|restart`. Sequence numbers
are per subscription and contiguous; a gap or a resync means invalidate everything
and request a snapshot. The actor reserves one mailbox slot so an overflow always
delivers the resync marker instead of dropping silently.

`CMD_START` and `CMD_END` keep their fields. The sender caps `cmd` at 8 KiB of
encoded bytes and sets `cmd_truncated=true` when it does; `session` is the shared
session identity. Idempotency rules are unchanged.

`VERSION` answers `VERSION binary=<semver> build=<id> proto=<n> min_proto=<n>
schema=<n> caps=<list>`.

`SHUTDOWN drain=<bool>` answers `SHUTDOWN ok=true` and the daemon exits after
draining, sending `resync reason=restart` to subscribers. `QUIT` closes the
connection.

`ERR code=<token> msg=<bytes>` is the only error shape. Codes are stable tokens:
`version`, `unsupported`, `malformed`, `toolong`, `busy`, `unavailable`,
`internal`. New codes are additive; clients treat unknown codes as `internal`.

## Upgrade mechanisms

1. **Negotiated version range and capabilities**, so additive changes need no bump.
2. **Daemon supports N and N-1; client speaks N**, so a mixed pair always has a
   common version during the window.
3. **Retire on newer client**, so the window closes on first contact without a
   filesystem watch or a manual restart, and works with Nix store paths where the
   old binary remains present.
4. **Additive-only evolution with deprecation**: optional fields with defaults, one
   major version of deprecation, removals only at a major bump, unknown tokens
   decoded to unknown.
5. **Schema**: `meta.schema_version` becomes an integer; the daemon refuses to open a
   newer schema with a doctor-visible finding and never runs a migration on a version
   it does not know; migrations are forward-only, idempotent, and tested from a
   fixture of every prior version.
6. **Config**: `schema = <n>` in the TOML file; `chevron configure` migrates; an
   unknown newer schema is refused with a message rather than guessed.
7. **Shell contract**: the generated init script carries the protocol version it was
   generated for, and doctor reports a pasted snippet whose version no longer matches
   the binary.
8. **Executable specification**: golden vectors for every version under a
   `tests/protocol/` tree; property tests over encode and decode for arbitrary byte
   strings, including NUL, newline, percent and invalid UTF-8; a CI job that builds
   the previous release tag and runs old client against new daemon and new client
   against old daemon; a `chevron daemon probe` subcommand that prints the
   negotiated version and capabilities for humans.

## Alignment with the architecture direction

A resident client per shell, as decided in [architecture.md](architecture.md), pays
the handshake once per shell lifetime, holds one control connection and one relay
connection, and lets the daemon tie leases and subscriptions to the session so a dead
client cleans up on disconnect. A split into a thin `chevron` client and a fat
`chevrond` keeps working because both carry the same version and build identifier in
`HELLO`. Keyed ASCII lines and stable error codes remove every heuristic the shell or
client would otherwise apply to daemon output, which is the same property the cache
design demands of the prompt frame.
