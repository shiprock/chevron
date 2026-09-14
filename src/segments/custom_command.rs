use std::fmt::Write;
use std::hash::{Hash, Hasher};
use std::io::Read as _;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::color::{arrow, fg};
use crate::segments::prompt::PromptContext;
use crate::segments::registry::{Segment, SegmentOutput};

const DEFAULT_BG: u8 = 240;
const DEFAULT_FG: u8 = 15;
const DEFAULT_CACHE_SECS: u64 = 30;
// Default timeout: 100ms. On cache miss, this segment blocks the prompt render
// synchronously for up to `timeout_ms`. Keep the default tight so a poorly
// behaved user command can't jank the prompt by more than ~100ms. Users with
// known-slow commands should explicitly raise `timeout_ms` in their config and
// also bump `cache_secs` so the slow path is taken less often.
const DEFAULT_TIMEOUT_MS: u64 = 100;
// Polling cadence inside run_with_timeout. Lower = snappier return for fast
// commands, at the cost of slightly more scheduler wakeups.
const POLL_SLEEP_MS: u64 = 5;

pub struct CustomCommandSegment;

impl Segment for CustomCommandSegment {
    fn name(&self) -> &'static str {
        "custom_command"
    }

    fn render(&self, ctx: &mut PromptContext, from_bg: Option<u8>) -> SegmentOutput {
        let empty = SegmentOutput {
            text: String::new(),
            end_bg: from_bg,
        };

        let seg_config = ctx.config.segment.get("custom_command");
        let Some(command) = seg_config.and_then(|c| c.command.as_deref()) else {
            return empty;
        };

        let cache_secs = seg_config
            .and_then(|c| c.cache_secs)
            .unwrap_or(DEFAULT_CACHE_SECS);
        let timeout_ms = seg_config
            .and_then(|c| c.timeout_ms)
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        let background = seg_config.and_then(|c| c.bg).unwrap_or(DEFAULT_BG);
        let foreground = seg_config.and_then(|c| c.fg).unwrap_or(DEFAULT_FG);

        let output = get_cached_output_in(command, cache_secs, timeout_ms, Path::new(&ctx.pwd));
        let Some(output) = output else {
            return empty;
        };

        let mut out = String::with_capacity(128);
        let _ = write!(
            out,
            "{} {}{output} ",
            arrow(from_bg, background),
            fg(foreground),
        );
        SegmentOutput {
            text: out,
            end_bg: Some(background),
        }
    }
}

fn cache_path_in(command: &str, cwd: &Path) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    command.hash(&mut hasher);
    cwd.canonicalize()
        .unwrap_or_else(|_| cwd.to_path_buf())
        .hash(&mut hasher);
    let hash = hasher.finish();
    std::env::temp_dir().join(format!("chevron-cmd-{hash:016x}.cache"))
}

