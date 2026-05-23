//! Wire protocol for the chevrond daemon — line-oriented text over UDS.
//!
//! ## Grammar
//!
//! Each message is one line terminated by `\n`. The parser accepts an optional
//! trailing `\r` for resilience.
//!
//! ```text
//! Requests (client → server):
//!   HELLO <version>
//!   STATUS <path>
//!   CMD_START id=<ulid> session=<ulid> host=<host> cwd=<path> started_at=<ms> cmd=<cmd>
//!   CMD_END id=<ulid> finished_at=<ms> duration_ms=<u64> exit=<i32>
//!   SUBSCRIBE [cwd=<path>]
//!   QUIT
//!
//! Responses (server → client):
//!   HELLO <version>
//!   OK key=value key=value ...
//!   ACK
//!   NONE
//!   EVENT type=<topic> [cwd=<path>] [id=<ulid>]
//!   PING <unix-ms>
//!   ERR <reason>
//! ```
//!
//! ## Connection states
//!
//! A connection lives in one of two states after the HELLO handshake:
//!
//! 1. **Request/response** (default): client sends a request, daemon sends
//!    one response, repeat. Used for `STATUS`, `CMD_START`, `CMD_END`.
//! 2. **Subscriber-relay** (terminal — entered via `SUBSCRIBE`): the daemon
//!    writes `EVENT` lines on state changes and `PING` lines for keepalive;
//!    the client only reads. The client cannot return to request/response
//!    mode — to issue queries, open a separate connection. To leave the
//!    relay state, close the connection.
//!
//! `PING` is a typed opcode (not a magic comment) so subscribers can parse
//! it through the normal response decoder and discard. Heartbeat cadence is
//! a per-connection daemon choice (~60s); subscribers should not depend on
//! a specific interval.
//!
//! ## Encoding
//!
//! Numeric fields (`staged`, `modified`, …) and bools (`detached`) are written
//! plain. The optional `state` field is `rebasing|merging|cherry|bisect` or
//! omitted entirely. String fields (`repo_name`, `branch`, `cmd`, …) and the
//! `STATUS` path arg are **percent-encoded**:
//!
//! - `%` (0x25) → `%25`
//! - any byte at or below 0x20 (control + space) → `%XX` (uppercase hex)
//! - DEL (0x7F) → `%7F`
//!
//! Everything else (printable ASCII and any byte >= 0x80) is passed through
//! literally. The decoder accepts any well-formed `%XX` pair, so future
//! encoder changes that escape more bytes remain wire-compatible. This is
//! deliberately a small subset of RFC 3986 — just enough to keep the
//! line-oriented kv format unambiguous even when `cmd` contains tabs,
//! newlines, or other whitespace.
//!
//! ## Field order
//!
//! Encoders emit a fixed order for snapshot-friendly diffs, but the parser
//! tolerates any order and ignores unknown keys (forward-compat — phase 2 may
//! add `last_modified` or similar without breaking older clients).

use std::path::PathBuf;

use crate::segments::git::{OpState, RepoStatus};

/// The protocol version this build speaks. Bumped on incompatible changes.
pub const PROTO_VERSION: u32 = 1;

/// `CMD_START` payload. IDs are ULIDs minted client-side so preexec
/// doesn't need to await a daemon round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdStartEvent {
    pub id: String,
    pub session_id: String,
    pub hostname: String,
    pub cwd: PathBuf,
    pub cmd: String,
    /// Unix-epoch milliseconds. Stored as i64 to keep the door open for
    /// `time::OffsetDateTime` conversion later without unsigned-cast
    /// gymnastics, and to match `SQLite`'s INTEGER affinity.
    pub started_at_ms: i64,
}

/// `CMD_END` payload. `id` refers to a previously-published
/// [`CmdStartEvent`]; if the daemon never saw the start (e.g. the shell
/// hook fired into a then-dead daemon), the update is a silent no-op.
///
/// `output_bytes` and `output_truncated` are Phase 4 additions
/// (chevron-1yn.4). When `chevron capture` wraps a command, it sets
/// `output_bytes` to the number of bytes written to the capture file
/// and `output_truncated` to true if the file hit the size cap.
/// Regular (non-wrapped) `CMD_END` events leave both fields `None`,
/// which serialises to "omit the kv on the wire" — older daemons
/// just ignore the unknown keys and never read them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdEndEvent {
    pub id: String,
    pub finished_at_ms: i64,
    pub duration_ms: u64,
    pub exit_status: i32,
    pub output_bytes: Option<i64>,
    pub output_truncated: Option<bool>,
}

/// `SUBSCRIBE` payload. Phase 3 (chevron-1yn.3): a long-lived
/// subscriber connection that the daemon writes `EVENT` and `PING`
/// lines to as state changes. The optional cwd filter narrows
/// broadcasts to a single workdir; omitted means "all events the
/// daemon emits".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubscribeSpec {
    pub cwd: Option<PathBuf>,
}

/// `EVENT` payload. Topic is intentionally a free-form string
/// (currently `"git"` or `"cmd"`) so we can grow the topic set
/// without bumping the protocol version. cwd is set for `git`
/// events (the workdir whose state changed); id is set for `cmd`
/// events (the ULID of the command that fired).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPayload {
    pub topic: String,
    pub cwd: Option<PathBuf>,
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Hello(u32),
    Status(PathBuf),
    CmdStart(CmdStartEvent),
    CmdEnd(CmdEndEvent),
    Subscribe(SubscribeSpec),
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Hello(u32),
    /// `Some` → `OK …`; `None` → `NONE` (no repo discovered at the given path).
    Status(Option<RepoStatus>),
    /// Generic acknowledgement for fire-and-forget messages
    /// (`CMD_START`, `CMD_END`, `SUBSCRIBE`).
    Ack,
    /// Push-direction event line sent during subscriber-relay mode.
    Event(EventPayload),
    /// Keepalive heartbeat sent during subscriber-relay mode when no
    /// events have fired for ~60s. Carries a unix-epoch-ms timestamp
    /// the subscriber can use to detect daemon-side clock skew or
    /// stuck broadcast pipelines (otherwise just ignored).
    Ping(i64),
    Err(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtoError {
    /// Empty line — peer likely closed mid-stream.
    Empty,
    /// Unrecognised opcode (e.g. `FOO`).
    UnknownOpcode(String),
    /// Recognised opcode but malformed payload (e.g. non-numeric version,
    /// missing required key, bad percent-escape).
    Malformed(&'static str),
}

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("empty line"),
            Self::UnknownOpcode(op) => write!(f, "unknown opcode: {op}"),
            Self::Malformed(reason) => write!(f, "malformed message: {reason}"),
        }
    }
}

