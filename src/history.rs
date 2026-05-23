//! `chevron history` — query CLI for the chevrond commands log
//! (chevron-1yn.2).
//!
//! Reads directly from the `SQLite` DB written by chevrond's state
//! actor. No daemon contact: under WAL the daemon's writer and our
//! read-only opener don't block each other, and skipping IPC lets
//! history queries run even when the daemon is down.
//!
//! ## Surface
//!
//! ```text
//! chevron history [FLAGS] [PATTERN]
//!
//! Filters (positive):
//!   --cwd PATH              cwd exact match
//!   --here                  shortcut for --cwd "$(pwd)"
//!   --session ID            session-id match
//!   --host NAME             hostname match
//!   --exit N                exit_status == N
//!   --failed                exit_status > 0 (NULL exits excluded)
//!   --success               exit_status == 0
//!   --since DURATION        within the last DURATION
//!   --until DURATION        older than DURATION
//!   --since-ms MS           unix-epoch-ms lower bound (escape hatch)
//!   --until-ms MS           unix-epoch-ms upper bound (escape hatch)
//!   --workspace             current git work-tree (path prefix)
//!   --grep PATTERN, -g      substring match in cmd
//!   PATTERN                 positional shortcut for --grep
//!
//! Filters (negative):
//!   --exclude-cwd PATH
//!   --exclude-host NAME
//!   --exclude-exit N
//!   --exclude-session ID
//!
//! Preset combos (atuin-style):
//!   --filter-mode MODE      global | host | directory | workspace
//!
//! Display:
//!   --limit N, -n N         cap rows (default 25, max 10000)
//!   --reverse, -r           flip default oldest-at-top order
//!   --unique, -u            DISTINCT on cmd column
//!   --format FMT, -f FMT    human | json | cmds | ids | "<template>"
//!   -h, --help              this help
//! ```
//!
//! Default display is oldest-at-top so the bottom of the output is
//! the most recent row — matches `fc -l` / scrollback reading
//! direction. `--reverse` flips to newest-first.
//!
//! Templates: any non-keyword `--format` value is treated as a
//! template with `{id}`, `{cwd}`, `{cmd}`, `{exit}`, `{duration}`,
//! `{started_at}`, `{session}`, `{host}` substitutions plus `{n}` for
//! the row counter. Atuin compat.

use std::io::{IsTerminal, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};

use crate::daemon::paths;

/// Hard ceiling on `--limit` so a typo'd `--limit 100000000` doesn't
/// allocate a huge result vector. 10k is well past any human-readable
/// view; pipe to a downstream tool if you genuinely want more.
const MAX_LIMIT: usize = 10_000;
const DEFAULT_LIMIT: usize = 25;

/// Schema version this CLI understands. Bumping the daemon's
/// `schema_version` requires a coordinated CLI update; older history
/// builds error out rather than silently misinterpreting columns.
const SUPPORTED_SCHEMA: &str = "1";

#[derive(Debug, Clone, Default)]
enum Format {
    #[default]
    Human,
    Json,
    Cmds,
    Ids,
    Template(String),
}

#[derive(Debug, Default)]
// CLI filter struct with one bool per opt-in flag. The lint complaining
// about "more than 3 bools" doesn't fit here — these aren't a sloppy
// way to encode an enum, they're independent toggles whose combinations
// are validated post-parse.
#[allow(clippy::struct_excessive_bools)]
struct Filters {
    cwd: Option<String>,
    session: Option<String>,
    host: Option<String>,
    exit_eq: Option<i32>,
    failed_only: bool,
    success_only: bool,
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    workspace_prefix: Option<String>,
    grep: Option<String>,
    exclude_cwd: Option<String>,
    exclude_host: Option<String>,
    exclude_exit: Option<i32>,
    exclude_session: Option<String>,
    limit: usize,
    reverse: bool,
    unique: bool,
    format: Format,
}

/// CLI entry point. Returns the desired process exit code.
#[must_use]
#[allow(clippy::too_many_lines)] // arg parser; one long match is clearer than fragmenting
pub fn run(args: &[String]) -> i32 {
    let filters = match parse_args(args) {
        Ok(f) => f,
        Err(ParseOutcome::Help) => {
            print!("{}", help_text());
            return 0;
        }
        Err(ParseOutcome::Err(msg)) => {
            eprintln!("chevron history: {msg}");
            eprintln!();
            eprint!("{}", help_text());
            return 2;
        }
    };

    let db_path = paths::socket_dir().join("commands.db");
    if !db_path.exists() {
        eprintln!(
            "chevron history: no commands.db at {} — chevrond hasn't recorded anything yet",
            db_path.display()
        );
        return 0;
    }

    let conn = match Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("chevron history: opening {}: {e}", db_path.display());
            return 2;
        }
    };

    if let Err(msg) = check_schema(&conn) {
        eprintln!("chevron history: {msg}");
        return 2;
    }

    let (sql, params) = build_query(&filters);
    let now_ms = now_unix_ms();
    let use_color = stdout_is_color_tty();
    let mut stdout = std::io::stdout().lock();

    let rows = match execute(&conn, &sql, &params) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("chevron history: query: {e}");
            return 2;
        }
    };

    // SQL returns newest-first (ORDER BY started_at DESC). For human
    // display we want oldest-at-top by default so the last printed line
    // is the most recent — reverse before formatting unless --reverse
    // re-flips it back. JSON/cmds/ids/template stay in the natural SQL
    // order (newest-first) because they're typically piped to other
    // tools that expect time-descending.
    let ordered: Vec<&Row> = match filters.format {
        Format::Human if !filters.reverse => rows.iter().rev().collect(),
        Format::Human => rows.iter().collect(),
        _ if filters.reverse => rows.iter().rev().collect(),
        _ => rows.iter().collect(),
    };

    for (idx, row) in ordered.iter().enumerate() {
        let line = format_row(row, idx, &filters.format, use_color, now_ms);
        if writeln!(stdout, "{line}").is_err() {
            // EPIPE — downstream closed (e.g. `| head`); exit cleanly.
            return 0;
        }
    }
    0
}

