#![cfg(target_os = "macos")]

use std::process::Command;
use std::time::Duration;

use super::cache;
use super::check::Check;

// ── CPU temperature ─────────────────────────────────────────────────────────

/// Parse the leading number from `osx-cpu-temp` output (e.g. "56.3°C\n" → 56.3).
fn parse_cpu_temp(raw: &str) -> Option<f64> {
    let trimmed = raw.trim_start();
    let num: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if num.is_empty() {
        return None;
    }
    num.parse().ok()
}

pub fn cpu_temp() -> Check {
    let Ok(output) = Command::new("osx-cpu-temp").output() else {
        return Check::unknown("cpu_temp", "CPU Temperature", "N/A (install osx-cpu-temp)");
    };
    if !output.status.success() {
        return Check::unknown("cpu_temp", "CPU Temperature", "N/A");
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let Some(temp) = parse_cpu_temp(&raw) else {
        return Check::unknown("cpu_temp", "CPU Temperature", raw.trim().to_string());
    };
    let value = format!("{temp:.1}°C");
    if temp > 90.0 {
        Check::critical(
            "cpu_temp",
            "CPU Temperature",
            value,
            "check cooling: CPU is critically hot",
        )
    } else if temp > 80.0 {
        Check::warn(
            "cpu_temp",
            "CPU Temperature",
            value,
            "CPU temperature elevated",
        )
    } else {
        Check::ok("cpu_temp", "CPU Temperature", value)
    }
}

// ── Disk SMART status ───────────────────────────────────────────────────────

/// Pull the `SMART Status:` value from `diskutil info disk0` output.
fn parse_smart_status(raw: &str) -> Option<String> {
    raw.lines().find_map(|l| {
        let l = l.trim();
        l.strip_prefix("SMART Status:")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

pub fn disk_health() -> Check {
    const KEY: &str = "disk_health";
    const TTL: Duration = Duration::from_secs(3_600); // 1h

    let status = if let Some(cached) = cache::read(KEY, TTL) {
        cached
    } else {
        let Ok(output) = Command::new("diskutil").args(["info", "disk0"]).output() else {
            return Check::unknown("disk_health", "Disk Health", "N/A");
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let Some(status) = parse_smart_status(&text) else {
            return Check::unknown("disk_health", "Disk Health", "unknown");
        };
        cache::write(KEY, &status);
        status
    };

    if status.eq_ignore_ascii_case("Verified") {
        Check::ok("disk_health", "Disk Health", status)
    } else {
        Check::critical(
            "disk_health",
            "Disk Health",
            status,
            "back up data: SMART status is non-verified",
        )
    }
}

// ── Application firewall ────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum FirewallState {
    Enabled,
    Disabled,
    Unknown,
}

/// Parse `socketfilterfw --getglobalstate` output.
/// Typical messages: "Firewall is enabled." / "Firewall is disabled."
fn parse_firewall_state(raw: &str) -> FirewallState {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("disabled") {
        FirewallState::Disabled
    } else if lower.contains("enabled") {
        FirewallState::Enabled
    } else {
        FirewallState::Unknown
    }
}

pub fn firewall() -> Check {
    let Ok(output) = Command::new("/usr/libexec/ApplicationFirewall/socketfilterfw")
        .arg("--getglobalstate")
        .output()
    else {
        return Check::unknown("firewall", "Firewall", "N/A");
    };
    let text = String::from_utf8_lossy(&output.stdout);
    match parse_firewall_state(&text) {
        FirewallState::Enabled => Check::ok("firewall", "Firewall", "Enabled"),
        FirewallState::Disabled => Check::warn(
            "firewall",
            "Firewall",
            "Disabled",
            "enable the application firewall for better security",
        ),
        FirewallState::Unknown => Check::unknown("firewall", "Firewall", text.trim().to_string()),
    }
}

// ── Software updates ────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum UpdateState {
    UpToDate,
    Pending(usize),
    Unknown,
}

/// Parse combined stdout+stderr of `softwareupdate -l`.
/// macOS prints "No new software available." on the no-updates path; otherwise
/// the lines starting with "* Label:" enumerate available packages.
fn parse_updates(raw: &str) -> UpdateState {
    if raw.contains("No new software available") {
        return UpdateState::UpToDate;
    }
    let count = raw
        .lines()
        .filter(|l| l.trim_start().starts_with("* Label:"))
        .count();
    if count > 0 || raw.contains("Software Update found the following") {
        UpdateState::Pending(count)
    } else {
        UpdateState::Unknown
    }
}

pub fn software_updates() -> Check {
    const KEY: &str = "software_updates";
    const TTL: Duration = Duration::from_secs(86_400); // 24h

    let combined = if let Some(cached) = cache::read(KEY, TTL) {
        cached
    } else {
        let Ok(out) = Command::new("softwareupdate").arg("-l").output() else {
            return Check::unknown("software_updates", "Software Updates", "N/A");
        };
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        cache::write(KEY, &combined);
        combined
    };

    match parse_updates(&combined) {
        UpdateState::UpToDate => Check::ok("software_updates", "Software Updates", "Up to date"),
        UpdateState::Pending(0) => Check::warn(
            "software_updates",
            "Software Updates",
            "Updates available",
            "run: softwareupdate -i -a",
        ),
        UpdateState::Pending(n) => Check::warn(
            "software_updates",
            "Software Updates",
            format!("{n} update(s) available"),
            "run: softwareupdate -i -a",
        ),
        UpdateState::Unknown => Check::unknown(
            "software_updates",
            "Software Updates",
            "unable to determine",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cpu_temp_strips_unit() {
        assert_eq!(parse_cpu_temp("56.3°C\n"), Some(56.3));
        assert_eq!(parse_cpu_temp("  72.5°C"), Some(72.5));
    }

    #[test]
    fn parse_cpu_temp_handles_integer() {
        assert_eq!(parse_cpu_temp("88°C"), Some(88.0));
    }

    #[test]
    fn parse_cpu_temp_returns_none_for_garbage() {
        assert_eq!(parse_cpu_temp(""), None);
        assert_eq!(parse_cpu_temp("not a number"), None);
    }

    #[test]
    fn parse_smart_verified() {
        let input = "\
   Device / Media Name:      APPLE SSD AP0512Z
   SMART Status:             Verified
   Volume Name:              ...
";
        assert_eq!(parse_smart_status(input).as_deref(), Some("Verified"));
    }

    #[test]
    fn parse_smart_failing() {
        let input = "   SMART Status:             Failing\n";
        assert_eq!(parse_smart_status(input).as_deref(), Some("Failing"));
    }

    #[test]
    fn parse_smart_missing_returns_none() {
        let input = "no smart line here\n";
        assert!(parse_smart_status(input).is_none());
    }

    #[test]
    fn parse_firewall_enabled() {
        assert_eq!(
            parse_firewall_state("Firewall is enabled. (State = 1)\n"),
            FirewallState::Enabled
        );
    }

    #[test]
    fn parse_firewall_disabled() {
        assert_eq!(
            parse_firewall_state("Firewall is disabled. (State = 0)\n"),
            FirewallState::Disabled
        );
    }

    #[test]
    fn parse_firewall_disabled_beats_enabled_word() {
        // "enabled" appears as substring of "disabled" — we should still classify as Disabled
        assert_eq!(
            parse_firewall_state("Firewall is disabled."),
            FirewallState::Disabled
        );
    }

    #[test]
    fn parse_firewall_unknown() {
        assert_eq!(parse_firewall_state(""), FirewallState::Unknown);
        assert_eq!(parse_firewall_state("garbage"), FirewallState::Unknown);
    }

    #[test]
    fn parse_updates_up_to_date() {
        let raw =
            "Software Update Tool\n\nFinding available software\nNo new software available.\n";
        assert_eq!(parse_updates(raw), UpdateState::UpToDate);
    }

    #[test]
    fn parse_updates_three_pending() {
        let raw = "\
Software Update Tool

Finding available software
Software Update found the following new or updated software:
* Label: Foo-1.2
   Title: Foo, Version: 1.2
* Label: Bar-2.0
   Title: Bar, Version: 2.0
* Label: Baz-3.1
   Title: Baz, Version: 3.1
";
        assert_eq!(parse_updates(raw), UpdateState::Pending(3));
    }

    #[test]
    fn parse_updates_unknown_for_garbage() {
        assert_eq!(parse_updates(""), UpdateState::Unknown);
        assert_eq!(parse_updates("random text"), UpdateState::Unknown);
    }
}
