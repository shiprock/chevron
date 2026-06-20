// Integration tests are a separate crate from the main binary; the bin's
// crate-root cfg_attr(test, allow(...)) doesn't reach here. Allow the same
// set explicitly so unwrap/expect can be used freely for fixtures.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc
)]

use assert_cmd::prelude::*;
use git2::Repository;
use predicates::prelude::*;
use std::process::Command;
use tempfile::TempDir;

fn cmd() -> Command {
    Command::cargo_bin("chevron").unwrap()
}

/// Init a bare-minimum git repo with one empty commit so git operations work.
fn init_repo(dir: &std::path::Path) {
    let repo = Repository::init(dir).unwrap();
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Test").unwrap();
    config.set_str("user.email", "test@test.com").unwrap();
    let sig = repo.signature().unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
}

// ── dispatch ────────────────────────────────────────────────────────────────

#[test]
fn no_args_exits_failure() {
    cmd().assert().failure();
}

#[test]
fn unknown_subcommand_exits_failure() {
    cmd().arg("bogus").assert().failure();
}

// ── path ────────────────────────────────────────────────────────────────────

#[test]
fn path_home_dir_shows_tilde() {
    let tmp = TempDir::new().unwrap();
    // Canonicalize because macOS /var/folders is a symlink to /private/var/folders,
    // so current_dir() resolves to the real path while TempDir reports the symlink path.
    let real = tmp.path().canonicalize().unwrap();
    cmd()
        .arg("path")
        .current_dir(&real)
        .env("HOME", &real)
        .assert()
        .success()
        .stdout(predicate::str::contains("~"));
}

#[test]
fn path_non_home_dir_no_tilde() {
    let tmp = TempDir::new().unwrap();
    cmd()
        .arg("path")
        .current_dir(tmp.path())
        .env("HOME", "/nonexistent")
        .assert()
        .success()
        .stdout(predicate::str::contains("~").not());
}

#[test]
fn path_with_max_dir_size_arg() {
    let tmp = TempDir::new().unwrap();
    cmd()
        .args(["path", "5"])
        .current_dir(tmp.path())
        .env("HOME", "/nonexistent")
        .assert()
        .success();
}

// ── git ─────────────────────────────────────────────────────────────────────

#[test]
fn git_not_in_repo_succeeds() {
    let tmp = TempDir::new().unwrap();
    cmd().arg("git").current_dir(tmp.path()).assert().success();
}

#[test]
fn git_clean_repo_shows_branch_icon() {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    cmd()
        .arg("git")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\u{E0A0}")); // BRANCH_ICON
}

#[test]
fn git_dirty_repo_shows_dirty_indicator() {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    std::fs::write(tmp.path().join("dirty.txt"), "x").unwrap();
    // Untracked file shows the pink bar and a `+` count indicator.
    cmd()
        .arg("git")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("+"));
}

// ── nix-shell ────────────────────────────────────────────────────────────────

#[test]
fn nix_shell_unset_produces_no_output() {
    cmd()
        .arg("nix-shell")
        .env_remove("IN_NIX_SHELL")
        .assert()
        .success()
        .stdout("");
}

#[test]
fn nix_shell_set_shows_snowflake_and_label() {
    cmd()
        .arg("nix-shell")
        .env("IN_NIX_SHELL", "impure")
        .assert()
        .success()
        .stdout(predicate::str::contains("nix"))
        .stdout(predicate::str::contains("❄"));
}

// ── aws ──────────────────────────────────────────────────────────────────────

#[test]
fn aws_unset_produces_no_output() {
    cmd()
        .arg("aws")
        .env_remove("AWS_PROFILE")
        .assert()
        .success()
        .stdout("");
}

#[test]
fn aws_set_shows_profile_name() {
    cmd()
        .arg("aws")
        .env("AWS_PROFILE", "prod-admin")
        .assert()
        .success()
        .stdout(predicate::str::contains("prod-admin"));
}

// ── prompt ───────────────────────────────────────────────────────────────────

