//! Live settings and a read-only, time-bounded daemon handshake.
use std::path::Path;

use crate::config::{Config, ShellConfig};
use crate::health::Check;

pub(super) fn section() -> Vec<Check> {
    let (config, config_check) = load_shell_config(&crate::config::config_path());
    let live_env = std::env::var("CHEVRON_LIVE").ok();
    let scope = std::env::var("CHEVRON_LIVE_SCOPE").ok();
    let shell = std::env::var("SHELL").unwrap_or_default();
    let zsh = Path::new(&shell).file_name().is_some_and(|s| s == "zsh");
    let enabled = live_env.as_deref().map_or(config.live, |v| v != "0");
    let disabled = std::env::var_os("CHEVRON_NO_DAEMON").is_some();
    let mut checks = vec![
        config_check,
        live_check(&config, live_env.as_deref(), zsh),
        scope_check(scope.as_deref()),
    ];
    if disabled {
        checks.push(Check::info_hint(
            "daemon_connection",
            "daemon connection",
            "daemon access disabled by CHEVRON_NO_DAEMON",
            "unset CHEVRON_NO_DAEMON to use daemon caching and live updates",
        ));
    } else {
        #[cfg(feature = "daemon")]
        checks.push(daemon_check(
            &crate::daemon::paths::socket_path(),
            enabled && zsh,
        ));
        #[cfg(not(feature = "daemon"))]
        {
            let _ = enabled;
            checks.push(Check::info(
                "daemon_connection",
                "daemon connection",
                "not compiled in",
            ));
        }
    }
    checks
}

fn load_shell_config(path: &Path) -> (ShellConfig, Check) {
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(config) => (
                config.shell,
                Check::info("live_config", "live config", path.display().to_string()),
            ),
            Err(_) => (
                ShellConfig::default(),
                Check::warn(
                    "live_config",
                    "live config",
                    format!("invalid config at {}; using defaults", path.display()),
                    "fix the TOML configuration; environment overrides still take precedence",
                ),
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            ShellConfig::default(),
            Check::info(
                "live_config",
                "live config",
                format!("{} not present; built-in defaults", path.display()),
            ),
        ),
        Err(_) => (
            ShellConfig::default(),
            Check::warn(
                "live_config",
                "live config",
                format!("cannot read {}; using defaults", path.display()),
                "check the configuration file permissions",
            ),
        ),
    }
}

fn live_check(config: &ShellConfig, value: Option<&str>, zsh: bool) -> Check {
    let enabled = value.map_or(config.live, |v| v != "0");
    let state = if enabled { "enabled" } else { "disabled" };
    let source = if value.is_some() {
        "inherited CHEVRON_LIVE"
    } else {
        "config/default for the next shell initialization"
    };
    let summary = format!("{state} ({source})");
    if !zsh {
        return Check::info_hint(
            "live_prompt",
            "live prompt",
            summary,
            "live redraws are implemented for zsh; SHELL identifies the login shell, not necessarily the current process",
        );
    }
    if value.is_some_and(|v| v != "0" && v != "1") {
        return Check::warn(
            "live_prompt",
            "live prompt",
            summary,
            "use CHEVRON_LIVE=0 or 1; the shell treats any value other than 0 as enabled",
        );
    }
    let hint = if enabled {
        "configuration only: this does not prove a subscriber is attached; inspect daemon connection below"
    } else {
        "live redraws are intentionally off; normal prompt rendering still works"
    };
    Check::info_hint("live_prompt", "live prompt", summary, hint)
}

fn scope_check(scope: Option<&str>) -> Check {
    match scope {
        None | Some("cwd") => Check::info(
            "live_scope",
            "live scope",
            "cwd: events from the current directory/repository",
        ),
        Some("all") => Check::info_hint(
            "live_scope",
            "live scope",
            "all: events from every repository",
            "use CHEVRON_LIVE_SCOPE=cwd to avoid unrelated repository redraws",
        ),
        Some(_) => Check::warn(
            "live_scope",
            "live scope",
            "unrecognized scope; shell uses cwd behavior",
            "set CHEVRON_LIVE_SCOPE to cwd or all",
        ),
    }
}

#[cfg(feature = "daemon")]
mod probe {
    use crate::daemon::proto::{self, DaemonVersion, Request, Response};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[derive(Debug)]
    pub(super) enum Failure {
        Io(std::io::ErrorKind),
        Protocol,
        InvalidReply,
        Timeout,
    }

    /// The outer deadline also covers connect backlog stalls and a peer
    /// trickling bytes indefinitely. No daemon spawn, restart or subscription.
    pub(super) fn version(
        path: &Path,
        budget: Duration,
    ) -> Result<(DaemonVersion, Duration), Failure> {
        let (tx, rx) = mpsc::channel();
        let path: PathBuf = path.into();
        let started = Instant::now();
        std::thread::Builder::new()
            .name("doctor-daemon-probe".into())
            .spawn(move || {
                let _ = tx.send(exchange(&path, budget));
            })
            .map_err(|e| Failure::Io(e.kind()))?;
        rx.recv_timeout(budget)
            .map_err(|_| Failure::Timeout)?
            .map(|version| (version, started.elapsed()))
    }

