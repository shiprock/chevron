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
//!   QUIT
//!
//! Responses (server → client):
//!   HELLO <version>
//!   OK key=value key=value ...
//!   NONE
//!   ERR <reason>
//! ```
//!
//! ## Encoding
//!
//! Numeric fields (`staged`, `modified`, …) and bools (`detached`) are written
//! plain. The optional `state` field is `rebasing|merging|cherry|bisect` or
//! omitted entirely. String fields (`repo_name`, `branch`) and the `STATUS`
//! path arg are **percent-encoded** for the two reserved bytes:
//!
//! - `%` (0x25) → `%25`
//! - ` ` (0x20) → `%20`
//!
//! That's the entire escaping rule. Branch refs already forbid spaces per
//! `git check-ref-format`; repo directory names commonly contain them.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Hello(u32),
    Status(PathBuf),
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Hello(u32),
    /// `Some` → `OK …`; `None` → `NONE` (no repo discovered at the given path).
    Status(Option<RepoStatus>),
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
        Request::Quit => "QUIT".to_string(),
    }
}

#[must_use]
pub fn encode_response(resp: &Response) -> String {
    match resp {
        Response::Hello(v) => format!("HELLO {v}"),
        Response::Status(None) => "NONE".to_string(),
        Response::Status(Some(s)) => encode_status_ok(s),
        Response::Err(reason) => format!("ERR {}", percent_encode(reason)),
    }
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
        "QUIT" => {
            if !rest.trim().is_empty() {
                return Err(ProtoError::Malformed("QUIT takes no arguments"));
            }
            Ok(Request::Quit)
        }
        other => Err(ProtoError::UnknownOpcode(other.to_string())),
    }
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
        "OK" => decode_status_ok(rest).map(|s| Response::Status(Some(s))),
        "ERR" => {
            let reason = percent_decode(rest.trim())?;
            Ok(Response::Err(reason))
        }
        other => Err(ProtoError::UnknownOpcode(other.to_string())),
    }
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

/// Encode `%` → `%25` and ` ` → `%20`. Nothing else needs escaping for the
/// line-oriented kv format: the parser splits on ASCII whitespace and `=`, and
/// we control the strings (no newlines reach this function — branch refs and
/// directory names can't contain them).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'%' => out.push_str("%25"),
            b' ' => out.push_str("%20"),
            _ => out.push(b as char),
        }
    }
    out
}

/// Reverse of `percent_encode`. Accepts only `%25` and `%20`; any other
/// percent-escape is rejected with `Malformed`. We deliberately don't
/// implement full RFC 3986 — the protocol's alphabet is intentionally small.
fn percent_decode(s: &str) -> Result<String, ProtoError> {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(ProtoError::Malformed("truncated percent escape"));
            }
            let hi = bytes[i + 1];
            let lo = bytes[i + 2];
            match (hi, lo) {
                (b'2', b'5') => out.push('%'),
                (b'2', b'0') => out.push(' '),
                _ => return Err(ProtoError::Malformed("unsupported percent escape")),
            }
            i += 3;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    Ok(out)
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
    fn percent_decode_rejects_unsupported_escape() {
        // RFC 3986 normally allows %41 → 'A', but our alphabet is tighter.
        assert!(matches!(
            percent_decode("%41"),
            Err(ProtoError::Malformed(_))
        ));
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
}