#[test]
fn prompt_succeeds_with_all_args() {
    let tmp = TempDir::new().unwrap();
    cmd()
        .args(["prompt", "20", "0", "0", "0"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .env("USER", "testuser")
        .env_remove("IN_NIX_SHELL")
        .env_remove("AWS_PROFILE")
        .env_remove("VIRTUAL_ENV")
        .env_remove("TMUX")
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn prompt_defaults_when_args_omitted() {
    let tmp = TempDir::new().unwrap();
    cmd()
        .arg("prompt")
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .env("USER", "testuser")
        .env_remove("IN_NIX_SHELL")
        .env_remove("AWS_PROFILE")
        .env_remove("VIRTUAL_ENV")
        .env_remove("TMUX")
        .assert()
        .success();
}

#[test]
fn prompt_nonzero_exit_includes_code_in_output() {
    let tmp = TempDir::new().unwrap();
    // exit code 127 should appear in the error badge
    cmd()
        .args(["prompt", "20", "127", "0", "0"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .env("USER", "testuser")
        .env_remove("IN_NIX_SHELL")
        .env_remove("AWS_PROFILE")
        .env_remove("VIRTUAL_ENV")
        .env_remove("TMUX")
        .assert()
        .success()
        .stdout(predicate::str::contains("127"));
}

#[test]
fn prompt_in_tmux_produces_two_lines() {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    let output = cmd()
        .args(["prompt", "20", "0", "0", "0"])
        .current_dir(tmp.path())
        .env("HOME", "/nonexistent")
        .env("USER", "testuser")
        .env("TMUX", "/tmp/tmux-1000/default,12345,0")
        .env_remove("IN_NIX_SHELL")
        .env_remove("AWS_PROFILE")
        .env_remove("VIRTUAL_ENV")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains('\n'),
        "tmux mode should produce two lines: {text:?}"
    );
}

#[test]
fn prompt_writes_cache_file_when_env_set() {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    let cwd = tmp.path().canonicalize().unwrap();
    let cache = tmp.path().join("cache").join("last-prompt");

    let output = cmd()
        .args(["prompt", "20", "0", "0", "0"])
        .current_dir(&cwd)
        .env("HOME", "/nonexistent")
        .env("CHEVRON_CACHE_FILE", &cache)
        .env_remove("IN_NIX_SHELL")
        .env_remove("AWS_PROFILE")
        .env_remove("VIRTUAL_ENV")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let printed = String::from_utf8_lossy(&output).to_string();

    assert!(cache.exists(), "cache file should be created");
    let cached = std::fs::read_to_string(&cache).unwrap();
    let (cached_cwd, cached_body) = cached.split_once('\n').unwrap();
    assert_eq!(
        cached_cwd,
        cwd.to_string_lossy(),
        "first line of cache must be the cwd at render time"
    );
    assert_eq!(
        cached_body, printed,
        "cache body must byte-match the prompt that was printed"
    );
}

#[test]
fn prompt_cache_write_is_atomic_via_tmp_rename() {
    // Verify the .tmp staging file doesn't linger after a successful write.
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    let cache = tmp.path().join("cache.dat");

    cmd()
        .args(["prompt", "20", "0", "0", "0"])
        .current_dir(tmp.path())
        .env("CHEVRON_CACHE_FILE", &cache)
        .env_remove("IN_NIX_SHELL")
        .assert()
        .success();

    assert!(cache.exists());
    assert!(
        !tmp.path().join("cache.tmp").exists(),
        ".tmp staging file should have been renamed"
    );
}

#[test]
fn prompt_works_without_cache_env_var() {
    // Sanity: with CHEVRON_CACHE_FILE unset, prompt rendering is unaffected.
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    cmd()
        .args(["prompt", "20", "0", "0", "0"])
        .current_dir(tmp.path())
        .env_remove("CHEVRON_CACHE_FILE")
        .env_remove("IN_NIX_SHELL")
        .assert()
        .success();
}

// ── tmux-title ───────────────────────────────────────────────────────────────

#[test]
fn tmux_title_home_dir_shows_tilde() {
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().canonicalize().unwrap();
    cmd()
        .arg("tmux-title")
        .current_dir(&real)
        .env("HOME", &real)
        .assert()
        .success()
        .stdout(predicate::str::contains("~"));
}

#[test]
fn tmux_title_non_repo_shows_folder_emoji() {
    let tmp = TempDir::new().unwrap();
    cmd()
        .arg("tmux-title")
        .current_dir(tmp.path())
        .env("HOME", "/nonexistent")
        .assert()
        .success()
        .stdout(predicate::str::contains("\u{1F4C1}")); // 📁
}

#[test]
fn tmux_title_clean_repo_shows_branch_icon() {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    cmd()
        .arg("tmux-title")
        .current_dir(tmp.path())
        .env("HOME", "/nonexistent")
        .assert()
        .success()
        .stdout(predicate::str::contains("\u{E0A0}")); // BRANCH_ICON
}

// ── init ─────────────────────────────────────────────────────────────────────

#[test]
fn init_zsh_outputs_hook_registration() {
    cmd()
        .args(["init", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("add-zsh-hook"))
        .stdout(predicate::str::contains("chevron prompt"));
}

#[test]
fn init_without_shell_arg_exits_failure() {
    cmd().arg("init").assert().failure();
}

#[test]
fn init_bash_outputs_prompt_command() {
    cmd()
        .args(["init", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PROMPT_COMMAND"))
        .stdout(predicate::str::contains("chevron prompt"));
}

#[test]
fn init_fish_outputs_fish_prompt() {
    cmd()
        .args(["init", "fish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fish_prompt"))
        .stdout(predicate::str::contains("chevron prompt"));
}

#[test]
fn init_unsupported_shell_exits_failure() {
    cmd().args(["init", "tcsh"]).assert().failure();
}

// ── status ───────────────────────────────────────────────────────────────────

#[test]
fn status_in_repo_shows_commits() {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    cmd()
        .arg("status")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Recent commits"))
        .stdout(predicate::str::contains("init"));
}

#[test]
fn status_not_in_repo_exits_failure() {
    let tmp = TempDir::new().unwrap();
    cmd()
        .arg("status")
        .current_dir(tmp.path())
        .assert()
        .failure();
}

// ── health ───────────────────────────────────────────────────────────────────

#[test]
fn health_fast_no_color_emits_core_checks() {
    cmd()
        .args(["health", "--fast", "--no-color"])
        .assert()
        // exit can be 0/1/2 depending on machine state; just confirm it ran
        .stdout(predicate::str::contains("System Health Report"))
        .stdout(predicate::str::contains("System Load"))
        .stdout(predicate::str::contains("Memory Usage"))
        .stdout(predicate::str::contains("Disk Usage"));
}

#[test]
fn health_unknown_flag_exits_two() {
    cmd().args(["health", "--bogus"]).assert().code(2);
}

#[test]
fn health_help_exits_zero() {
    cmd().args(["health", "--help"]).assert().code(0);
}

#[test]
fn health_check_single_load_succeeds() {
    cmd()
        .args(["health", "--check", "load", "--no-color"])
        .assert()
        .stdout(predicate::str::contains("System Load"));
}

#[test]
fn health_check_value_only_prints_just_value() {
    let output = cmd()
        .args(["health", "--check", "load", "--value"])
        .assert()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&output);
    // Should be a single line: "<load> (<per-core> per core)\n"
    assert!(text.contains("per core"), "unexpected: {text:?}");
    assert!(!text.contains("System Load"), "label leaked: {text:?}");
    assert!(!text.contains('\x1b'), "ANSI leaked: {text:?}");
}

#[test]
fn health_check_unknown_name_exits_two() {
    cmd().args(["health", "--check", "nosuch"]).assert().code(2);
}

#[test]
fn health_value_without_check_errors() {
    cmd().args(["health", "--value"]).assert().code(2);
}

#[test]
fn health_json_full_has_checks_wrapper() {
    cmd()
        .args(["health", "--fast", "--json"])
        .assert()
        .stdout(predicate::str::starts_with("{\"checks\":["))
        .stdout(predicate::str::contains("\"name\":\"load\""))
        .stdout(predicate::str::contains("\"severity\":"));
}

#[test]
fn health_check_json_emits_bare_object() {
    cmd()
        .args(["health", "--check", "load", "--json"])
        .assert()
        .stdout(predicate::str::starts_with("{\"name\":\"load\""))
        .stdout(predicate::str::contains("\"severity\":"));
}

#[test]
fn health_config_tight_memory_threshold_triggers_critical() {
    // Memory is normally well under 95%; with a 1%/2% threshold any real
    // machine will land at critical.
    let tmp = TempDir::new().unwrap();
    let cfg_path = tmp.path().join("chevron.toml");
    std::fs::write(
        &cfg_path,
        "[health.thresholds]\nmemory_warn = 1\nmemory_critical = 2\n",
    )
    .unwrap();
    cmd()
        .args(["health", "--check", "memory", "--json"])
        .env("CHEVRON_CONFIG", &cfg_path)
        .assert()
        .stdout(predicate::str::contains("\"severity\":\"critical\""));
}

#[test]
fn health_config_disabled_check_omitted_from_report() {
    let tmp = TempDir::new().unwrap();
    let cfg_path = tmp.path().join("chevron.toml");
    std::fs::write(&cfg_path, "[health]\ndisabled = [\"memory\"]\n").unwrap();
    cmd()
        .args(["health", "--fast", "--json"])
        .env("CHEVRON_CONFIG", &cfg_path)
        .assert()
        .stdout(predicate::str::contains("\"name\":\"memory\"").not())
        .stdout(predicate::str::contains("\"name\":\"load\""));
}

#[test]
fn health_config_disabled_check_still_runs_in_single_check_mode() {
    let tmp = TempDir::new().unwrap();
    let cfg_path = tmp.path().join("chevron.toml");
    std::fs::write(&cfg_path, "[health]\ndisabled = [\"memory\"]\n").unwrap();
    cmd()
        .args(["health", "--check", "memory", "--json"])
        .env("CHEVRON_CONFIG", &cfg_path)
        .assert()
        .stdout(predicate::str::contains("\"name\":\"memory\""));
}

// ── weather ──────────────────────────────────────────────────────────────────

#[cfg(feature = "weather")]
#[test]
fn weather_help_lists_all_flags() {
    cmd()
        .args(["weather", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--lat"))
        .stdout(predicate::str::contains("--lon"))
        .stdout(predicate::str::contains("--provider"))
        .stdout(predicate::str::contains("--units"))
        .stdout(predicate::str::contains("--cache-ttl"))
        .stdout(predicate::str::contains("--no-show-city"))
        .stdout(predicate::str::contains("--no-show-icon"))
        .stdout(predicate::str::contains("--use-nerd-font"))
        .stdout(predicate::str::contains("CHEVRON_WEATHER_"));
}

#[cfg(feature = "weather")]
#[test]
fn weather_bad_flag_is_error_silent() {
    // Any parse error MUST NOT crash. We still exit 0 and print nothing
    // harmful to stdout.
    let tmp = TempDir::new().unwrap();
    cmd()
        .args(["weather", "--bogus-flag"])
        .env("XDG_CACHE_HOME", tmp.path())
        .env("CHEVRON_WEATHER_LOCATION_CMD", "false") // avoid IP geolocation
        .assert()
        .success();
}

#[cfg(feature = "weather")]
#[test]
fn weather_bad_lat_is_error_silent() {
    let tmp = TempDir::new().unwrap();
    cmd()
        .args(["weather", "--lat", "not-a-number"])
        .env("XDG_CACHE_HOME", tmp.path())
        .env("CHEVRON_WEATHER_LOCATION_CMD", "false") // avoid IP geolocation
        .assert()
        .success();
}

#[cfg(feature = "weather")]
#[test]
fn weather_location_cmd_failure_is_error_silent() {
    // location cmd fails, IP geolocation would be next but we can't rely
    // on network in CI. The binary must still exit 0 regardless.
    let tmp = TempDir::new().unwrap();
    cmd()
        .args(["weather", "--location-cmd", "false"])
        .env("XDG_CACHE_HOME", tmp.path())
        .assert()
        .success();
}

// ── event (chevron-1yn Phase 1) ──────────────────────────────────────────────

#[test]
fn event_new_session_prints_ulid() {
    // ULIDs are 26 chars of Crockford base32 (uppercase ASCII + digits).
    let out = cmd().args(["event", "new-session"]).output().unwrap();
    assert!(out.status.success(), "exit: {:?}", out.status);
    let id = String::from_utf8(out.stdout).unwrap();
    let id = id.trim();
    assert_eq!(id.len(), 26, "ULID should be 26 chars: {id:?}");
    assert!(
        id.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
        "ULID should be uppercase Crockford base32: {id:?}"
    );
}

#[test]
fn event_cmd_start_prints_ulid_even_without_daemon() {
    // The shell hook depends on stdout being the ULID; a daemon-down
    // failure mustn't take the shell hook down with it.
    let tmp = TempDir::new().unwrap();
    let out = cmd()
        .args(["event", "cmd-start", "sess-abc", "/tmp", "ls"])
        .env("CHEVRON_SOCKET_DIR", tmp.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8(out.stdout).unwrap();
    assert_eq!(id.trim().len(), 26);
}

#[test]
fn event_cmd_end_exits_success_without_daemon() {
    // Same contract: shell precmd doesn't see a failing exit from
    // chevron event cmd-end even when the daemon is missing.
    let tmp = TempDir::new().unwrap();
    cmd()
        .args(["event", "cmd-end", "some-id", "0", "100"])
        .env("CHEVRON_SOCKET_DIR", tmp.path())
        .assert()
        .success();
}

/// RAII guard that kills the daemon subprocess (via the chevron-daemon-stop
/// subcommand) and cleans up the socket dir override on drop. Holds the
/// `Child` so Drop can `wait()` to reap the process — otherwise we leave
/// a zombie until the test binary exits.
struct DaemonGuard {
    socket_dir: TempDir,
    child: Option<std::process::Child>,
}

impl DaemonGuard {
    // clippy doesn't trace that the Child is stashed on `self` and the
    // Drop impl below `try_wait` + `kill` + `wait`s it. Suppress the
    // false positive locally rather than restructuring the guard.
    #[allow(clippy::zombie_processes)]
    fn start() -> Self {
        let dir = TempDir::new().unwrap();
        // Background `chevron daemon serve` with the socket dir override.
        // We can't use spawn() + a Drop that calls kill() because the daemon
        // serve never returns from its accept loop — instead we let it run
        // and shoot it down via the daemon stop subcommand on drop.
        let child = std::process::Command::cargo_bin("chevron")
            .unwrap()
            .args(["daemon", "serve"])
            .env("CHEVRON_SOCKET_DIR", dir.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Wait for the daemon to bind the socket. ~50 ms is enough on
        // typical machines but we poll with a generous budget to handle
        // slow CI runners.
        let sock = dir.path().join("chevrond.sock");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if sock.exists() {
                // Also wait a tick for the listener thread to be accepting.
                std::thread::sleep(std::time::Duration::from_millis(20));
                return Self {
                    socket_dir: dir,
                    child: Some(child),
                };
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("daemon failed to bind socket within timeout");
    }

    fn socket_dir(&self) -> &std::path::Path {
        self.socket_dir.path()
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        // Best-effort graceful shutdown via the daemon-stop subcommand.
        let _ = std::process::Command::cargo_bin("chevron")
            .unwrap()
            .args(["daemon", "stop"])
            .env("CHEVRON_SOCKET_DIR", self.socket_dir.path())
            .output();
        // Reap the child so we don't leave a zombie. If stop didn't
        // exit it cleanly (race, signal mishap), fall back to kill().
        if let Some(mut child) = self.child.take() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                Err(_) => {
                    let _ = child.wait();
                }
            }
        }
    }
}

/// Read a single row, retrying until it materialises. The daemon's
/// state actor persists `CMD_START`/`CMD_END` asynchronously, and over a
/// different connection than the test's STATUS-round-trip flush — so
/// under load a single read can run ahead of the write and see no row
/// (the flush is not a hard cross-connection barrier, chevron-8q7).
/// Poll on `QueryReturnedNoRows` up to a generous deadline instead;
/// each `query_row` is its own WAL read transaction, so every retry sees
/// a fresh snapshot. Pair with a completeness predicate in the query
/// (e.g. `finished_at IS NOT NULL`) to also wait out the intermediate
/// `CMD_START`-only row, whose `CMD_END` columns read back NULL.
fn query_row_eventually<T>(label: &str, mut read: impl FnMut() -> rusqlite::Result<T>) -> T {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match read() {
            Ok(v) => return v,
            Err(rusqlite::Error::QueryReturnedNoRows)
                if std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => panic!("{label}: {e}"),
        }
    }
}

#[test]
fn event_lifecycle_publishes_to_daemon_and_persists_row() {
    // End-to-end smoke test: spawn the real daemon binary, run the real
    // event subcommand, then read the commands.db SQLite file to verify
    // that the row landed with the expected completion fields. This is
    // the integration test that proves the wire layer + state actor +
    // file-backed DB compose correctly under real process boundaries.
    let daemon = DaemonGuard::start();

    let start = cmd()
        .args([
            "event",
            "cmd-start",
            "sess-int",
            "/tmp/some-cwd",
            "cargo",
            "test",
        ])
        .env("CHEVRON_SOCKET_DIR", daemon.socket_dir())
        .output()
        .unwrap();
    assert!(start.status.success());
    let id = String::from_utf8(start.stdout).unwrap();
    let id = id.trim().to_string();
    assert_eq!(id.len(), 26);

    cmd()
        .args(["event", "cmd-end", &id, "0", "250"])
        .env("CHEVRON_SOCKET_DIR", daemon.socket_dir())
        .assert()
        .success();

    // The daemon ACKs lifecycle events as soon as they're queued for
    // the state actor; the actor commits to SQLite asynchronously.
    // Flush by invoking `chevron git` here — that goes through
    // try_query → STATUS → state-actor Get → reply. The actor
    // processes mpsc messages in FIFO order, so once STATUS replies,
    // the earlier CmdStart and CmdEnd have already been committed.
    // More reliable than a fixed-duration sleep under CI load (the
    // original 50ms wait flaked in the pre-push 5x stress run).
    cmd()
        .arg("git")
        .env("CHEVRON_SOCKET_DIR", daemon.socket_dir())
        .assert()
        .success();

    let db_path = daemon.socket_dir().join("commands.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let (session_id, cwd, cmd_text, exit_status, duration_ms): (String, String, String, i64, i64) =
        query_row_eventually("event_lifecycle commands row", || {
            conn.query_row(
                "SELECT session_id, cwd, cmd, exit_status, duration_ms \
                 FROM commands WHERE id = ?1 AND finished_at IS NOT NULL",
                [&id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
        });
    assert_eq!(session_id, "sess-int");
    assert_eq!(cwd, "/tmp/some-cwd");
    // cmd-start joins trailing args with a space, so multi-arg `cargo
    // test` reconstructs to "cargo test" on disk.
    assert_eq!(cmd_text, "cargo test");
    assert_eq!(exit_status, 0);
    assert_eq!(duration_ms, 250);
}

// ── history (chevron-1yn.2) ─────────────────────────────────────────────────

/// Pre-populate a commands.db with rows for the history-CLI tests. We
/// bypass the daemon entirely — the wire-layer integration is already
/// covered by `event_lifecycle_publishes_to_daemon_and_persists_row` —
/// and instead drive direct rusqlite writes so each test gets a known
/// fixture set without spawning processes.
fn seed_commands_db(dir: &std::path::Path) {
    let conn = rusqlite::Connection::open(dir.join("commands.db")).unwrap();
    // Seed the v2 schema directly (matches what production daemons
    // write after chevron-1yn.4 lands). The Phase 4 output_* columns
    // are nullable so existing fixture rows don't need them.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT OR IGNORE INTO meta(key, value) VALUES('schema_version', '2');
         CREATE TABLE IF NOT EXISTS commands (
             id TEXT PRIMARY KEY, session_id TEXT NOT NULL, hostname TEXT NOT NULL,
             cwd TEXT NOT NULL, cmd TEXT NOT NULL,
             started_at INTEGER NOT NULL, finished_at INTEGER,
             duration_ms INTEGER, exit_status INTEGER,
             output_bytes INTEGER,
             output_truncated INTEGER NOT NULL DEFAULT 0);",
    )
    .unwrap();
    // Seed five rows with predictable values. Use absolute milli
    // timestamps so --since-ms tests are deterministic.
    let rows: &[(&str, &str, &str, i64, Option<i64>)] = &[
        (
            "a",
            "/repo",
            "cargo test --features daemon",
            1_716_350_000_000,
            Some(0),
        ),
        ("b", "/repo", "cargo check", 1_716_350_001_000, Some(1)),
        ("c", "/repo/sub", "git status", 1_716_350_002_000, Some(0)),
        ("d", "/other", "ls -la", 1_716_350_003_000, Some(0)),
        ("e", "/repo", "cargo build", 1_716_350_004_000, None), // still running
    ];
    for (id, cwd, cmdtxt, started, exit) in rows {
        let finished = exit.map(|_| started + 100);
        let dur = exit.map(|_| 100i64);
        conn.execute(
            "INSERT OR REPLACE INTO commands \
             (id, session_id, hostname, cwd, cmd, started_at, finished_at, duration_ms, exit_status) \
             VALUES (?1, 'sess-test', 'host-test', ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, cwd, cmdtxt, started, finished, dur, exit],
        )
        .unwrap();
    }
}

#[test]
fn history_no_db_exits_quietly() {
    let dir = TempDir::new().unwrap();
    // No commands.db exists in this tempdir.
    let out = cmd()
        .arg("history")
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR") // avoid leaking a real db in
        .output()
        .unwrap();
    assert!(out.status.success(), "exit: {:?}", out.status);
    assert!(
        out.stdout.is_empty(),
        "stdout should be empty: {:?}",
        out.stdout
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no commands.db"),
        "stderr should explain missing db: {stderr}"
    );
}

#[test]
fn history_default_prints_all_rows() {
    let dir = TempDir::new().unwrap();
    seed_commands_db(dir.path());
    let out = cmd()
        .arg("history")
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // All 5 rows should appear.
    for cmd_text in [
        "cargo test --features daemon",
        "cargo check",
        "git status",
        "ls -la",
        "cargo build",
    ] {
        assert!(
            stdout.contains(cmd_text),
            "missing {cmd_text:?} in: {stdout}"
        );
    }
    // Default order is oldest-at-top for human format → the LAST line
    // should be the most-recent row.
    let last_line = stdout.lines().last().unwrap_or("");
    assert!(
        last_line.contains("cargo build"),
        "newest row should be at the bottom: {stdout}"
    );
}

#[test]
fn history_grep_filters_substring() {
    let dir = TempDir::new().unwrap();
    seed_commands_db(dir.path());
    let out = cmd()
        .args(["history", "--grep", "cargo"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("cargo test"));
    assert!(stdout.contains("cargo check"));
    assert!(stdout.contains("cargo build"));
    assert!(!stdout.contains("git status"));
    assert!(!stdout.contains("ls -la"));
}

#[test]
fn history_positional_is_grep_shortcut() {
    let dir = TempDir::new().unwrap();
    seed_commands_db(dir.path());
    let out = cmd()
        .args(["history", "git"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("git status"));
    assert!(!stdout.contains("cargo"));
}

#[test]
fn history_failed_excludes_zero_and_running() {
    let dir = TempDir::new().unwrap();
    seed_commands_db(dir.path());
    let out = cmd()
        .args(["history", "--failed"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Only `cargo check` (exit 1) should show.
    assert!(stdout.contains("cargo check"));
    assert!(!stdout.contains("cargo test"));
    assert!(!stdout.contains("cargo build")); // running, NULL exit
    assert!(!stdout.contains("git status"));
}

#[test]
fn history_cwd_filter_exact_match() {
    let dir = TempDir::new().unwrap();
    seed_commands_db(dir.path());
    let out = cmd()
        .args(["history", "--cwd", "/repo"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // /repo matches but /repo/sub doesn't (exact-match, not prefix).
    assert!(stdout.contains("cargo test"));
    assert!(stdout.contains("cargo check"));
    assert!(stdout.contains("cargo build"));
    assert!(!stdout.contains("git status")); // /repo/sub
    assert!(!stdout.contains("ls -la")); // /other
}

#[test]
fn history_json_format_is_valid_ndjson() {
    let dir = TempDir::new().unwrap();
    seed_commands_db(dir.path());
    let out = cmd()
        .args(["history", "--format", "json"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Each line should parse via a minimal shape check: starts with {
    // and ends with }, contains the required keys.
    for line in stdout.lines() {
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "bad NDJSON line: {line:?}"
        );
        for key in ["\"id\":", "\"cwd\":", "\"cmd\":", "\"started_at\":"] {
            assert!(line.contains(key), "missing {key} in {line:?}");
        }
    }
    // NDJSON is newest-first by design (matches SQL ORDER BY).
    let first = stdout.lines().next().unwrap_or("");
    assert!(
        first.contains("cargo build"),
        "newest row should be first: {first}"
    );
}

#[test]
fn history_cmds_format_one_per_line() {
    let dir = TempDir::new().unwrap();
    seed_commands_db(dir.path());
    let out = cmd()
        .args(["history", "--format", "cmds", "--grep", "cargo"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "expected 3 cargo cmds, got {lines:?}");
    assert!(lines.contains(&"cargo build"));
}

#[test]
fn history_unique_collapses_duplicates() {
    let dir = TempDir::new().unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("commands.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT OR IGNORE INTO meta(key, value) VALUES('schema_version', '2');
         CREATE TABLE IF NOT EXISTS commands (
             id TEXT PRIMARY KEY, session_id TEXT NOT NULL, hostname TEXT NOT NULL,
             cwd TEXT NOT NULL, cmd TEXT NOT NULL,
             started_at INTEGER NOT NULL, finished_at INTEGER,
             duration_ms INTEGER, exit_status INTEGER,
             output_bytes INTEGER, output_truncated INTEGER NOT NULL DEFAULT 0);",
    )
    .unwrap();
    // Three `ls` rows, two `ps` rows.
    for (id, cmd_text, started) in [
        ("a", "ls", 1_000),
        ("b", "ls", 2_000),
        ("c", "ls", 3_000),
        ("d", "ps", 4_000),
        ("e", "ps", 5_000),
    ] {
        conn.execute(
            "INSERT INTO commands (id, session_id, hostname, cwd, cmd, started_at, finished_at, duration_ms, exit_status) \
             VALUES (?1, 's', 'h', '/', ?2, ?3, ?3 + 50, 50, 0)",
            rusqlite::params![id, cmd_text, started],
        )
        .unwrap();
    }
    let out = cmd()
        .args(["history", "--unique", "--format", "cmds"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 2, "expected 2 unique cmds, got {lines:?}");
}

#[test]
fn history_since_ms_filters_time_window() {
    let dir = TempDir::new().unwrap();
    seed_commands_db(dir.path());
    let out = cmd()
        .args(["history", "--since-ms", "1716350002500", "--format", "cmds"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Only rows started_at >= 1716350002500 should remain: 'ls -la'
    // (1716350003000) and 'cargo build' (1716350004000).
    let cmds: Vec<&str> = stdout.lines().collect();
    assert_eq!(cmds.len(), 2, "expected 2 cmds in window: {cmds:?}");
    assert!(cmds.contains(&"cargo build"));
    assert!(cmds.contains(&"ls -la"));
}

#[test]
fn history_template_format_substitutes_fields() {
    let dir = TempDir::new().unwrap();
    seed_commands_db(dir.path());
    let out = cmd()
        .args([
            "history",
            "--grep",
            "git",
            "--format",
            "{cmd} :: {cwd} :: {exit}",
        ])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("git status :: /repo/sub :: 0"),
        "template substitution failed: {stdout}"
    );
}

#[test]
fn history_exclude_cwd_drops_rows() {
    let dir = TempDir::new().unwrap();
    seed_commands_db(dir.path());
    let out = cmd()
        .args(["history", "--exclude-cwd", "/repo", "--format", "cmds"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    let cmds: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    // /repo/sub and /other remain; /repo dropped.
    assert!(cmds.contains(&"git status".to_string()));
    assert!(cmds.contains(&"ls -la".to_string()));
    assert!(!cmds.iter().any(|c| c.starts_with("cargo")));
}

#[test]
fn history_reverse_flips_order() {
    let dir = TempDir::new().unwrap();
    seed_commands_db(dir.path());
    let out = cmd()
        .args(["history", "--reverse", "--format", "cmds"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    let cmds: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    // cmds format default order is newest-first; --reverse flips to
    // oldest-first.
    assert_eq!(
        cmds.first().map(String::as_str),
        Some("cargo test --features daemon")
    );
}

#[test]
fn history_help_lists_filters() {
    let out = cmd().args(["history", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for needle in ["--cwd", "--failed", "--since", "--limit", "--format"] {
        assert!(
            stdout.contains(needle),
            "help should list {needle}: {stdout}"
        );
    }
}

#[test]
fn history_bad_arg_exits_two() {
    let out = cmd().args(["history", "--mystery-flag"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn history_conflicting_filters_exit_two() {
    let out = cmd()
        .args(["history", "--failed", "--success"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn history_schema_mismatch_exits_two() {
    let dir = TempDir::new().unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("commands.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO meta(key, value) VALUES('schema_version', '999');",
    )
    .unwrap();
    let out = cmd()
        .arg("history")
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("999"),
        "stderr should mention bad version: {stderr}"
    );
}

use std::io::{BufRead as _, Read};
use std::os::unix::io::AsRawFd;

// ── daemon version check ─────────────────────────────────────────────────────

#[test]
fn daemon_version_no_daemon_prints_cli_info_and_exits_zero() {
    // Without a running daemon, `chevron daemon version` should
    // still print the CLI's own version + "not running" and exit
    // cleanly. Not a failure.
    let dir = TempDir::new().unwrap();
    let out = cmd()
        .args(["daemon", "version"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("chevron cli:"),
        "missing cli line: {stdout}"
    );
    assert!(
        stdout.contains("not running"),
        "missing 'not running': {stdout}"
    );
}

#[test]
fn daemon_version_against_live_daemon_reports_matching_versions() {
    // Against a daemon spawned by the same binary, all three
    // version dimensions (binary, proto, schema) should agree —
    // and no WARNING lines should appear on stderr.
    let daemon = DaemonGuard::start();
    let out = cmd()
        .args(["daemon", "version"])
        .env("CHEVRON_SOCKET_DIR", daemon.socket_dir())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("chevron cli:"));
    assert!(stdout.contains("chevron daemon:"));
    assert!(
        !stderr.contains("WARNING"),
        "matching versions should not warn: {stderr}"
    );
}

// ── capture (chevron-1yn.4 Phase 4) ──────────────────────────────────────────

/// Seed an outputs directory + commands.db so we can test the
/// post-SQL `--grep-output` / `--show-output` paths without spinning
/// up the real `chevron capture` binary (PTY-using; covered in unit
/// tests).
fn seed_outputs(dir: &std::path::Path) {
    seed_commands_db(dir);
    let outputs = dir.join("outputs");
    std::fs::create_dir_all(&outputs).unwrap();
    // Attach an output file to row "b" (cargo check, exit 1) with a
    // color-wrapped error in it.
    std::fs::write(
        outputs.join("b.log"),
        b"checking project...\n\x1b[1;31merror:\x1b[0m unused variable `x`\n",
    )
    .unwrap();
    // Row "d" (ls -la) gets innocuous output, no match for "error".
    std::fs::write(
        outputs.join("d.log"),
        b"total 0\ndrwxr-xr-x 2 mim staff 64\n",
    )
    .unwrap();
    // Mark the rows as having captured output.
    let conn = rusqlite::Connection::open(dir.join("commands.db")).unwrap();
    conn.execute(
        "UPDATE commands SET output_bytes = ?1 WHERE id = 'b'",
        rusqlite::params![
            i64::try_from(std::fs::metadata(outputs.join("b.log")).unwrap().len()).unwrap()
        ],
    )
    .unwrap();
    conn.execute(
        "UPDATE commands SET output_bytes = ?1 WHERE id = 'd'",
        rusqlite::params![
            i64::try_from(std::fs::metadata(outputs.join("d.log")).unwrap().len()).unwrap()
        ],
    )
    .unwrap();
}

#[test]
fn capture_no_args_exits_one_with_usage() {
    let out = cmd().arg("capture").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Usage"), "stderr: {stderr}");
}

#[test]
fn capture_help_exits_zero() {
    let out = cmd().args(["capture", "--help"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn capture_runs_command_and_creates_output_file() {
    // End-to-end: spawn daemon, run `chevron capture echo hi`, verify
    // the output file lands in $SOCKET_DIR/outputs and the DB row
    // records output_bytes.
    let daemon = DaemonGuard::start();
    let out = cmd()
        .args(["capture", "echo", "phase-4-echo-test"])
        .env("CHEVRON_SOCKET_DIR", daemon.socket_dir())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success(), "capture exit: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("phase-4-echo-test"),
        "stdout should still show the echo output: {stdout}"
    );

    // Flush actor before reading the DB (same trick as Phase 1's
    // lifecycle e2e test).
    cmd()
        .arg("git")
        .env("CHEVRON_SOCKET_DIR", daemon.socket_dir())
        .assert()
        .success();

    let db = rusqlite::Connection::open(daemon.socket_dir().join("commands.db")).unwrap();
    let (cmd_text, output_bytes, output_truncated): (String, i64, i64) =
        query_row_eventually("capture commands row", || {
            db.query_row(
                "SELECT cmd, output_bytes, output_truncated FROM commands \
                 WHERE cmd LIKE 'echo phase-4-echo-test%' AND finished_at IS NOT NULL \
                 ORDER BY started_at DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
        });
    assert_eq!(cmd_text, "echo phase-4-echo-test");
    // Echo's output is "phase-4-echo-test\r\n" (18 bytes after PTY
    // adds \r). Allow some slack for terminal noise.
    assert!(
        (17..=32).contains(&output_bytes),
        "unexpected output_bytes: {output_bytes}"
    );
    assert_eq!(output_truncated, 0);

    // Look up the id and verify the file exists.
    let id: String = db
        .query_row(
            "SELECT id FROM commands WHERE cmd = 'echo phase-4-echo-test' ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let log_path = daemon
        .socket_dir()
        .join("outputs")
        .join(format!("{id}.log"));
    let bytes = std::fs::read(&log_path).expect("output file should exist");
    assert!(
        String::from_utf8_lossy(&bytes).contains("phase-4-echo-test"),
        "log content: {:?}",
        String::from_utf8_lossy(&bytes)
    );
}

#[test]
fn history_grep_output_finds_color_wrapped_match() {
    let dir = TempDir::new().unwrap();
    seed_outputs(dir.path());
    let out = cmd()
        .args(["history", "--grep-output", "error", "--format", "cmds"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Only row "b" has "error" in its output (color-wrapped).
    assert!(
        stdout.lines().any(|l| l == "cargo check"),
        "expected `cargo check` row, got: {stdout}"
    );
    // Row "d" has no "error" in its captured output.
    assert!(!stdout.contains("ls -la"));
}

#[test]
fn history_grep_output_skips_rows_with_no_capture() {
    // Only rows "b" and "d" have output files; the other seed rows
    // don't. --grep-output for a pattern not in either file returns
    // nothing.
    let dir = TempDir::new().unwrap();
    seed_outputs(dir.path());
    let out = cmd()
        .args([
            "history",
            "--grep-output",
            "nope-not-here",
            "--format",
            "cmds",
        ])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "expected empty stdout, got: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn history_show_output_dumps_captured_bytes() {
    let dir = TempDir::new().unwrap();
    seed_outputs(dir.path());
    let out = cmd()
        .args(["history", "--show-output", "b"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    // ANSI escapes preserved (we only strip for SEARCH, not display).
    let bytes = out.stdout;
    assert!(bytes.windows(7).any(|w| w == b"\x1b[1;31m"));
    assert!(String::from_utf8_lossy(&bytes).contains("error:"));
}

#[test]
fn history_show_output_missing_id_exits_one() {
    let dir = TempDir::new().unwrap();
    seed_outputs(dir.path());
    let out = cmd()
        .args(["history", "--show-output", "no-such-id"])
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no captured output"), "stderr: {stderr}");
}

// ── subscribe (chevron-1yn.3 Phase 3) ────────────────────────────────────────

#[test]
fn subscribe_no_daemon_exits_two() {
    let dir = TempDir::new().unwrap();
    let out = cmd()
        .arg("subscribe")
        .env("CHEVRON_SOCKET_DIR", dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("daemon not running"),
        "stderr should explain: {stderr}"
    );
}

#[test]
fn subscribe_help_exits_one_with_usage() {
    // The current arg parser treats `--help` as an error and prints
    // usage to stderr. That's fine for the helper subcommand — users
    // don't typically discover it via `--help`; the shell init wires
    // it up automatically.
    let out = cmd().args(["subscribe", "--help"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--cwd"),
        "usage should mention --cwd: {stderr}"
    );
}

#[test]
fn subscribe_bad_arg_exits_one() {
    let out = cmd().args(["subscribe", "--mystery"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn subscribe_receives_git_event_from_real_daemon() {
    // End-to-end: spawn a real daemon, spawn `chevron subscribe` as
    // a child capturing its stdout, drive an FS event by writing
    // inside the watched gitdir via a STATUS request first (to
    // register the watch) and then a synthetic file touch.
    //
    // We test the SHELL-OBSERVED contract: lines of `EVENT type=git
    // cwd=…` appear on the subscriber's stdout when a watched
    // gitdir changes. This is the linchpin of Phase 3 — if this
    // test passes, the shell can wire up reset-prompt with confidence.
    let daemon = DaemonGuard::start();

    // Make a real git repo in a tempdir so the daemon registers an
    // FS watch when we STATUS it.
    let repo_dir = TempDir::new().unwrap();
    init_repo(repo_dir.path());
    let repo_path = repo_dir.path().canonicalize().unwrap();

    // STATUS once to make the daemon discover + watch the repo.
    // We do this through `chevron git` (which calls status_for_cwd).
    cmd()
        .arg("git")
        .current_dir(&repo_path)
        .env("CHEVRON_SOCKET_DIR", daemon.socket_dir())
        .assert()
        .success();

    // Spawn the subscriber as a child; capture its stdout.
    let mut sub_child = std::process::Command::cargo_bin("chevron")
        .unwrap()
        .arg("subscribe")
        .env("CHEVRON_SOCKET_DIR", daemon.socket_dir())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let sub_stdout = sub_child.stdout.take().unwrap();

    // Spawn the stdout reader first so no event can slip through the
    // window between firing the trigger and starting to read. It
    // relays the first line the subscriber prints.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(sub_stdout);
        let mut line = String::new();
        if reader.read_line(&mut line).is_ok() {
            let _ = tx.send(line);
        }
    });

    // Drive the gitdir event until the subscriber relays it. A single
    // fixed 150 ms sleep before one touch raced the subscriber's
    // SUBSCRIBE registration under load: an event fired before the
    // watch went live is dropped, and the test then waited forever for
    // a second that never came. Retrigger on a short cadence until one
    // lands, bounded by a generous deadline. Rewriting HEAD's own bytes
    // is an idempotent way to raise a notify event each round.
    let head_path = repo_path.join(".git").join("HEAD");
    let head_bytes = std::fs::read(&head_path).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let line = loop {
        std::fs::write(&head_path, &head_bytes).unwrap();
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(line) => break line,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => assert!(
                std::time::Instant::now() < deadline,
                "expected EVENT line within timeout"
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("subscriber reader thread exited without an EVENT line")
            }
        }
    };
    assert!(
        line.starts_with("EVENT type=git"),
        "unexpected line: {line:?}"
    );
    assert!(line.contains("cwd="), "EVENT missing cwd: {line:?}");

    // Clean up the subscriber. Killing it triggers the daemon's
    // disconnected-subscriber pruning on next broadcast.
    let _ = sub_child.kill();
    let _ = sub_child.wait();
}

#[test]
fn subscribe_filters_out_ping_heartbeats() {
    // PING lines from the daemon should be consumed silently by the
    // subscriber helper. The shell's zle -F handler wakes on stdout
    // readability, so a PING that leaked through would cause a
    // spurious prompt redraw every 60s.
    //
    // We test by spawning a daemon with a SHORT heartbeat (50ms via
    // a test-only entry point would be ideal, but we don't expose
    // that). Instead we verify the indirect contract: when no real
    // events fire, the subscriber's stdout stays empty.
    let daemon = DaemonGuard::start();
    let mut sub_child = std::process::Command::cargo_bin("chevron")
        .unwrap()
        .arg("subscribe")
        .env("CHEVRON_SOCKET_DIR", daemon.socket_dir())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let sub_stdout = sub_child.stdout.take().unwrap();

    // 100ms is enough to confirm no immediate spurious output (the
    // daemon's default 60s heartbeat is far longer than this). We're
    // testing that the subscriber doesn't echo its own PING noise to
    // stdout — not the heartbeat cadence itself (covered in
    // unit-level relay_loop tests).
    let fd = sub_stdout.as_raw_fd();
    // SAFETY: fcntl with F_GETFL / F_SETFL on a stdout fd we own is
    // documented-safe; the value of `flags | O_NONBLOCK` is a valid
    // file-status-flags bitmask.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    let mut buf = [0u8; 256];
    let mut reader = sub_stdout;
    let n = reader.read(&mut buf).unwrap_or(0);
    assert_eq!(
        n,
        0,
        "subscriber should not emit anything when no events fire, got {:?}",
        std::str::from_utf8(&buf[..n]).unwrap_or("<binary>")
    );

    let _ = sub_child.kill();
    let _ = sub_child.wait();
}