impl std::error::Error for ProtoError {}

// ── Encoding ────────────────────────────────────────────────────────────────

#[must_use]
pub fn encode_request(req: &Request) -> String {
    match req {
        Request::Hello(v) => format!("HELLO {v}"),
        Request::Status(p) => format!("STATUS {}", percent_encode(&p.to_string_lossy())),
        Request::CmdStart(e) => encode_cmd_start(e),
        Request::CmdEnd(e) => encode_cmd_end(e),
        Request::Subscribe(s) => encode_subscribe(s),
        Request::Quit => "QUIT".to_string(),
    }
}

#[must_use]
pub fn encode_response(resp: &Response) -> String {
    match resp {
        Response::Hello(v) => format!("HELLO {v}"),
        Response::Status(None) => "NONE".to_string(),
        Response::Status(Some(s)) => encode_status_ok(s),
        Response::Ack => "ACK".to_string(),
        Response::Event(e) => encode_event(e),
        Response::Ping(ts) => format!("PING {ts}"),
        Response::Err(reason) => format!("ERR {}", percent_encode(reason)),
    }
}

fn encode_subscribe(s: &SubscribeSpec) -> String {
    let mut out = String::with_capacity(32);
    out.push_str("SUBSCRIBE");
    if let Some(cwd) = &s.cwd {
        write_kv_str(&mut out, "cwd", &cwd.to_string_lossy());
    }
    out
}

fn encode_event(e: &EventPayload) -> String {
    let mut out = String::with_capacity(64);
    out.push_str("EVENT");
    write_kv_str(&mut out, "type", &e.topic);
    if let Some(cwd) = &e.cwd {
        write_kv_str(&mut out, "cwd", &cwd.to_string_lossy());
    }
    if let Some(id) = &e.id {
        write_kv_str(&mut out, "id", id);
    }
    out
}

fn encode_cmd_start(e: &CmdStartEvent) -> String {
    let mut out = String::with_capacity(192);
    out.push_str("CMD_START");
    write_kv_str(&mut out, "id", &e.id);
    write_kv_str(&mut out, "session", &e.session_id);
    write_kv_str(&mut out, "host", &e.hostname);
    write_kv_str(&mut out, "cwd", &e.cwd.to_string_lossy());
    write_kv_num(&mut out, "started_at", e.started_at_ms);
    // `cmd` last so it's easy to eyeball in `socat` dumps — the rest of the
    // fields are short and stable; cmd can run long.
    write_kv_str(&mut out, "cmd", &e.cmd);
    out
}

fn encode_cmd_end(e: &CmdEndEvent) -> String {
    let mut out = String::with_capacity(96);
    out.push_str("CMD_END");
    write_kv_str(&mut out, "id", &e.id);
    write_kv_num(&mut out, "finished_at", e.finished_at_ms);
    write_kv_num(&mut out, "duration_ms", e.duration_ms);
    write_kv_num(&mut out, "exit", e.exit_status);
    if let Some(n) = e.output_bytes {
        write_kv_num(&mut out, "output_bytes", n);
    }
    if let Some(t) = e.output_truncated {
        write_kv_raw(
            &mut out,
            "output_truncated",
            if t { "true" } else { "false" },
        );
    }
    out
}

fn encode_status_ok(s: &RepoStatus) -> String {
    let mut out = String::with_capacity(256);
    out.push_str("OK");
    write_kv_str(&mut out, "repo_name", &s.repo_name);
    write_kv_str(&mut out, "branch", &s.branch);
    write_kv_bool(&mut out, "detached", s.detached);
    if let Some(state) = s.state {
        write_kv_raw(&mut out, "state", op_state_token(state));
    }
    write_kv_u32(&mut out, "staged", s.staged);
    write_kv_u32(&mut out, "modified", s.modified);
    write_kv_u32(&mut out, "untracked", s.untracked);
    write_kv_u32(&mut out, "conflicted", s.conflicted);
    write_kv_u32(&mut out, "ahead", s.ahead);
    write_kv_u32(&mut out, "behind", s.behind);
    write_kv_u32(&mut out, "stashed", s.stashed);
    out
}

fn write_kv_str(out: &mut String, key: &str, val: &str) {
    out.push(' ');
    out.push_str(key);
    out.push('=');
    out.push_str(&percent_encode(val));
}

fn write_kv_bool(out: &mut String, key: &str, val: bool) {
    out.push(' ');
    out.push_str(key);
    out.push('=');
    out.push_str(if val { "true" } else { "false" });
}

fn write_kv_u32(out: &mut String, key: &str, val: u32) {
    use std::fmt::Write;
    let _ = write!(out, " {key}={val}");
}

/// Numeric kv writer for the lifecycle opcodes — accepts anything that
/// `Display`s as a sane integer literal (i32/i64/u64). Sibling to
/// [`write_kv_u32`] which predates the lifecycle work and is left
/// untouched to minimise churn in the status-encoding call sites.
fn write_kv_num<T: std::fmt::Display>(out: &mut String, key: &str, val: T) {
    use std::fmt::Write;
    let _ = write!(out, " {key}={val}");
}

fn write_kv_raw(out: &mut String, key: &str, val: &str) {
    out.push(' ');
    out.push_str(key);
    out.push('=');
    out.push_str(val);
}