// ── arg parsing ─────────────────────────────────────────────────────────────

#[derive(Debug)]
enum ParseOutcome {
    Help,
    Err(String),
}

#[allow(clippy::too_many_lines)]
fn parse_args(args: &[String]) -> Result<Filters, ParseOutcome> {
    let mut f = Filters {
        limit: DEFAULT_LIMIT,
        ..Default::default()
    };

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let take_val = |i: usize, name: &str| -> Result<String, ParseOutcome> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| ParseOutcome::Err(format!("{name} requires a value")))
        };

        match arg.as_str() {
            "-h" | "--help" => return Err(ParseOutcome::Help),
            "--cwd" => {
                f.cwd = Some(take_val(i, "--cwd")?);
                i += 1;
            }
            "--here" => {
                f.cwd = Some(current_dir_string()?);
            }
            "--session" => {
                f.session = Some(take_val(i, "--session")?);
                i += 1;
            }
            "--host" => {
                f.host = Some(take_val(i, "--host")?);
                i += 1;
            }
            "--exit" => {
                f.exit_eq = Some(parse_i32(&take_val(i, "--exit")?)?);
                i += 1;
            }
            "--failed" => f.failed_only = true,
            "--success" => f.success_only = true,
            "--since" => {
                f.since_ms = Some(parse_since(&take_val(i, "--since")?)?);
                i += 1;
            }
            "--until" => {
                f.until_ms = Some(parse_since(&take_val(i, "--until")?)?);
                i += 1;
            }
            "--since-ms" => {
                f.since_ms = Some(parse_i64(&take_val(i, "--since-ms")?)?);
                i += 1;
            }
            "--until-ms" => {
                f.until_ms = Some(parse_i64(&take_val(i, "--until-ms")?)?);
                i += 1;
            }
            "--workspace" => {
                f.workspace_prefix = Some(workspace_root_string()?);
            }
            "--grep" | "-g" => {
                f.grep = Some(take_val(i, "--grep")?);
                i += 1;
            }
            "--exclude-cwd" => {
                f.exclude_cwd = Some(take_val(i, "--exclude-cwd")?);
                i += 1;
            }
            "--exclude-host" => {
                f.exclude_host = Some(take_val(i, "--exclude-host")?);
                i += 1;
            }
            "--exclude-exit" => {
                f.exclude_exit = Some(parse_i32(&take_val(i, "--exclude-exit")?)?);
                i += 1;
            }
            "--exclude-session" => {
                f.exclude_session = Some(take_val(i, "--exclude-session")?);
                i += 1;
            }
            "--filter-mode" => {
                apply_filter_mode(&mut f, &take_val(i, "--filter-mode")?)?;
                i += 1;
            }
            "--limit" | "-n" => {
                let n = parse_usize(&take_val(i, "--limit")?)?;
                if n > MAX_LIMIT {
                    return Err(ParseOutcome::Err(format!(
                        "--limit {n} exceeds maximum {MAX_LIMIT}"
                    )));
                }
                f.limit = n;
                i += 1;
            }
            "--reverse" | "-r" => f.reverse = true,
            "--unique" | "-u" => f.unique = true,
            "--format" | "-f" => {
                f.format = parse_format(&take_val(i, "--format")?);
                i += 1;
            }
            other if other.starts_with('-') => {
                return Err(ParseOutcome::Err(format!("unknown flag: {other}")));
            }
            other => {
                // Positional → --grep (atuin-style). Only allowed once.
                if f.grep.is_some() {
                    return Err(ParseOutcome::Err(format!(
                        "unexpected positional argument: {other}"
                    )));
                }
                f.grep = Some(other.to_string());
            }
        }
        i += 1;
    }

    if f.failed_only && f.success_only {
        return Err(ParseOutcome::Err(
            "cannot combine --failed with --success".to_string(),
        ));
    }
    if f.failed_only && f.exit_eq == Some(0) {
        return Err(ParseOutcome::Err(
            "cannot combine --failed with --exit 0".to_string(),
        ));
    }
    if f.success_only && matches!(f.exit_eq, Some(n) if n != 0) {
        return Err(ParseOutcome::Err(
            "cannot combine --success with --exit non-zero".to_string(),
        ));
    }
    Ok(f)
}

fn parse_format(s: &str) -> Format {
    match s {
        "human" => Format::Human,
        "json" => Format::Json,
        "cmds" => Format::Cmds,
        "ids" => Format::Ids,
        // Anything else is a template; presence of `{` is a hint but
        // we accept plain strings too (silly but consistent).
        other => Format::Template(other.to_string()),
    }
}

fn apply_filter_mode(f: &mut Filters, mode: &str) -> Result<(), ParseOutcome> {
    match mode {
        "global" => {}
        "host" => f.host = Some(hostname_or_unknown()),
        "directory" => f.cwd = Some(current_dir_string()?),
        "workspace" => f.workspace_prefix = Some(workspace_root_string()?),
        // We don't have a CHEVRON_SESSION_ID env yet (see chevron-1yn.2's
        // deferred follow-up). Surface that clearly instead of silently
        // doing nothing.
        "session" => {
            return Err(ParseOutcome::Err(
                "--filter-mode session not yet supported (deferred — needs CHEVRON_SESSION_ID export from shell init)".to_string(),
            ));
        }
        other => {
            return Err(ParseOutcome::Err(format!(
                "unknown --filter-mode: {other} (expected global|host|directory|workspace)"
            )));
        }
    }
    Ok(())
}