    fn exchange(path: &Path, budget: Duration) -> Result<DaemonVersion, Failure> {
        let mut conn = UnixStream::connect(path).map_err(|e| Failure::Io(e.kind()))?;
        conn.set_read_timeout(Some(budget))
            .map_err(|e| Failure::Io(e.kind()))?;
        conn.set_write_timeout(Some(budget))
            .map_err(|e| Failure::Io(e.kind()))?;
        let reader_conn = conn.try_clone().map_err(|e| Failure::Io(e.kind()))?;
        let mut reader = BufReader::new(reader_conn);
        send(&mut conn, &Request::Hello(proto::PROTO_VERSION))?;
        match receive(&mut reader)? {
            Response::Hello(v) if v == proto::PROTO_VERSION => {}
            Response::Hello(_) => return Err(Failure::Protocol),
            _ => return Err(Failure::InvalidReply),
        }
        send(&mut conn, &Request::Version)?;
        match receive(&mut reader)? {
            Response::Version(version) => Ok(version),
            _ => Err(Failure::InvalidReply),
        }
    }

    fn send(conn: &mut UnixStream, req: &Request) -> Result<(), Failure> {
        writeln!(conn, "{}", proto::encode_request(req)).map_err(|e| Failure::Io(e.kind()))
    }

    fn receive(reader: &mut BufReader<UnixStream>) -> Result<Response, Failure> {
        let mut text = String::new();
        reader
            .take(4097)
            .read_line(&mut text)
            .map_err(|e| Failure::Io(e.kind()))?;
        if text.len() > 4096 || !text.ends_with('\n') {
            return Err(Failure::InvalidReply);
        }
        proto::decode_response(&text).map_err(|_| Failure::InvalidReply)
    }
}