const fn op_state_token(s: OpState) -> &'static str {
    match s {
        OpState::Rebasing => "rebasing",
        OpState::Merging => "merging",
        OpState::CherryPick => "cherry",
        OpState::Bisect => "bisect",
    }
}

fn parse_op_state(s: &str) -> Result<OpState, ProtoError> {
    match s {
        "rebasing" => Ok(OpState::Rebasing),
        "merging" => Ok(OpState::Merging),
        "cherry" => Ok(OpState::CherryPick),
        "bisect" => Ok(OpState::Bisect),
        _ => Err(ProtoError::Malformed("unknown state token")),
    }
}

// ── Decoding ────────────────────────────────────────────────────────────────

/// Parse one line (without trailing `\n`) as a request.
///
/// # Errors
///
/// Returns [`ProtoError`] for empty input, an unknown opcode, or a
/// well-formed opcode with a bad payload (e.g. `HELLO abc`).
pub fn decode_request(line: &str) -> Result<Request, ProtoError> {
    let line = strip_eol(line);
    if line.is_empty() {
        return Err(ProtoError::Empty);
    }
    let (op, rest) = split_opcode(line);
    match op {
        "HELLO" => {
            let v: u32 = rest
                .trim()
                .parse()
                .map_err(|_| ProtoError::Malformed("HELLO version not a u32"))?;
            Ok(Request::Hello(v))
        }
        "STATUS" => {
            let path_enc = rest.trim();
            if path_enc.is_empty() {
                return Err(ProtoError::Malformed("STATUS requires a path"));
            }
            let path = percent_decode(path_enc)?;
            Ok(Request::Status(PathBuf::from(path)))
        }
        "CMD_START" => decode_cmd_start(rest).map(Request::CmdStart),
        "CMD_END" => decode_cmd_end(rest).map(Request::CmdEnd),
        "SUBSCRIBE" => decode_subscribe(rest).map(Request::Subscribe),
        "QUIT" => {
            if !rest.trim().is_empty() {
                return Err(ProtoError::Malformed("QUIT takes no arguments"));
            }
            Ok(Request::Quit)
        }
        other => Err(ProtoError::UnknownOpcode(other.to_string())),
    }
}

fn decode_cmd_start(rest: &str) -> Result<CmdStartEvent, ProtoError> {
    let mut id: Option<String> = None;
    let mut session: Option<String> = None;
    let mut host: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut started_at: Option<i64> = None;
    let mut cmd: Option<String> = None;
    for tok in rest.split_ascii_whitespace() {
        let (k, v) = tok
            .split_once('=')
            .ok_or(ProtoError::Malformed("CMD_START kv missing '='"))?;
        match k {
            "id" => id = Some(percent_decode(v)?),
            "session" => session = Some(percent_decode(v)?),
            "host" => host = Some(percent_decode(v)?),
            "cwd" => cwd = Some(percent_decode(v)?),
            "started_at" => started_at = Some(parse_i64(v)?),
            "cmd" => cmd = Some(percent_decode(v)?),
            // Forward-compat: ignore unknown keys.
            _ => {}
        }
    }
    Ok(CmdStartEvent {
        id: id.ok_or(ProtoError::Malformed("CMD_START missing id"))?,
        session_id: session.ok_or(ProtoError::Malformed("CMD_START missing session"))?,
        hostname: host.ok_or(ProtoError::Malformed("CMD_START missing host"))?,
        cwd: PathBuf::from(cwd.ok_or(ProtoError::Malformed("CMD_START missing cwd"))?),
        cmd: cmd.ok_or(ProtoError::Malformed("CMD_START missing cmd"))?,
        started_at_ms: started_at.ok_or(ProtoError::Malformed("CMD_START missing started_at"))?,
    })
}

fn decode_subscribe(rest: &str) -> Result<SubscribeSpec, ProtoError> {
    let mut cwd: Option<String> = None;
    for tok in rest.split_ascii_whitespace() {
        let (k, v) = tok
            .split_once('=')
            .ok_or(ProtoError::Malformed("SUBSCRIBE kv missing '='"))?;
        // Forward-compat: ignore unknown keys so future filter axes
        // (topics, session) can land without breaking older daemons
        // that don't yet know them.
        if k == "cwd" {
            cwd = Some(percent_decode(v)?);
        }
    }
    Ok(SubscribeSpec {
        cwd: cwd.map(PathBuf::from),
    })
}

fn decode_cmd_end(rest: &str) -> Result<CmdEndEvent, ProtoError> {
    let mut id: Option<String> = None;
    let mut finished_at: Option<i64> = None;
    let mut duration_ms: Option<u64> = None;
    let mut exit: Option<i32> = None;
    let mut output_bytes: Option<i64> = None;
    let mut output_truncated: Option<bool> = None;
    for tok in rest.split_ascii_whitespace() {
        let (k, v) = tok
            .split_once('=')
            .ok_or(ProtoError::Malformed("CMD_END kv missing '='"))?;
        match k {
            "id" => id = Some(percent_decode(v)?),
            "finished_at" => finished_at = Some(parse_i64(v)?),
            "duration_ms" => duration_ms = Some(parse_u64(v)?),
            "exit" => exit = Some(parse_i32(v)?),
            "output_bytes" => output_bytes = Some(parse_i64(v)?),
            "output_truncated" => output_truncated = Some(parse_bool(v)?),
            _ => {}
        }
    }
    Ok(CmdEndEvent {
        id: id.ok_or(ProtoError::Malformed("CMD_END missing id"))?,
        finished_at_ms: finished_at.ok_or(ProtoError::Malformed("CMD_END missing finished_at"))?,
        duration_ms: duration_ms.ok_or(ProtoError::Malformed("CMD_END missing duration_ms"))?,
        exit_status: exit.ok_or(ProtoError::Malformed("CMD_END missing exit"))?,
        output_bytes,
        output_truncated,
    })
}