fn parse_i32(s: &str) -> Result<i32, ParseOutcome> {
    s.parse()
        .map_err(|_| ParseOutcome::Err(format!("expected integer, got {s:?}")))
}

fn parse_i64(s: &str) -> Result<i64, ParseOutcome> {
    s.parse()
        .map_err(|_| ParseOutcome::Err(format!("expected integer, got {s:?}")))
}

fn parse_usize(s: &str) -> Result<usize, ParseOutcome> {
    s.parse()
        .map_err(|_| ParseOutcome::Err(format!("expected non-negative integer, got {s:?}")))
}

// ── duration parsing ────────────────────────────────────────────────────────

/// Parse `--since`/`--until` arguments into a unix-epoch-ms lower bound
/// (for `--since`) or upper bound (for `--until`). Both accept the same
/// grammar; semantics differ at the SQL level.
///
/// Accepted forms:
///   - `now`                         → 0 ms ago = now
///   - `Nx` where x ∈ {s, m, h, d, w} → N units ago
///   - `N <unit> ago` (long form)     → same
fn parse_since(s: &str) -> Result<i64, ParseOutcome> {
    let ms_ago = parse_duration_to_ms(s)?;
    let now = now_unix_ms();
    Ok(now.saturating_sub(ms_ago))
}

fn parse_duration_to_ms(s: &str) -> Result<i64, ParseOutcome> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("now") {
        return Ok(0);
    }
    // Long form: "N <unit> ago" (3 whitespace-separated parts).
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() == 3 && parts[2].eq_ignore_ascii_case("ago") {
        let n: i64 = parts[0]
            .parse()
            .map_err(|_| ParseOutcome::Err(format!("invalid count in {s:?}")))?;
        let unit_ms = unit_to_ms(parts[1])
            .ok_or_else(|| ParseOutcome::Err(format!("unknown time unit in {s:?}")))?;
        return Ok(n.saturating_mul(unit_ms));
    }
    // Compact form: "Nx" (digits, then suffix).
    let cut = s.find(|c: char| !c.is_ascii_digit()).ok_or_else(|| {
        ParseOutcome::Err(format!(
            "{s:?}: missing unit suffix (expected one of s,m,h,d,w)"
        ))
    })?;
    if cut == 0 {
        return Err(ParseOutcome::Err(format!("{s:?}: missing numeric prefix")));
    }
    let n: i64 = s[..cut]
        .parse()
        .map_err(|_| ParseOutcome::Err(format!("invalid count in {s:?}")))?;
    let suffix = &s[cut..];
    let unit_ms = unit_to_ms(suffix)
        .ok_or_else(|| ParseOutcome::Err(format!("unknown unit suffix {suffix:?} in {s:?}")))?;
    Ok(n.saturating_mul(unit_ms))
}

fn unit_to_ms(unit: &str) -> Option<i64> {
    match unit.to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1_000),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(60_000),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(60 * 60_000),
        "d" | "day" | "days" => Some(24 * 60 * 60_000),
        "w" | "wk" | "wks" | "week" | "weeks" => Some(7 * 24 * 60 * 60_000),
        _ => None,
    }
}

// ── SQL composition ─────────────────────────────────────────────────────────

fn build_query(f: &Filters) -> (String, Vec<Value>) {
    let mut where_clauses: Vec<&'static str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(cwd) = &f.cwd {
        where_clauses.push("cwd = ?");
        params.push(Value::Text(cwd.clone()));
    }
    if let Some(session) = &f.session {
        where_clauses.push("session_id = ?");
        params.push(Value::Text(session.clone()));
    }
    if let Some(host) = &f.host {
        where_clauses.push("hostname = ?");
        params.push(Value::Text(host.clone()));
    }
    if let Some(exit) = f.exit_eq {
        where_clauses.push("exit_status = ?");
        params.push(Value::Integer(i64::from(exit)));
    }
    if f.failed_only {
        // exit_status > 0 implicitly excludes NULL (3-valued logic).
        where_clauses.push("exit_status > 0");
    }
    if f.success_only {
        where_clauses.push("exit_status = 0");
    }
    if let Some(since) = f.since_ms {
        where_clauses.push("started_at >= ?");
        params.push(Value::Integer(since));
    }
    if let Some(until) = f.until_ms {
        where_clauses.push("started_at <= ?");
        params.push(Value::Integer(until));
    }
    if let Some(prefix) = &f.workspace_prefix {
        where_clauses.push("cwd LIKE ? ESCAPE '\\'");
        // Match exact-root and any subdir of it.
        let escaped = like_escape(prefix);
        params.push(Value::Text(format!("{escaped}%")));
    }
    if let Some(pat) = &f.grep {
        where_clauses.push("cmd LIKE ? ESCAPE '\\'");
        let escaped = like_escape(pat);
        params.push(Value::Text(format!("%{escaped}%")));
    }
    if let Some(cwd) = &f.exclude_cwd {
        where_clauses.push("cwd != ?");
        params.push(Value::Text(cwd.clone()));
    }
    if let Some(host) = &f.exclude_host {
        where_clauses.push("hostname != ?");
        params.push(Value::Text(host.clone()));
    }
    if let Some(exit) = f.exclude_exit {
        where_clauses.push("(exit_status != ? OR exit_status IS NULL)");
        params.push(Value::Integer(i64::from(exit)));
    }
    if let Some(session) = &f.exclude_session {
        where_clauses.push("session_id != ?");
        params.push(Value::Text(session.clone()));
    }

    // `--unique` collapses to one row per distinct cmd, keeping the
    // most recent occurrence. Implemented via a `started_at IN (SELECT
    // MAX(started_at) … GROUP BY cmd)` subquery — the canonical
    // "latest-per-group" SQLite pattern. The filter predicate appears
    // BOTH in the outer query (so we only fetch the relevant rows) AND
    // in the subquery (so the GROUP BY only considers matching rows;
    // otherwise a popular old `cmd` not matching our filters could
    // shadow a more recent matching occurrence).
    let base_select = "SELECT id, session_id, hostname, cwd, cmd, \
        started_at, finished_at, duration_ms, exit_status FROM commands";
    let filter_join = where_clauses.join(" AND ");
    let outer_where = if where_clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {filter_join}")
    };
    let inner_where = outer_where.clone();

    let sql = if f.unique {
        let outer_prefix = if where_clauses.is_empty() {
            " WHERE ".to_string()
        } else {
            format!(" WHERE {filter_join} AND ")
        };
        format!(
            "{base_select}{outer_prefix}started_at IN \
             (SELECT MAX(started_at) FROM commands{inner_where} GROUP BY cmd) \
             ORDER BY started_at DESC LIMIT ?"
        )
    } else {
        format!("{base_select}{outer_where} ORDER BY started_at DESC LIMIT ?")
    };

    if f.unique {
        // Filter params appear twice (outer + subquery), then LIMIT.
        let mut doubled = params.clone();
        doubled.extend(params.iter().cloned());
        doubled.push(Value::Integer(i64::try_from(f.limit).unwrap_or(i64::MAX)));
        return (sql, doubled);
    }
    params.push(Value::Integer(i64::try_from(f.limit).unwrap_or(i64::MAX)));
    (sql, params)
}