#[cfg(feature = "daemon")]
fn daemon_check(path: &Path, expected: bool) -> Check {
    use crate::daemon::{proto, state};
    use probe::Failure;
    use std::io::ErrorKind;
    use std::time::Duration;
    let restart =
        "run `chevron daemon stop`, then `chevron daemon start` to use the installed binary";
    match probe::version(path, Duration::from_millis(500)) {
        Ok((version, elapsed)) => {
            let value = format!(
                "{}: binary {}, protocol {}, schema {}; {:.1} ms",
                path.display(),
                version.binary,
                version.proto,
                version.schema,
                elapsed.as_secs_f64() * 1000.0
            );
            if version.proto != proto::PROTO_VERSION
                || version.schema != state::CURRENT_SCHEMA_VERSION
            {
                Check::critical("daemon_connection", "daemon connection", value, restart)
            } else if version.binary != env!("CARGO_PKG_VERSION") {
                Check::warn(
                    "daemon_connection",
                    "daemon connection",
                    value,
                    format!("CLI is {}; {restart}", env!("CARGO_PKG_VERSION")),
                )
            } else {
                Check::ok("daemon_connection", "daemon connection", value)
            }
        }
        Err(Failure::Io(ErrorKind::NotFound)) if !expected => Check::info(
            "daemon_connection",
            "daemon connection",
            format!("not running ({} missing)", path.display()),
        ),
        Err(Failure::Io(ErrorKind::NotFound)) => Check::warn(
            "daemon_connection",
            "daemon connection",
            format!("not running ({} missing)", path.display()),
            "start it with `chevron daemon start` to receive live events",
        ),
        Err(Failure::Io(ErrorKind::ConnectionRefused)) => Check::warn(
            "daemon_connection",
            "daemon connection",
            format!("stale socket or no listener at {}", path.display()),
            restart,
        ),
        Err(Failure::Protocol) => Check::critical(
            "daemon_connection",
            "daemon connection",
            "protocol mismatch",
            restart,
        ),
        Err(Failure::Timeout | Failure::Io(ErrorKind::TimedOut | ErrorKind::WouldBlock)) => {
            Check::warn(
                "daemon_connection",
                "daemon connection",
                "no complete response within 500 ms",
                "daemon may be overloaded or stuck; check `chevron daemon status`",
            )
        }
        Err(Failure::InvalidReply) => Check::warn(
            "daemon_connection",
            "daemon connection",
            "invalid daemon response",
            restart,
        ),
        Err(Failure::Io(kind)) => Check::warn(
            "daemon_connection",
            "daemon connection",
            format!("cannot query {}: {kind:?}", path.display()),
            "check socket directory permissions and CHEVRON_SOCKET_DIR",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::Severity;

    #[cfg(feature = "daemon")]
    fn peer(
        replies: Vec<crate::daemon::proto::Response>,
    ) -> (tempfile::TempDir, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, BufReader, Write};
        let tmp = tempfile::tempdir().unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(tmp.path().join("daemon.sock")).unwrap();
        let join = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut requests = Vec::new();
            for reply in replies {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                requests.push(line.trim().to_string());
                writeln!(stream, "{}", crate::daemon::proto::encode_response(&reply)).unwrap();
            }
            requests
        });
        (tmp, join)
    }

    #[test]
    #[cfg(feature = "daemon")]
    fn daemon_probe_is_read_only_and_reports_version_drift() {
        use crate::daemon::{
            proto::{DaemonVersion, PROTO_VERSION, Response},
            state,
        };
        for (binary, schema, expected) in [
            (
                env!("CARGO_PKG_VERSION"),
                state::CURRENT_SCHEMA_VERSION.to_string(),
                Severity::Ok,
            ),
            (
                "old-version",
                state::CURRENT_SCHEMA_VERSION.to_string(),
                Severity::Warn,
            ),
            (
                env!("CARGO_PKG_VERSION"),
                "unsupported".into(),
                Severity::Critical,
            ),
        ] {
            let (tmp, join) = peer(vec![
                Response::Hello(PROTO_VERSION),
                Response::Version(DaemonVersion {
                    binary: binary.into(),
                    proto: PROTO_VERSION,
                    schema,
                }),
            ]);
            let check = daemon_check(&tmp.path().join("daemon.sock"), true);
            assert_eq!(check.severity, expected, "{}", check.value);
            assert!(check.value.contains("ms"));
            assert_eq!(
                join.join().unwrap(),
                vec![format!("HELLO {PROTO_VERSION}"), "VERSION".into()]
            );
        }
    }

    #[test]
    #[cfg(feature = "daemon")]
    fn daemon_probe_distinguishes_missing_stale_and_invalid_protocol() {
        use crate::daemon::proto::{PROTO_VERSION, Response};
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.sock");
        assert_eq!(daemon_check(&path, false).severity, Severity::Info);
        assert_eq!(daemon_check(&path, true).severity, Severity::Warn);
        assert!(!path.exists(), "doctor must not start or create a daemon");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(daemon_check(&path, true).value.contains("stale socket"));
        let (tmp, join) = peer(vec![Response::Hello(PROTO_VERSION + 1)]);
        assert_eq!(
            daemon_check(&tmp.path().join("daemon.sock"), true).severity,
            Severity::Critical
        );
        join.join().unwrap();
        let (tmp, join) = peer(vec![Response::Ack]);
        assert!(
            daemon_check(&tmp.path().join("daemon.sock"), true)
                .value
                .contains("invalid")
        );
        join.join().unwrap();
    }

    #[test]
    #[cfg(feature = "daemon")]
    fn unresponsive_daemon_cannot_stall_doctor() {
        use std::time::{Duration, Instant};
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            let _ = done_rx.recv_timeout(Duration::from_secs(2));
        });
        let start = Instant::now();
        let result = probe::version(&path, Duration::from_millis(30));
        assert!(
            matches!(
                result,
                Err(probe::Failure::Timeout
                    | probe::Failure::Io(
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ))
            ),
            "{result:?}"
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        done_tx.send(()).unwrap();
        join.join().unwrap();
    }

    #[test]
    fn inherited_disable_wins_over_config_and_is_not_an_error() {
        let check = live_check(&ShellConfig::default(), Some("0"), true);
        assert_eq!(check.severity, Severity::Info);
        assert!(check.value.starts_with("disabled"));
        assert!(check.value.contains("inherited CHEVRON_LIVE"));
        let cfg = ShellConfig {
            live: false,
            ..ShellConfig::default()
        };
        assert!(
            live_check(&cfg, Some("1"), true)
                .value
                .starts_with("enabled")
        );
        assert!(
            live_check(&cfg, None, true)
                .value
                .contains("next shell initialization")
        );
    }

    #[test]
    fn nonstandard_values_match_shell_semantics_but_warn() {
        assert_eq!(
            live_check(&ShellConfig::default(), Some("false"), true).severity,
            Severity::Warn
        );
        assert!(
            live_check(&ShellConfig::default(), Some(""), true)
                .value
                .starts_with("enabled")
        );
        assert_eq!(scope_check(Some("bogus")).severity, Severity::Warn);
        assert!(scope_check(Some("all")).hint.is_some());
    }

    #[test]
    fn invalid_config_does_not_echo_secret_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "password = SUPER_SECRET invalid toml").unwrap();
        let (_, check) = load_shell_config(&path);
        assert_eq!(check.severity, Severity::Warn);
        assert!(!check.value.contains("SUPER_SECRET"));
        assert!(!check.hint.unwrap().contains("SUPER_SECRET"));
        std::fs::write(&path, "[shell]\nlive = false\n").unwrap();
        assert!(!load_shell_config(&path).0.live);
    }
}