/// Parse one line (without trailing `\n`) as a response.
///
/// # Errors
///
/// Returns [`ProtoError`] for empty input, an unknown opcode, an `OK` payload
/// missing a required key, or any other malformed field (bad bool, unknown
/// state token, malformed kv pair, …).
pub fn decode_response(line: &str) -> Result<Response, ProtoError> {
    let line = strip_eol(line);
    if line.is_empty() {
        return Err(ProtoError::Empty);
    }
    let (op, rest) = split_opcode(line);
    match op {
        "HELLO" => {
            let v: u32 = rest
                .trim()
                .parse()
                .map_err(|_| ProtoError::Malformed("HELLO version not a u32"))?;
            Ok(Response::Hello(v))
        }
        "NONE" => {
            if !rest.trim().is_empty() {
                return Err(ProtoError::Malformed("NONE takes no arguments"));
            }
            Ok(Response::Status(None))
        }
        "ACK" => {
            if !rest.trim().is_empty() {
                return Err(ProtoError::Malformed("ACK takes no arguments"));
            }
            Ok(Response::Ack)
        }
        "EVENT" => decode_event(rest).map(Response::Event),
        "PING" => {
            let ts: i64 = rest
                .trim()
                .parse()
                .map_err(|_| ProtoError::Malformed("PING timestamp not an i64"))?;
            Ok(Response::Ping(ts))
        }
        "OK" => decode_status_ok(rest).map(|s| Response::Status(Some(s))),
        "ERR" => {
            let reason = percent_decode(rest.trim())?;
            Ok(Response::Err(reason))
        }
        other => Err(ProtoError::UnknownOpcode(other.to_string())),
    }
}

fn decode_event(rest: &str) -> Result<EventPayload, ProtoError> {
    let mut topic: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut id: Option<String> = None;
    for tok in rest.split_ascii_whitespace() {
        let (k, v) = tok
            .split_once('=')
            .ok_or(ProtoError::Malformed("EVENT kv missing '='"))?;
        match k {
            "type" => topic = Some(percent_decode(v)?),
            "cwd" => cwd = Some(percent_decode(v)?),
            "id" => id = Some(percent_decode(v)?),
            // Future event metadata (exit, duration, session, etc.) is
            // silently ignored so older subscribers can read newer
            // EVENT lines without erroring.
            _ => {}
        }
    }
    Ok(EventPayload {
        topic: topic.ok_or(ProtoError::Malformed("EVENT missing type"))?,
        cwd: cwd.map(PathBuf::from),
        id,
    })
}

fn decode_status_ok(rest: &str) -> Result<RepoStatus, ProtoError> {
    let mut repo_name: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut detached: Option<bool> = None;
    let mut state: Option<OpState> = None;
    let mut staged: Option<u32> = None;
    let mut modified: Option<u32> = None;
    let mut untracked: Option<u32> = None;
    let mut conflicted: Option<u32> = None;
    let mut ahead: Option<u32> = None;
    let mut behind: Option<u32> = None;
    let mut stashed: Option<u32> = None;

    for tok in rest.split_ascii_whitespace() {
        let (k, v) = tok
            .split_once('=')
            .ok_or(ProtoError::Malformed("OK kv missing '='"))?;
        match k {
            "repo_name" => repo_name = Some(percent_decode(v)?),
            "branch" => branch = Some(percent_decode(v)?),
            "detached" => detached = Some(parse_bool(v)?),
            "state" => state = Some(parse_op_state(v)?),
            "staged" => staged = Some(parse_u32(v)?),
            "modified" => modified = Some(parse_u32(v)?),
            "untracked" => untracked = Some(parse_u32(v)?),
            "conflicted" => conflicted = Some(parse_u32(v)?),
            "ahead" => ahead = Some(parse_u32(v)?),
            "behind" => behind = Some(parse_u32(v)?),
            "stashed" => stashed = Some(parse_u32(v)?),
            // Forward-compat: silently ignore unknown keys so phase-2+
            // servers can add fields without breaking older clients.
            _ => {}
        }
    }

    Ok(RepoStatus {
        repo_name: repo_name.ok_or(ProtoError::Malformed("OK missing repo_name"))?,
        branch: branch.ok_or(ProtoError::Malformed("OK missing branch"))?,
        detached: detached.ok_or(ProtoError::Malformed("OK missing detached"))?,
        state,
        staged: staged.ok_or(ProtoError::Malformed("OK missing staged"))?,
        modified: modified.ok_or(ProtoError::Malformed("OK missing modified"))?,
        untracked: untracked.ok_or(ProtoError::Malformed("OK missing untracked"))?,
        conflicted: conflicted.ok_or(ProtoError::Malformed("OK missing conflicted"))?,
        ahead: ahead.ok_or(ProtoError::Malformed("OK missing ahead"))?,
        behind: behind.ok_or(ProtoError::Malformed("OK missing behind"))?,
        stashed: stashed.ok_or(ProtoError::Malformed("OK missing stashed"))?,
    })
}

fn parse_bool(s: &str) -> Result<bool, ProtoError> {
    match s {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ProtoError::Malformed("bool must be true/false")),
    }
}

fn parse_u32(s: &str) -> Result<u32, ProtoError> {
    s.parse().map_err(|_| ProtoError::Malformed("expected u32"))
}

fn parse_i32(s: &str) -> Result<i32, ProtoError> {
    s.parse().map_err(|_| ProtoError::Malformed("expected i32"))
}

fn parse_i64(s: &str) -> Result<i64, ProtoError> {
    s.parse().map_err(|_| ProtoError::Malformed("expected i64"))
}

fn parse_u64(s: &str) -> Result<u64, ProtoError> {
    s.parse().map_err(|_| ProtoError::Malformed("expected u64"))
}

fn strip_eol(s: &str) -> &str {
    s.strip_suffix('\n')
        .unwrap_or(s)
        .strip_suffix('\r')
        .unwrap_or_else(|| s.strip_suffix('\n').unwrap_or(s))
}

fn split_opcode(line: &str) -> (&str, &str) {
    line.split_once(' ').unwrap_or((line, ""))
}