/// LIKE-escape: `%`, `_`, and `\` become `\%`, `\_`, `\\`. Used in
/// conjunction with `LIKE ? ESCAPE '\\'` so user patterns don't carry
/// hidden wildcards. Backslash is doubled to survive Rust source
/// string parsing → SQL.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '%' || c == '_' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ── row execution ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Row {
    id: String,
    session_id: String,
    hostname: String,
    cwd: String,
    cmd: String,
    started_at: i64,
    finished_at: Option<i64>,
    duration_ms: Option<i64>,
    exit_status: Option<i64>,
}

fn execute(conn: &Connection, sql: &str, params: &[Value]) -> rusqlite::Result<Vec<Row>> {
    let mut stmt = conn.prepare(sql)?;
    let params_refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let iter = stmt.query_map(params_refs.as_slice(), |r| {
        Ok(Row {
            id: r.get(0)?,
            session_id: r.get(1)?,
            hostname: r.get(2)?,
            cwd: r.get(3)?,
            cmd: r.get(4)?,
            started_at: r.get(5)?,
            finished_at: r.get(6)?,
            duration_ms: r.get(7)?,
            exit_status: r.get(8)?,
        })
    })?;
    let mut rows = Vec::new();
    for r in iter {
        rows.push(r?);
    }
    Ok(rows)
}

// ── output formatting ───────────────────────────────────────────────────────

fn format_row(row: &Row, idx: usize, fmt: &Format, color: bool, now_ms: i64) -> String {
    match fmt {
        Format::Human => format_human(row, color, now_ms),
        Format::Json => format_json(row),
        Format::Cmds => format_cmds(row),
        Format::Ids => row.id.clone(),
        Format::Template(t) => format_template(row, idx, t),
    }
}

fn format_human(row: &Row, color: bool, now_ms: i64) -> String {
    let rel = relative_time(now_ms - row.started_at);
    let cwd = collapse_home(&row.cwd);
    let cmd_first_line = row.cmd.lines().next().unwrap_or("");
    let trailer = match row.exit_status {
        None => String::new(),
        Some(0) => match row.duration_ms {
            Some(ms) if ms >= 1000 => format!("  ({})", format_duration(ms)),
            _ => String::new(),
        },
        Some(n) => format!("  ✗ exit {n}"),
    };
    if color {
        let dim = "\x1b[2m";
        let red = "\x1b[31m";
        let reset = "\x1b[0m";
        let cyan = "\x1b[36m";
        let trailer_colored = match row.exit_status {
            Some(n) if n != 0 => format!("  {red}✗ exit {n}{reset}"),
            _ if !trailer.is_empty() => format!("{dim}{trailer}{reset}"),
            _ => String::new(),
        };
        format!("{dim}{rel:>9}{reset}  {cyan}{cwd}{reset}  {cmd_first_line}{trailer_colored}")
    } else {
        format!("{rel:>9}  {cwd}  {cmd_first_line}{trailer}")
    }
}

fn format_cmds(row: &Row) -> String {
    row.cmd.lines().next().unwrap_or("").to_string()
}

fn format_json(row: &Row) -> String {
    let mut out = String::with_capacity(256);
    out.push('{');
    push_json_str(&mut out, "id", &row.id);
    out.push(',');
    push_json_str(&mut out, "session_id", &row.session_id);
    out.push(',');
    push_json_str(&mut out, "hostname", &row.hostname);
    out.push(',');
    push_json_str(&mut out, "cwd", &row.cwd);
    out.push(',');
    push_json_str(&mut out, "cmd", &row.cmd);
    out.push(',');
    push_json_num(&mut out, "started_at", row.started_at);
    out.push(',');
    push_json_opt_num(&mut out, "finished_at", row.finished_at);
    out.push(',');
    push_json_opt_num(&mut out, "duration_ms", row.duration_ms);
    out.push(',');
    push_json_opt_num(&mut out, "exit_status", row.exit_status);
    out.push('}');
    out
}

