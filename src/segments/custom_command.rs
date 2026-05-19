use std::fmt::Write;
use std::hash::{Hash, Hasher};
use std::io::Read as _;
use std::path::PathBuf;
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

        let output = get_cached_output(command, cache_secs, timeout_ms);
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

fn cache_path(command: &str) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    command.hash(&mut hasher);
    let hash = hasher.finish();
    std::env::temp_dir().join(format!("chevron-cmd-{hash:016x}.cache"))
}

fn get_cached_output(command: &str, cache_secs: u64, timeout_ms: u64) -> Option<String> {
    let path = cache_path(command);

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
    let output = run_with_timeout(command, Duration::from_millis(timeout_ms))?;

    // Write to cache (best-effort)
    let _ = std::fs::write(&path, &output);

    Some(output)
}

fn run_with_timeout(command: &str, timeout: Duration) -> Option<String> {
    let mut child = Command::new("sh")
        .args(["-c", command])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut stdout = child.stdout.take()?;
                let mut buf = String::new();
                stdout.read_to_string(&mut buf).ok()?;
                let trimmed = buf.trim().to_string();
                return if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                };
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(POLL_SLEEP_MS));
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