fn get_cached_output_in(
    command: &str,
    cache_secs: u64,
    timeout_ms: u64,
    cwd: &Path,
) -> Option<String> {
    let path = cache_path_in(command, cwd);

    // Check cache freshness
    if let Ok(meta) = std::fs::metadata(&path)
        && let Ok(modified) = meta.modified()
    {
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::MAX);
        if age < Duration::from_secs(cache_secs)
            && let Ok(contents) = std::fs::read_to_string(&path)
        {
            let trimmed = contents.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    // Cache miss: run the command
    let output = run_in_with_timeout(command, Duration::from_millis(timeout_ms), cwd)?;

    // Write to cache (best-effort)
    let _ = std::fs::write(&path, &output);

    Some(output)
}

// Bound memory independently of the time budget; oversized output is omitted.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

fn run_in_with_timeout(command: &str, timeout: Duration, cwd: &Path) -> Option<String> {
    let start = Instant::now();
    let mut child = Command::new("sh")
        .args(["-c", command])
        .current_dir(cwd)
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut complete = false;
    let result = (|| {
        let mut stdout = child.stdout.take()?;
        let fd = stdout.as_raw_fd();
        // SAFETY: fd is owned by the live ChildStdout; fcntl does not retain it.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        // SAFETY: the same owned descriptor remains open throughout this call.
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return None;
        }
        let mut output = Vec::new();
        let mut eof = false;
        loop {
            // Check even while a producer continuously writes to the pipe.
            if start.elapsed() >= timeout {
                return None;
            }
            let mut chunk = [0_u8; 8192];
            match stdout.read(&mut chunk) {
                Ok(0) => eof = true,
                Ok(n) => {
                    if output.len() + n > MAX_OUTPUT_BYTES {
                        return None;
                    }
                    output.extend_from_slice(&chunk[..n]);
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return None,
            }
            if let Some(status) = child.try_wait().ok()? {
                if !status.success() {
                    return None;
                }
                if eof {
                    complete = true;
                    let text = String::from_utf8(output).ok()?.trim().to_owned();
                    return (!text.is_empty()).then_some(text);
                }
            }
            std::thread::sleep(
                Duration::from_millis(POLL_SLEEP_MS).min(timeout.saturating_sub(start.elapsed())),
            );
        }
    })();
    if !complete {
        // The shell and descendants share this newly created process group.
        // Kill the group so a background writer cannot outlive a failed render.
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: positive child pid identifies our dedicated process group.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cache_path(command: &str) -> PathBuf {
        cache_path_in(command, &std::env::current_dir().unwrap())
    }
    fn get_cached_output(command: &str, secs: u64, ms: u64) -> Option<String> {
        get_cached_output_in(command, secs, ms, &std::env::current_dir().unwrap())
    }
    fn run_with_timeout(command: &str, timeout: Duration) -> Option<String> {
        run_in_with_timeout(command, timeout, &std::env::current_dir().unwrap())
    }

    #[test]
    fn cache_path_is_deterministic() {
        let a = cache_path("echo hello");
        let b = cache_path("echo hello");
        assert_eq!(a, b);
    }

    #[test]
    fn cache_path_differs_for_different_commands() {
        let a = cache_path("echo hello");
        let b = cache_path("echo world");
        assert_ne!(a, b);
    }

    #[test]
    fn run_simple_command() {
        let output = run_with_timeout("echo hello", Duration::from_secs(5));
        assert_eq!(output.unwrap(), "hello");
    }

    #[test]
    fn run_failing_command_returns_none() {
        let output = run_with_timeout("false", Duration::from_secs(5));
        assert!(output.is_none());
    }

    #[test]
    fn run_empty_output_returns_none() {
        // `true` is portable; `echo -n ''` was flaky on GitHub's macOS runner
        // (some shells print "-n" as a literal when the -n flag is followed by
        // an empty argument). `true` always succeeds and writes nothing.
        let output = run_with_timeout("true", Duration::from_secs(5));
        assert!(output.is_none());
    }

    #[test]
    fn run_timeout_returns_none() {
        let output = run_with_timeout("sleep 10", Duration::from_millis(50));
        assert!(output.is_none());
    }

    #[test]
    fn cached_output_roundtrip() {
        let output = get_cached_output("echo cached-test", 60, 5000);
        assert_eq!(output.unwrap(), "cached-test");

        // Second call should hit cache
        let output2 = get_cached_output("echo cached-test", 60, 5000);
        assert_eq!(output2.unwrap(), "cached-test");

        // Clean up
        let _ = std::fs::remove_file(cache_path("echo cached-test"));
    }
    #[test]
    fn regression_timeout_covers_descendant_stdout() {
        let start = Instant::now();
        let result = run_with_timeout("sleep 1 & printf READY", Duration::from_millis(20));
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "stdout exceeded deadline: {:?}",
            start.elapsed()
        );
        assert!(result.is_none());
    }

    #[test]
    fn regression_output_larger_than_pipe_is_drained() {
        let result = run_with_timeout(
            "head -c 100000 /dev/zero | tr '\\000' X",
            Duration::from_secs(2),
        );
        let output = result.expect("stdout must be drained while the command runs");
        assert_eq!(output.len(), 100_000);
        assert!(output.bytes().all(|b| b == b'X'));
    }

    #[test]
    fn regression_cache_worker() {
        let Ok(output_path) = std::env::var("CHEVRON_TEST_CACHE_RESULT") else {
            return;
        };
        let command = std::env::var("CHEVRON_TEST_COMMAND").unwrap();
        let result = get_cached_output(&command, 30, 2000).unwrap();
        std::fs::write(output_path, result).unwrap();
    }

    #[test]
    fn regression_cache_does_not_cross_directories() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["repo-A", "repo-B"] {
            let cwd = tmp.path().join(name);
            std::fs::create_dir(&cwd).unwrap();
            let output = tmp.path().join(format!("{name}.result"));
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "segments::custom_command::tests::regression_cache_worker",
                ])
                .current_dir(&cwd)
                .env("TMPDIR", tmp.path())
                .env(
                    "CHEVRON_TEST_COMMAND",
                    format!("pwd # {}", tmp.path().display()),
                )
                .env("CHEVRON_TEST_CACHE_RESULT", &output)
                .output()
                .unwrap();
            assert!(status.status.success(), "{status:?}");
            assert_eq!(
                std::fs::read_to_string(output).unwrap(),
                cwd.canonicalize().unwrap().to_string_lossy()
            );
        }
    }
    #[test]
    fn oversized_command_output_is_bounded() {
        let result = run_with_timeout(
            "head -c 300000 /dev/zero | tr '\\000' X",
            Duration::from_secs(2),
        );
        assert!(result.is_none());
    }

    #[test]
    fn command_runs_in_requested_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run_in_with_timeout("pwd", Duration::from_secs(2), tmp.path()).unwrap();
        assert_eq!(result, tmp.path().canonicalize().unwrap().to_string_lossy());
    }
}