fn format_template(row: &Row, idx: usize, tmpl: &str) -> String {
    let mut out = String::with_capacity(tmpl.len() * 2);
    let bytes = tmpl.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{'
            && let Some(end) = tmpl[i..].find('}')
        {
            let key = &tmpl[i + 1..i + end];
            substitute_field(&mut out, row, idx, key);
            i += end + 1;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn substitute_field(out: &mut String, row: &Row, idx: usize, key: &str) {
    use std::fmt::Write;
    match key {
        "id" => out.push_str(&row.id),
        "session" => out.push_str(&row.session_id),
        "host" => out.push_str(&row.hostname),
        "cwd" => out.push_str(&row.cwd),
        "cmd" => out.push_str(&row.cmd),
        "started_at" => {
            let _ = write!(out, "{}", row.started_at);
        }
        "finished_at" => match row.finished_at {
            Some(n) => {
                let _ = write!(out, "{n}");
            }
            None => out.push_str("null"),
        },
        "duration" => match row.duration_ms {
            Some(n) => {
                let _ = write!(out, "{n}");
            }
            None => out.push_str("null"),
        },
        "exit" => match row.exit_status {
            Some(n) => {
                let _ = write!(out, "{n}");
            }
            None => out.push_str("null"),
        },
        "n" => {
            let _ = write!(out, "{idx}");
        }
        // Unknown placeholder: leave as-is so the user can debug.
        other => {
            out.push('{');
            out.push_str(other);
            out.push('}');
        }
    }
}

fn push_json_str(out: &mut String, key: &str, value: &str) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":\"");
    json_escape_into(out, value);
    out.push('"');
}

fn push_json_num(out: &mut String, key: &str, value: i64) {
    use std::fmt::Write;
    let _ = write!(out, "\"{key}\":{value}");
}

fn push_json_opt_num(out: &mut String, key: &str, value: Option<i64>) {
    use std::fmt::Write;
    match value {
        Some(n) => {
            let _ = write!(out, "\"{key}\":{n}");
        }
        None => {
            let _ = write!(out, "\"{key}\":null");
        }
    }
}

fn json_escape_into(out: &mut String, s: &str) {
    use std::fmt::Write;
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

// ── presentation helpers ────────────────────────────────────────────────────

// Time-bucket thresholds for `relative_time`. Hoisted out of the
// function body to satisfy `clippy::items_after_statements`.
const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;
const YEAR: i64 = 365 * DAY;

fn relative_time(ago_ms: i64) -> String {
    let ago_ms = ago_ms.max(0);
    // < 60 s → "Ns"
    // < 60 m → "Nm"
    // < 24 h → "Nh"
    // < 30 d → "Nd"
    // < 365 d → "Nw"
    // else   → "Ny"
    if ago_ms < MINUTE {
        format!("{}s ago", ago_ms / SECOND)
    } else if ago_ms < HOUR {
        format!("{}m ago", ago_ms / MINUTE)
    } else if ago_ms < DAY {
        format!("{}h ago", ago_ms / HOUR)
    } else if ago_ms < 30 * DAY {
        format!("{}d ago", ago_ms / DAY)
    } else if ago_ms < YEAR {
        format!("{}w ago", ago_ms / WEEK)
    } else {
        format!("{}y ago", ago_ms / YEAR)
    }
}

fn format_duration(ms: i64) -> String {
    if ms >= 60_000 {
        format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1_000)
    } else {
        format!("{}.{}s", ms / 1_000, (ms % 1_000) / 100)
    }
}

fn collapse_home(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home_str = home.to_string_lossy();
        if !home_str.is_empty() && path.starts_with(home_str.as_ref()) {
            let rest = &path[home_str.len()..];
            if rest.is_empty() {
                return "~".to_string();
            }
            if rest.starts_with('/') {
                return format!("~{rest}");
            }
        }
    }
    path.to_string()
}

// ── environment helpers ─────────────────────────────────────────────────────

fn current_dir_string() -> Result<String, ParseOutcome> {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| ParseOutcome::Err(format!("reading current directory: {e}")))
}

fn workspace_root_string() -> Result<String, ParseOutcome> {
    let cwd = std::env::current_dir()
        .map_err(|e| ParseOutcome::Err(format!("reading current directory: {e}")))?;
    let repo = git2::Repository::discover(&cwd)
        .map_err(|_| ParseOutcome::Err("--workspace: not inside a git repository".to_string()))?;
    let root = repo.workdir().ok_or_else(|| {
        ParseOutcome::Err("--workspace: repository has no workdir (bare repo?)".to_string())
    })?;
    Ok(root.to_string_lossy().trim_end_matches('/').to_string())
}

fn hostname_or_unknown() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes and either
    // null-terminates the result or returns -1; passing a valid
    // &mut [u8] of the matching length is the documented contract.
    let rc = unsafe {
        libc::gethostname(
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len() as libc::size_t,
        )
    };
    if rc != 0 {
        return "unknown".to_string();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..end]).map_or_else(|_| "unknown".to_string(), str::to_string)
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn stdout_is_color_tty() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::io::stdout().is_terminal()
}

// ── schema check ────────────────────────────────────────────────────────────

fn check_schema(conn: &Connection) -> Result<(), String> {
    let version: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("reading commands.db schema_version: {e}"))?;
    if version != SUPPORTED_SCHEMA {
        return Err(format!(
            "unsupported commands.db schema version {version} (this chevron expects {SUPPORTED_SCHEMA}). Upgrade chevron."
        ));
    }
    Ok(())
}