// ── Percent encoding ────────────────────────────────────────────────────────

/// Escape any byte that isn't printable-ASCII-except-`%` as `%XX`
/// (uppercase hex). Pass-through set is `[0x21, 0x7E] \ {%}` — letters,
/// digits, common punctuation. Everything else (control bytes including
/// space, DEL, and any byte ≥ 0x80) gets hex-escaped, so a UTF-8 string
/// round-trips byte-for-byte through [`percent_decode`] even when it
/// contains non-ASCII codepoints.
///
/// The wider escape set (vs the original `%25`/`%20`-only scheme) exists
/// to make the `cmd` field of `CMD_START` safe: a heredoc or copy-paste
/// command may contain tabs, newlines, or non-ASCII text that would
/// otherwise break the line-oriented kv parser or be reinterpreted
/// through Latin-1 at the decoder. Existing STATUS encoding output is
/// unchanged for any realistic input (branches/repo names are ASCII).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if (0x21..=0x7E).contains(&b) && b != b'%' {
            out.push(b as char);
        } else {
            use std::fmt::Write;
            // Uppercase hex per RFC 3986 §2.1 (and what we tested in the
            // status path snapshot, so existing snapshots still match).
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Reverse of [`percent_encode`]. Accepts any well-formed `%XX` (broader
/// than what the encoder emits, so future encoder changes stay
/// wire-compatible) and rebuilds the original byte sequence. The result
/// must be valid UTF-8; bytes that escape to invalid UTF-8 are rejected
/// as `Malformed` rather than silently lossily reinterpreted.
fn percent_decode(s: &str) -> Result<String, ProtoError> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(ProtoError::Malformed("truncated percent escape"));
            }
            let hi = hex_nibble(bytes[i + 1])?;
            let lo = hex_nibble(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ProtoError::Malformed("decoded bytes are not valid UTF-8"))
}

fn hex_nibble(b: u8) -> Result<u8, ProtoError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(ProtoError::Malformed("invalid hex in percent escape")),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segments::git::{OpState, RepoStatus};

    fn fixture_status() -> RepoStatus {
        RepoStatus {
            repo_name: "chevron".to_string(),
            branch: "master".to_string(),
            detached: false,
            state: None,
            staged: 0,
            modified: 2,
            untracked: 1,
            conflicted: 0,
            ahead: 3,
            behind: 0,
            stashed: 1,
        }
    }

    // ── Percent encoding ────────────────────────────────────────────────

    #[test]
    fn percent_encode_passes_through_normal() {
        assert_eq!(percent_encode("master"), "master");
        assert_eq!(percent_encode("feature/foo_bar.baz"), "feature/foo_bar.baz");
    }

    #[test]
    fn percent_encode_handles_space_and_percent() {
        assert_eq!(percent_encode("my project"), "my%20project");
        assert_eq!(percent_encode("100%"), "100%25");
        assert_eq!(percent_encode("a % b"), "a%20%25%20b");
    }

    #[test]
    fn percent_decode_round_trips_normal() {
        for s in ["", "master", "feature/x", "a-b_c.d", "/usr/local/bin"] {
            assert_eq!(percent_decode(&percent_encode(s)).unwrap(), s);
        }
    }

    #[test]
    fn percent_decode_round_trips_specials() {
        for s in ["my project", "100%", "a % b", "  ", "%%%"] {
            assert_eq!(percent_decode(&percent_encode(s)).unwrap(), s);
        }
    }

    #[test]
    fn percent_decode_rejects_truncated() {
        assert!(matches!(
            percent_decode("foo%2"),
            Err(ProtoError::Malformed(_))
        ));
        assert!(matches!(
            percent_decode("foo%"),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn percent_decode_accepts_any_hex_pair() {
        // Decoder is intentionally more permissive than the encoder — it
        // accepts any `%XX` so future encoder changes that escape more
        // bytes (e.g. `%7F` for DEL) stay wire-compatible with older
        // decoders that don't escape those bytes.
        assert_eq!(percent_decode("%41").unwrap(), "A");
        assert_eq!(percent_decode("%7F").unwrap(), "\x7f");
        assert_eq!(percent_decode("%0a").unwrap(), "\n");
    }

    #[test]
    fn percent_decode_rejects_non_hex_escape() {
        // The two bytes following `%` must be hex digits — `%XY` is not.
        assert!(matches!(
            percent_decode("%XY"),
            Err(ProtoError::Malformed(_))
        ));
        assert!(matches!(
            percent_decode("%2G"),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn percent_encode_escapes_control_bytes() {
        // Control bytes — tab, newline, CR — are escaped so they don't
        // break the line-oriented kv parser when used in `cmd` values.
        assert_eq!(percent_encode("\n"), "%0A");
        assert_eq!(percent_encode("\t"), "%09");
        assert_eq!(percent_encode("\r"), "%0D");
        // Printable ASCII (above space) passes through untouched.
        assert_eq!(percent_encode("git status"), "git%20status");
        assert_eq!(percent_encode("ls -la"), "ls%20-la");
    }

    #[test]
    fn percent_round_trips_command_with_specials() {
        // A cmd line with shell metacharacters, percent, tab, newline —
        // must survive encode/decode untouched.
        let cmd = "echo 'hello\tworld'\n# 50%";
        assert_eq!(percent_decode(&percent_encode(cmd)).unwrap(), cmd);
    }

    #[test]
    fn percent_round_trips_utf8() {
        // Multi-byte UTF-8 strings must round-trip byte-for-byte.
        // The original implementation pushed each byte as a char, which
        // reinterpreted high-bit bytes as Latin-1 and corrupted them when
        // re-encoded as UTF-8 by the String builder.
        for s in ["café", "プロジェクト", "naïve résumé", "✨🦀"] {
            assert_eq!(percent_decode(&percent_encode(s)).unwrap(), s);
        }
    }

    // ── Request encoding ────────────────────────────────────────────────

    #[test]
    fn encode_decode_hello_request() {
        let req = Request::Hello(PROTO_VERSION);
        let line = encode_request(&req);
        assert_eq!(line, "HELLO 1");
        assert_eq!(decode_request(&line).unwrap(), req);
    }

    #[test]
    fn encode_decode_status_request_simple() {
        let req = Request::Status(PathBuf::from("/Users/mim/src/chevron"));
        let line = encode_request(&req);
        assert_eq!(line, "STATUS /Users/mim/src/chevron");
        assert_eq!(decode_request(&line).unwrap(), req);
    }

    #[test]
    fn encode_decode_status_request_with_space() {
        let req = Request::Status(PathBuf::from("/Users/mim/My Project"));
        let line = encode_request(&req);
        assert_eq!(line, "STATUS /Users/mim/My%20Project");
        assert_eq!(decode_request(&line).unwrap(), req);
    }

    #[test]
    fn encode_decode_quit_request() {
        assert_eq!(encode_request(&Request::Quit), "QUIT");
        assert_eq!(decode_request("QUIT").unwrap(), Request::Quit);
    }

    #[test]
    fn decode_request_strips_trailing_newline() {
        assert_eq!(
            decode_request("HELLO 1\n").unwrap(),
            Request::Hello(PROTO_VERSION)
        );
        assert_eq!(
            decode_request("HELLO 1\r\n").unwrap(),
            Request::Hello(PROTO_VERSION)
        );
    }

    #[test]
    fn decode_request_rejects_unknown_opcode() {
        assert!(matches!(
            decode_request("FOO bar"),
            Err(ProtoError::UnknownOpcode(_))
        ));
    }

    #[test]
    fn decode_request_rejects_empty() {
        assert_eq!(decode_request(""), Err(ProtoError::Empty));
        assert_eq!(decode_request("\n"), Err(ProtoError::Empty));
    }

    #[test]
    fn decode_request_rejects_bad_hello_version() {
        assert!(matches!(
            decode_request("HELLO abc"),
            Err(ProtoError::Malformed(_))
        ));
        assert!(matches!(
            decode_request("HELLO -1"),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn decode_request_rejects_status_without_path() {
        assert!(matches!(
            decode_request("STATUS"),
            Err(ProtoError::Malformed(_))
        ));
        assert!(matches!(
            decode_request("STATUS   "),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn decode_request_rejects_quit_with_args() {
        assert!(matches!(
            decode_request("QUIT now"),
            Err(ProtoError::Malformed(_))
        ));
    }

    // ── Lifecycle event requests ────────────────────────────────────────

    fn fixture_cmd_start() -> CmdStartEvent {
        CmdStartEvent {
            id: "01HW4ZAB12CDEFGHJKMNPQRSTV".to_string(),
            session_id: "01HW4ZAA00000000000000SESS".to_string(),
            hostname: "matt-mbp".to_string(),
            cwd: PathBuf::from("/Users/mim/src/chevron"),
            cmd: "cargo test".to_string(),
            started_at_ms: 1_716_350_000_123,
        }
    }

    fn fixture_cmd_end() -> CmdEndEvent {
        CmdEndEvent {
            id: "01HW4ZAB12CDEFGHJKMNPQRSTV".to_string(),
            finished_at_ms: 1_716_350_001_456,
            duration_ms: 1333,
            exit_status: 0,
            output_bytes: None,
            output_truncated: None,
        }
    }

    #[test]
    fn encode_decode_cmd_start_simple() {
        let req = Request::CmdStart(fixture_cmd_start());
        let line = encode_request(&req);
        // Snapshot the exact wire format. Key order is fixed: id,
        // session, host, cwd, started_at, cmd (cmd last so wide values
        // sit at the right edge of socat dumps).
        assert_eq!(
            line,
            "CMD_START id=01HW4ZAB12CDEFGHJKMNPQRSTV session=01HW4ZAA00000000000000SESS \
             host=matt-mbp cwd=/Users/mim/src/chevron started_at=1716350000123 cmd=cargo%20test"
        );
        assert_eq!(decode_request(&line).unwrap(), req);
    }

    #[test]
    fn encode_decode_cmd_start_with_specials_in_cmd() {
        // Tabs, newlines, and non-ASCII bytes in the cmd must survive
        // the kv parser and round-trip cleanly.
        let req = Request::CmdStart(CmdStartEvent {
            cmd: "echo 'naïve résumé'\n# 50% off".to_string(),
            ..fixture_cmd_start()
        });
        let line = encode_request(&req);
        assert!(
            !line.contains('\n')
                && !line.contains('\t')
                && line.split_ascii_whitespace().count() == 7,
            "encoded line must contain exactly the opcode + 6 kv tokens, no raw whitespace in cmd: {line}"
        );
        assert_eq!(decode_request(&line).unwrap(), req);
    }

    #[test]
    fn encode_decode_cmd_start_with_spaces_in_cwd() {
        let req = Request::CmdStart(CmdStartEvent {
            cwd: PathBuf::from("/Users/mim/My Project"),
            ..fixture_cmd_start()
        });
        let line = encode_request(&req);
        assert!(
            line.contains("cwd=/Users/mim/My%20Project"),
            "cwd with space should be percent-encoded: {line}"
        );
        assert_eq!(decode_request(&line).unwrap(), req);
    }

    #[test]
    fn encode_decode_cmd_end_simple() {
        let req = Request::CmdEnd(fixture_cmd_end());
        let line = encode_request(&req);
        assert_eq!(
            line,
            "CMD_END id=01HW4ZAB12CDEFGHJKMNPQRSTV finished_at=1716350001456 \
             duration_ms=1333 exit=0"
        );
        assert_eq!(decode_request(&line).unwrap(), req);
    }

    #[test]
    fn encode_decode_cmd_end_with_capture_metadata() {
        // Phase 4 (chevron-1yn.4): chcap-wrapped commands carry
        // output_bytes + output_truncated. Regular cmd-end leaves them
        // None, which serialises to "omit the keys".
        let req = Request::CmdEnd(CmdEndEvent {
            output_bytes: Some(42_000),
            output_truncated: Some(true),
            ..fixture_cmd_end()
        });
        let line = encode_request(&req);
        assert!(line.contains("output_bytes=42000"), "got: {line}");
        assert!(line.contains("output_truncated=true"), "got: {line}");
        assert_eq!(decode_request(&line).unwrap(), req);
    }

    #[test]
    fn encode_cmd_end_omits_capture_keys_when_none() {
        // Backward-compat: pre-Phase-4 daemons don't know about
        // output_bytes / output_truncated. The encoder must omit the
        // keys entirely when they're None so a v1 daemon receiving a
        // v2-encoded CMD_END from a v2 chevron event still parses
        // correctly (the unknown-key forward-compat path).
        let req = Request::CmdEnd(fixture_cmd_end());
        let line = encode_request(&req);
        assert!(!line.contains("output_bytes"), "got: {line}");
        assert!(!line.contains("output_truncated"), "got: {line}");
    }

    #[test]
    fn encode_decode_cmd_end_negative_exit() {
        // Shells map signal-killed processes to exit codes 128 + signum
        // (positive), but some history layers report negative for
        // signal kills. We use i32 to accept either convention.
        let req = Request::CmdEnd(CmdEndEvent {
            exit_status: -15,
            ..fixture_cmd_end()
        });
        let line = encode_request(&req);
        assert!(line.contains("exit=-15"), "expected negative exit: {line}");
        assert_eq!(decode_request(&line).unwrap(), req);
    }

    #[test]
    fn decode_cmd_start_ignores_unknown_keys() {
        // Forward-compat: a phase-2 or phase-3 server might add fields.
        // Older parsers must keep working.
        let line = "CMD_START id=A session=B host=h cwd=/ started_at=0 \
                    cmd=ls future_field=42 user=mim";
        let Request::CmdStart(e) = decode_request(line).unwrap() else {
            panic!("expected CmdStart");
        };
        assert_eq!(e.id, "A");
        assert_eq!(e.cmd, "ls");
    }

    #[test]
    fn decode_cmd_start_tolerates_reordering() {
        let line = "CMD_START cmd=ls started_at=42 cwd=/ host=h session=B id=A";
        let Request::CmdStart(e) = decode_request(line).unwrap() else {
            panic!("expected CmdStart");
        };
        assert_eq!(e.id, "A");
        assert_eq!(e.session_id, "B");
        assert_eq!(e.cmd, "ls");
        assert_eq!(e.started_at_ms, 42);
    }

    #[test]
    fn decode_cmd_start_rejects_missing_required_field() {
        // Drop `id`.
        let line = "CMD_START session=B host=h cwd=/ started_at=0 cmd=ls";
        assert!(matches!(
            decode_request(line),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn decode_cmd_end_rejects_bad_numeric() {
        let line = "CMD_END id=A finished_at=notanumber duration_ms=1 exit=0";
        assert!(matches!(
            decode_request(line),
            Err(ProtoError::Malformed(_))
        ));
    }

    // ── Response encoding ───────────────────────────────────────────────

    #[test]
    fn encode_decode_hello_response() {
        let resp = Response::Hello(PROTO_VERSION);
        let line = encode_response(&resp);
        assert_eq!(line, "HELLO 1");
        assert_eq!(decode_response(&line).unwrap(), resp);
    }

    #[test]
    fn encode_decode_none_response() {
        let line = encode_response(&Response::Status(None));
        assert_eq!(line, "NONE");
        assert_eq!(decode_response(&line).unwrap(), Response::Status(None));
    }

    #[test]
    fn encode_decode_ok_clean() {
        let status = fixture_status();
        let resp = Response::Status(Some(status.clone()));
        let line = encode_response(&resp);
        // Snapshot of the exact wire format for the fixture — locks the field
        // order so future encoder changes that flip ordering get caught here.
        assert_eq!(
            line,
            "OK repo_name=chevron branch=master detached=false staged=0 modified=2 untracked=1 conflicted=0 ahead=3 behind=0 stashed=1"
        );
        let Response::Status(Some(parsed)) = decode_response(&line).unwrap() else {
            panic!("expected Status(Some)");
        };
        assert_eq!(parsed, status);
    }

    #[test]
    fn encode_decode_ok_all_states() {
        for state in [
            OpState::Rebasing,
            OpState::Merging,
            OpState::CherryPick,
            OpState::Bisect,
        ] {
            let status = RepoStatus {
                state: Some(state),
                ..fixture_status()
            };
            let line = encode_response(&Response::Status(Some(status.clone())));
            let Response::Status(Some(parsed)) = decode_response(&line).unwrap() else {
                panic!("expected Status(Some)");
            };
            assert_eq!(parsed, status, "round-trip failed for state {state:?}");
        }
    }

    #[test]
    fn encode_decode_ok_with_spaces_in_strings() {
        let status = RepoStatus {
            repo_name: "My Project".to_string(),
            branch: "feature/needs%escape".to_string(),
            detached: true,
            ..fixture_status()
        };
        let line = encode_response(&Response::Status(Some(status.clone())));
        assert!(
            line.contains("repo_name=My%20Project"),
            "expected encoded space: {line}"
        );
        assert!(
            line.contains("branch=feature/needs%25escape"),
            "expected encoded percent: {line}"
        );
        let Response::Status(Some(parsed)) = decode_response(&line).unwrap() else {
            panic!("expected Status(Some)");
        };
        assert_eq!(parsed, status);
    }

    #[test]
    fn encode_decode_err_response() {
        let resp = Response::Err("repo not found".to_string());
        let line = encode_response(&resp);
        assert_eq!(line, "ERR repo%20not%20found");
        assert_eq!(decode_response(&line).unwrap(), resp);
    }

    #[test]
    fn decode_response_ignores_unknown_kv_keys() {
        // Forward-compat: phase-2 might add fields. Old clients keep working.
        let line = "OK repo_name=x branch=y detached=false staged=0 modified=0 \
            untracked=0 conflicted=0 ahead=0 behind=0 stashed=0 \
            future_field=42 another=hello";
        let resp = decode_response(line).unwrap();
        let Response::Status(Some(s)) = resp else {
            panic!("expected Status(Some)");
        };
        assert_eq!(s.repo_name, "x");
        assert_eq!(s.branch, "y");
    }

    #[test]
    fn decode_response_tolerates_field_reordering() {
        let line = "OK stashed=1 behind=0 ahead=3 conflicted=0 untracked=1 modified=2 \
            staged=0 detached=false branch=master repo_name=chevron";
        let Response::Status(Some(parsed)) = decode_response(line).unwrap() else {
            panic!("expected Status(Some)");
        };
        assert_eq!(parsed, fixture_status());
    }

    #[test]
    fn decode_response_rejects_missing_required_field() {
        // Drop `branch`.
        let line = "OK repo_name=x detached=false staged=0 modified=0 \
            untracked=0 conflicted=0 ahead=0 behind=0 stashed=0";
        assert!(matches!(
            decode_response(line),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn decode_response_rejects_malformed_kv() {
        let line = "OK repo_name x branch=y";
        assert!(matches!(
            decode_response(line),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn decode_response_rejects_unknown_state_token() {
        let line = "OK repo_name=x branch=y detached=false state=mystery staged=0 \
            modified=0 untracked=0 conflicted=0 ahead=0 behind=0 stashed=0";
        assert!(matches!(
            decode_response(line),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn decode_response_rejects_bad_bool() {
        let line = "OK repo_name=x branch=y detached=maybe staged=0 \
            modified=0 untracked=0 conflicted=0 ahead=0 behind=0 stashed=0";
        assert!(matches!(
            decode_response(line),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn decode_none_rejects_args() {
        assert!(matches!(
            decode_response("NONE extra"),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn encode_decode_ack_response() {
        let line = encode_response(&Response::Ack);
        assert_eq!(line, "ACK");
        assert_eq!(decode_response(&line).unwrap(), Response::Ack);
    }

    #[test]
    fn decode_ack_rejects_args() {
        assert!(matches!(
            decode_response("ACK extra"),
            Err(ProtoError::Malformed(_))
        ));
    }

    // ── Phase 3 (chevron-1yn.3): SUBSCRIBE / EVENT / PING ───────────────

    #[test]
    fn encode_decode_subscribe_no_filter() {
        let req = Request::Subscribe(SubscribeSpec { cwd: None });
        let line = encode_request(&req);
        assert_eq!(line, "SUBSCRIBE");
        assert_eq!(decode_request(&line).unwrap(), req);
    }

    #[test]
    fn encode_decode_subscribe_with_cwd() {
        let req = Request::Subscribe(SubscribeSpec {
            cwd: Some(PathBuf::from("/Users/mim/src/chevron")),
        });
        let line = encode_request(&req);
        assert_eq!(line, "SUBSCRIBE cwd=/Users/mim/src/chevron");
        assert_eq!(decode_request(&line).unwrap(), req);
    }

    #[test]
    fn encode_decode_subscribe_with_space_in_cwd() {
        let req = Request::Subscribe(SubscribeSpec {
            cwd: Some(PathBuf::from("/Users/mim/My Project")),
        });
        let line = encode_request(&req);
        assert!(line.contains("cwd=/Users/mim/My%20Project"));
        assert_eq!(decode_request(&line).unwrap(), req);
    }

    #[test]
    fn decode_subscribe_ignores_unknown_keys() {
        // Forward-compat: future filter axes (topics, session) drop
        // through to None without erroring on this older daemon.
        let req = decode_request("SUBSCRIBE cwd=/x topics=git,cmd session=01HW").unwrap();
        let Request::Subscribe(spec) = req else {
            panic!("expected Subscribe");
        };
        assert_eq!(spec.cwd, Some(PathBuf::from("/x")));
    }

    #[test]
    fn encode_decode_event_git_topic() {
        let resp = Response::Event(EventPayload {
            topic: "git".to_string(),
            cwd: Some(PathBuf::from("/repo")),
            id: None,
        });
        let line = encode_response(&resp);
        assert_eq!(line, "EVENT type=git cwd=/repo");
        assert_eq!(decode_response(&line).unwrap(), resp);
    }

    #[test]
    fn encode_decode_event_cmd_topic() {
        let resp = Response::Event(EventPayload {
            topic: "cmd".to_string(),
            cwd: Some(PathBuf::from("/repo")),
            id: Some("01HW4ZAB12CDEFGHJKMNPQRSTV".to_string()),
        });
        let line = encode_response(&resp);
        assert_eq!(
            line,
            "EVENT type=cmd cwd=/repo id=01HW4ZAB12CDEFGHJKMNPQRSTV"
        );
        assert_eq!(decode_response(&line).unwrap(), resp);
    }

    #[test]
    fn decode_event_rejects_missing_type() {
        assert!(matches!(
            decode_response("EVENT cwd=/x"),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn decode_event_ignores_unknown_keys() {
        // Future event metadata (exit, duration, session, …) must be
        // silently ignored so older subscribers can read newer events.
        let resp = decode_response("EVENT type=cmd id=x exit=0 duration_ms=42").unwrap();
        let Response::Event(e) = resp else {
            panic!("expected Event");
        };
        assert_eq!(e.topic, "cmd");
        assert_eq!(e.id.as_deref(), Some("x"));
    }

    #[test]
    fn encode_decode_ping_response() {
        let resp = Response::Ping(1_716_350_000_000);
        let line = encode_response(&resp);
        assert_eq!(line, "PING 1716350000000");
        assert_eq!(decode_response(&line).unwrap(), resp);
    }

    #[test]
    fn decode_ping_rejects_non_numeric() {
        assert!(matches!(
            decode_response("PING notanumber"),
            Err(ProtoError::Malformed(_))
        ));
    }
}