// ── help ────────────────────────────────────────────────────────────────────

fn help_text() -> String {
    "Usage: chevron history [FLAGS] [PATTERN]

Query the chevrond command-lifecycle log.

Filters:
  --cwd PATH               commands run in this cwd (exact match)
  --here                   shortcut for --cwd \"$(pwd)\"
  --session ID             session-id exact match
  --host NAME              hostname exact match
  --exit N                 exit_status == N
  --failed                 exit_status > 0 (still-running rows excluded)
  --success                exit_status == 0
  --since DURATION         within the last DURATION
  --until DURATION         older than DURATION
  --since-ms MS            unix-epoch-ms lower bound
  --until-ms MS            unix-epoch-ms upper bound
  --workspace              limit to the current git work-tree
  --grep PATTERN, -g       substring match against cmd
  PATTERN                  positional shortcut for --grep

Exclusions:
  --exclude-cwd PATH       drop rows with this cwd
  --exclude-host NAME      drop rows with this hostname
  --exclude-exit N         drop rows with this exit status
  --exclude-session ID     drop rows with this session id

Preset combos:
  --filter-mode MODE       global | host | directory | workspace

Display:
  --limit N, -n N          cap rows (default 25, max 10000)
  --reverse, -r            flip the default oldest-at-top order
  --unique, -u             one row per distinct cmd (most recent)
  --format FMT, -f FMT     human (default) | json | cmds | ids | <template>
  -h, --help               this help

Time durations:
  Ns, Nm, Nh, Nd, Nw            seconds/minutes/hours/days/weeks
  \"N <unit> ago\"                e.g. \"3 hours ago\"
  now                           zero ago

Templates:
  Any non-keyword --format value is treated as a template with
  {id}, {cwd}, {cmd}, {exit}, {duration}, {started_at}, {session},
  {host}, {n} substitutions.

Examples:
  chevron history                            last 25 commands
  chevron history --here                     in this directory
  chevron history --failed --since 1d        failures in last day
  chevron history --workspace cargo          'cargo' in this repo
  chevron history -f json | jq ...           machine-readable
  chevron history -f '{cmd}' --unique        unique cmds, one per line
"
    .to_string()
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    // ── duration parsing ────────────────────────────────────────────────

    #[test]
    fn duration_compact_forms() {
        assert_eq!(parse_duration_to_ms("1s").unwrap(), 1_000);
        assert_eq!(parse_duration_to_ms("30s").unwrap(), 30_000);
        assert_eq!(parse_duration_to_ms("5m").unwrap(), 5 * 60_000);
        assert_eq!(parse_duration_to_ms("2h").unwrap(), 2 * 3_600_000);
        assert_eq!(parse_duration_to_ms("1d").unwrap(), 86_400_000);
        assert_eq!(parse_duration_to_ms("1w").unwrap(), 7 * 86_400_000);
    }

    #[test]
    fn duration_long_forms() {
        assert_eq!(parse_duration_to_ms("3 hours ago").unwrap(), 3 * 3_600_000);
        assert_eq!(parse_duration_to_ms("1 day ago").unwrap(), 86_400_000);
        assert_eq!(
            parse_duration_to_ms("2 weeks ago").unwrap(),
            2 * 7 * 86_400_000
        );
        assert_eq!(parse_duration_to_ms("30 minutes ago").unwrap(), 30 * 60_000);
    }

    #[test]
    fn duration_now() {
        assert_eq!(parse_duration_to_ms("now").unwrap(), 0);
        assert_eq!(parse_duration_to_ms("NOW").unwrap(), 0);
    }

    #[test]
    fn duration_rejects_bad_unit() {
        assert!(parse_duration_to_ms("1y").is_err());
        assert!(parse_duration_to_ms("30").is_err());
        assert!(parse_duration_to_ms("abc").is_err());
        assert!(parse_duration_to_ms("d").is_err());
    }

    // ── arg parsing ─────────────────────────────────────────────────────

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn args_default_filters() {
        let f = parse_args(&args(&[])).unwrap();
        assert_eq!(f.limit, DEFAULT_LIMIT);
        assert!(!f.failed_only);
        assert!(matches!(f.format, Format::Human));
    }

    #[test]
    fn args_positional_becomes_grep() {
        let f = parse_args(&args(&["cargo"])).unwrap();
        assert_eq!(f.grep.as_deref(), Some("cargo"));
    }

    #[test]
    fn args_failed_and_success_conflict() {
        assert!(parse_args(&args(&["--failed", "--success"])).is_err());
    }

    #[test]
    fn args_failed_and_exit_zero_conflict() {
        assert!(parse_args(&args(&["--failed", "--exit", "0"])).is_err());
    }

    #[test]
    fn args_success_and_exit_nonzero_conflict() {
        assert!(parse_args(&args(&["--success", "--exit", "1"])).is_err());
    }

    #[test]
    fn args_limit_over_max_errors() {
        assert!(parse_args(&args(&["--limit", "1000000"])).is_err());
    }

    #[test]
    fn args_unknown_flag_errors() {
        assert!(parse_args(&args(&["--mystery"])).is_err());
    }

    #[test]
    fn args_help_returns_help_outcome() {
        match parse_args(&args(&["-h"])) {
            Err(ParseOutcome::Help) => {}
            other => panic!("expected Help, got {other:?}"),
        }
        match parse_args(&args(&["--help"])) {
            Err(ParseOutcome::Help) => {}
            other => panic!("expected Help, got {other:?}"),
        }
    }

    #[test]
    fn args_format_template_passes_through() {
        let f = parse_args(&args(&["--format", "{cmd}"])).unwrap();
        match f.format {
            Format::Template(s) => assert_eq!(s, "{cmd}"),
            other => panic!("expected Template, got {other:?}"),
        }
    }

    // ── like escape ─────────────────────────────────────────────────────

    #[test]
    fn like_escape_protects_wildcards() {
        assert_eq!(like_escape("foo"), "foo");
        assert_eq!(like_escape("100%"), "100\\%");
        assert_eq!(like_escape("a_b"), "a\\_b");
        assert_eq!(like_escape(r"\foo"), r"\\foo");
    }

    // ── query composition ───────────────────────────────────────────────

    #[test]
    fn build_query_no_filters() {
        let f = Filters {
            limit: 10,
            ..Default::default()
        };
        let (sql, params) = build_query(&f);
        assert!(sql.contains("ORDER BY started_at DESC"));
        assert!(sql.contains("LIMIT ?"));
        assert!(!sql.contains("WHERE"));
        // Only the LIMIT param.
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn build_query_failed_emits_exit_status_clause() {
        let f = Filters {
            failed_only: true,
            limit: 10,
            ..Default::default()
        };
        let (sql, _) = build_query(&f);
        assert!(sql.contains("exit_status > 0"));
    }

    #[test]
    fn build_query_grep_uses_like_with_escape() {
        let f = Filters {
            grep: Some("cargo".to_string()),
            limit: 10,
            ..Default::default()
        };
        let (sql, params) = build_query(&f);
        assert!(sql.contains("cmd LIKE ? ESCAPE '\\'"));
        // First param is the LIKE pattern.
        assert!(matches!(&params[0], Value::Text(s) if s == "%cargo%"));
    }

    #[test]
    fn build_query_workspace_uses_prefix_like() {
        let f = Filters {
            workspace_prefix: Some("/Users/mim/src/chevron".to_string()),
            limit: 10,
            ..Default::default()
        };
        let (sql, params) = build_query(&f);
        assert!(sql.contains("cwd LIKE ? ESCAPE '\\'"));
        assert!(
            matches!(&params[0], Value::Text(s) if s == "/Users/mim/src/chevron%"),
            "expected prefix pattern, got {:?}",
            params[0]
        );
    }

    // ── end-to-end via in-memory DB ─────────────────────────────────────

    fn fixture_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta(key, value) VALUES('schema_version', '1');
             CREATE TABLE commands (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                hostname TEXT NOT NULL,
                cwd TEXT NOT NULL,
                cmd TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                finished_at INTEGER,
                duration_ms INTEGER,
                exit_status INTEGER);",
        )
        .unwrap();
        conn
    }

    fn insert_row(
        conn: &Connection,
        id: &str,
        cwd: &str,
        cmd: &str,
        started: i64,
        exit: Option<i32>,
    ) {
        conn.execute(
            "INSERT INTO commands (id, session_id, hostname, cwd, cmd, started_at, finished_at, duration_ms, exit_status)
             VALUES (?1, 'sess', 'host', ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                cwd,
                cmd,
                started,
                exit.map(|_| started + 100),
                exit.map(|_| 100i64),
                exit.map(i64::from),
            ],
        ).unwrap();
    }

    #[test]
    fn query_filters_by_grep() {
        let conn = fixture_conn();
        insert_row(&conn, "a", "/x", "cargo test", 1_000, Some(0));
        insert_row(&conn, "b", "/x", "git status", 2_000, Some(0));
        insert_row(&conn, "c", "/x", "cargo check", 3_000, Some(0));

        let f = Filters {
            grep: Some("cargo".to_string()),
            limit: 10,
            ..Default::default()
        };
        let (sql, params) = build_query(&f);
        let rows = execute(&conn, &sql, &params).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.cmd.contains("cargo")));
    }

    #[test]
    fn query_failed_excludes_zero_and_null() {
        let conn = fixture_conn();
        insert_row(&conn, "a", "/x", "ok", 1_000, Some(0));
        insert_row(&conn, "b", "/x", "fail", 2_000, Some(1));
        insert_row(&conn, "c", "/x", "running", 3_000, None);

        let f = Filters {
            failed_only: true,
            limit: 10,
            ..Default::default()
        };
        let (sql, params) = build_query(&f);
        let rows = execute(&conn, &sql, &params).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cmd, "fail");
    }

    #[test]
    fn query_workspace_matches_subdir() {
        let conn = fixture_conn();
        insert_row(&conn, "a", "/repo", "cmd-a", 1_000, Some(0));
        insert_row(&conn, "b", "/repo/sub", "cmd-b", 2_000, Some(0));
        insert_row(&conn, "c", "/other", "cmd-c", 3_000, Some(0));

        let f = Filters {
            workspace_prefix: Some("/repo".to_string()),
            limit: 10,
            ..Default::default()
        };
        let (sql, params) = build_query(&f);
        let rows = execute(&conn, &sql, &params).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.cwd.starts_with("/repo")));
    }

    #[test]
    fn query_unique_keeps_most_recent_per_cmd() {
        let conn = fixture_conn();
        // Same cmd, three timestamps; --unique should keep the latest.
        insert_row(&conn, "a", "/x", "ls", 1_000, Some(0));
        insert_row(&conn, "b", "/x", "ls", 2_000, Some(0));
        insert_row(&conn, "c", "/x", "ls", 3_000, Some(0));
        insert_row(&conn, "d", "/x", "ps", 4_000, Some(0));

        let f = Filters {
            unique: true,
            limit: 10,
            ..Default::default()
        };
        let (sql, params) = build_query(&f);
        let rows = execute(&conn, &sql, &params).unwrap();
        assert_eq!(rows.len(), 2, "rows: {rows:?}");
        // Most recent of `ls` is id "c".
        let ls_row = rows.iter().find(|r| r.cmd == "ls").unwrap();
        assert_eq!(ls_row.id, "c");
    }

    #[test]
    fn query_since_until_bracket_window() {
        let conn = fixture_conn();
        insert_row(&conn, "a", "/x", "old", 1_000, Some(0));
        insert_row(&conn, "b", "/x", "mid", 2_000, Some(0));
        insert_row(&conn, "c", "/x", "new", 3_000, Some(0));

        let f = Filters {
            since_ms: Some(2_000),
            until_ms: Some(2_500),
            limit: 10,
            ..Default::default()
        };
        let (sql, params) = build_query(&f);
        let rows = execute(&conn, &sql, &params).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cmd, "mid");
    }

    #[test]
    fn query_exclude_cwd_drops_matching() {
        let conn = fixture_conn();
        insert_row(&conn, "a", "/keep", "x", 1_000, Some(0));
        insert_row(&conn, "b", "/drop", "y", 2_000, Some(0));

        let f = Filters {
            exclude_cwd: Some("/drop".to_string()),
            limit: 10,
            ..Default::default()
        };
        let (sql, params) = build_query(&f);
        let rows = execute(&conn, &sql, &params).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cwd, "/keep");
    }

    // ── formatting ──────────────────────────────────────────────────────

    fn fixture_row() -> Row {
        Row {
            id: "01HW".to_string(),
            session_id: "sess".to_string(),
            hostname: "host".to_string(),
            cwd: "/home/mim/src/chevron".to_string(),
            cmd: "cargo test --features daemon".to_string(),
            started_at: 1_716_350_000_000,
            finished_at: Some(1_716_350_000_500),
            duration_ms: Some(500),
            exit_status: Some(0),
        }
    }

    #[test]
    fn human_format_no_color() {
        let row = fixture_row();
        let now = row.started_at + 5 * 60_000; // 5 m later
        let out = format_human(&row, false, now);
        assert!(out.contains("5m ago"));
        assert!(out.contains("cargo test --features daemon"));
        assert!(!out.contains('\x1b'), "no ANSI escapes in non-color mode");
    }

    #[test]
    fn human_format_marks_failures() {
        let row = Row {
            exit_status: Some(127),
            ..fixture_row()
        };
        let now = row.started_at + 30 * 1_000;
        let out = format_human(&row, false, now);
        assert!(out.contains("✗ exit 127"), "got: {out:?}");
    }

    #[test]
    fn human_format_shows_duration_for_slow_commands() {
        let row = Row {
            duration_ms: Some(2_500),
            ..fixture_row()
        };
        let now = row.started_at + 60_000;
        let out = format_human(&row, false, now);
        assert!(out.contains("2.5s") || out.contains("(2"), "got: {out:?}");
    }

    #[test]
    fn json_format_escapes_specials() {
        let row = Row {
            cmd: "echo 'hi\ttab\nnew\"quote'".to_string(),
            ..fixture_row()
        };
        let out = format_json(&row);
        assert!(out.contains(r#""cmd":"echo 'hi\ttab\nnew\"quote'""#));
        // Must be valid one-line JSON.
        assert!(!out.contains('\n'));
    }

    #[test]
    fn json_format_handles_null_completion_fields() {
        let row = Row {
            finished_at: None,
            duration_ms: None,
            exit_status: None,
            ..fixture_row()
        };
        let out = format_json(&row);
        assert!(out.contains(r#""finished_at":null"#));
        assert!(out.contains(r#""duration_ms":null"#));
        assert!(out.contains(r#""exit_status":null"#));
    }

    #[test]
    fn template_substitutes_known_fields() {
        let row = fixture_row();
        let out = format_template(&row, 0, "{cmd} -- {exit}");
        assert_eq!(out, "cargo test --features daemon -- 0");
    }

    #[test]
    fn template_unknown_placeholder_passes_through() {
        let row = fixture_row();
        let out = format_template(&row, 0, "{nope}");
        assert_eq!(out, "{nope}");
    }

    #[test]
    fn template_n_is_row_index() {
        let row = fixture_row();
        let out = format_template(&row, 3, "{n}: {cmd}");
        assert_eq!(out, "3: cargo test --features daemon");
    }

    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(0), "0s ago");
        assert_eq!(relative_time(45 * 1_000), "45s ago");
        assert_eq!(relative_time(10 * 60_000), "10m ago");
        assert_eq!(relative_time(3 * 3_600_000), "3h ago");
        assert_eq!(relative_time(2 * 86_400_000), "2d ago");
        assert_eq!(relative_time(45 * 86_400_000), "6w ago");
        assert_eq!(relative_time(400i64 * 86_400_000), "1y ago");
    }

    #[test]
    fn collapse_home_replaces_prefix() {
        // SAFETY: tests are #[serial] elsewhere; set_var in single test is fine.
        unsafe { std::env::set_var("HOME", "/Users/mim") };
        assert_eq!(collapse_home("/Users/mim/src/x"), "~/src/x");
        assert_eq!(collapse_home("/Users/mim"), "~");
        assert_eq!(collapse_home("/etc/hosts"), "/etc/hosts");
    }

    // ── schema check ────────────────────────────────────────────────────

    #[test]
    fn schema_check_passes_for_version_1() {
        let conn = fixture_conn();
        assert!(check_schema(&conn).is_ok());
    }

    #[test]
    fn schema_check_rejects_other_versions() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta(key, value) VALUES('schema_version', '999');",
        )
        .unwrap();
        let err = check_schema(&conn).unwrap_err();
        assert!(err.contains("999"), "err: {err}");
    }
}
