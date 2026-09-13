// `cache` is only used by the macOS-specific check module today; gating it
// avoids dead_code warnings on Linux.
#[cfg(target_os = "macos")]
mod cache;
mod check;
mod checks;
#[cfg(target_os = "macos")]
mod macos;
mod render;

use crate::config::HealthConfig;
use crate::sysinfo::SystemInfo;

// Re-exported so the doctor subcommand can build its own Check lists without
// duplicating the type. health owns the Check vocabulary; doctor composes on top.
pub use check::{Check, Severity};

#[must_use]
pub fn run(args: &[String]) -> i32 {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("chevron health: {e}");
            print_usage();
            return 2;
        }
    };

    if opts.help {
        print_usage();
        return 0;
    }

    let cfg = crate::config::Config::load().health;

    // Single-check mode — runs even if disabled in config; the user asked for it.
    if let Some(name) = &opts.check {
        let info = SystemInfo::gather();
        let Some(check) = run_named(name, &info, &cfg) else {
            eprintln!(
                "chevron health: unknown check '{name}'. Available: {}",
                available_names().join(", ")
            );
            return 2;
        };
        let single = std::slice::from_ref(&check);
        match opts.output {
            Output::Json => print!("{}", render::render_json(single, true)),
            Output::Value => print!("{}", render::render_value(&check)),
            Output::Text => println!("{}", render::render_line(&check, opts.color)),
        }
        return exit_code(single);
    }

    // Full-report mode
    let info = SystemInfo::gather();
    let collected = collect(&info, opts.fast, &cfg);
    match opts.output {
        Output::Json => print!("{}", render::render_json(&collected, false)),
        Output::Text | Output::Value => print!("{}", render::render(&collected, opts.color)),
    }
    exit_code(&collected)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Output {
    Text,
    Json,
    Value,
}

#[derive(Debug)]
struct Opts {
    fast: bool,
    color: bool,
    output: Output,
    check: Option<String>,
    help: bool,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts {
        fast: false,
        color: supports_color(),
        output: Output::Text,
        check: None,
        help: false,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--fast" => opts.fast = true,
            "--no-color" => opts.color = false,
            "--json" => opts.output = Output::Json,
            "--value" => opts.output = Output::Value,
            "--check" => {
                let name = iter
                    .next()
                    .ok_or_else(|| "--check requires a check name".to_string())?;
                opts.check = Some(name.clone());
            }
            "-h" | "--help" => opts.help = true,
            other => return Err(format!("unknown argument '{other}'")),
        }
    }

    if opts.output == Output::Value && opts.check.is_none() {
        return Err("--value requires --check <name>".to_string());
    }
    Ok(opts)
}

fn collect(info: &SystemInfo, fast: bool, cfg: &HealthConfig) -> Vec<Check> {
    let enabled = |name: &str| !cfg.disabled.iter().any(|d| d == name);
    let mut out = Vec::with_capacity(10);

    if enabled("load") {
        out.push(checks::load(info, &cfg.thresholds));
    }
    if enabled("memory") {
        out.push(checks::memory(info, &cfg.thresholds));
    }
    if enabled("disk") {
        out.push(checks::disk(info, &cfg.thresholds));
    }
    if !fast {
        if enabled("uptime") {
            out.push(checks::uptime(info));
        }
        if enabled("ip") {
            out.push(checks::ip_address(info));
        }
        if enabled("network") {
            out.push(checks::network(&cfg.network));
        }
        #[cfg(target_os = "macos")]
        {
            if enabled("cpu_temp") {
                out.push(macos::cpu_temp(&cfg.thresholds));
            }
            if enabled("disk_health") {
                out.push(macos::disk_health());
            }
            if enabled("software_updates") {
                out.push(macos::software_updates());
            }
            if enabled("firewall") {
                out.push(macos::firewall());
            }
        }
    }
    out
}

/// Look up and run a single check by machine-readable name.
fn run_named(name: &str, info: &SystemInfo, cfg: &HealthConfig) -> Option<Check> {
    match name {
        "load" => Some(checks::load(info, &cfg.thresholds)),
        "memory" => Some(checks::memory(info, &cfg.thresholds)),
        "disk" => Some(checks::disk(info, &cfg.thresholds)),
        "uptime" => Some(checks::uptime(info)),
        "ip" => Some(checks::ip_address(info)),
        "network" => Some(checks::network(&cfg.network)),
        #[cfg(target_os = "macos")]
        "cpu_temp" => Some(macos::cpu_temp(&cfg.thresholds)),
        #[cfg(target_os = "macos")]
        "disk_health" => Some(macos::disk_health()),
        #[cfg(target_os = "macos")]
        "software_updates" => Some(macos::software_updates()),
        #[cfg(target_os = "macos")]
        "firewall" => Some(macos::firewall()),
        _ => None,
    }
}

fn available_names() -> Vec<&'static str> {
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut names = vec!["load", "memory", "disk", "uptime", "ip", "network"];
    #[cfg(target_os = "macos")]
    names.extend(&["cpu_temp", "disk_health", "software_updates", "firewall"]);
    names
}

fn exit_code(checks: &[Check]) -> i32 {
    use check::Severity;
    checks.iter().fold(0_i32, |acc, c| {
        let code = match c.severity {
            Severity::Critical => 2,
            Severity::Warn => 1,
            _ => 0,
        };
        acc.max(code)
    })
}

fn supports_color() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    // SAFETY: isatty is always safe to call with any int fd; it returns 0/1
    // and sets errno on failure. Stdout (fd 1) is always a valid file descriptor
    // in a running process.
    unsafe { libc::isatty(1) != 0 }
}

fn print_usage() {
    eprintln!("Usage: chevron health [OPTIONS]");
    eprintln!();
    eprintln!("  --fast              Skip slower checks (uptime, network, macOS extras)");
    eprintln!("  --no-color          Disable ANSI color output");
    eprintln!("  --json              Emit machine-readable JSON");
    eprintln!("  --check <name>      Run only the named check");
    eprintln!("  --value             Print only the value (requires --check)");
    eprintln!("  -h, --help          Show this help");
    eprintln!();
    eprintln!("Available checks: {}", available_names().join(", "));
}

#[cfg(test)]
mod tests {
    use super::check::{Check, Severity};
    use super::{available_names, exit_code, parse_args};

    #[test]
    fn exit_code_zero_when_all_ok() {
        let checks = vec![Check::ok("a", "A", "1")];
        assert_eq!(exit_code(&checks), 0);
    }

    #[test]
    fn exit_code_one_on_warn() {
        let checks = vec![Check::ok("a", "A", "1"), Check::warn("b", "B", "x", "hint")];
        assert_eq!(exit_code(&checks), 1);
    }

    #[test]
    fn exit_code_two_on_critical_even_if_warn_present() {
        let checks = vec![
            Check::warn("a", "A", "x", "hint"),
            Check::critical("b", "B", "y", "hint"),
        ];
        assert_eq!(exit_code(&checks), 2);
    }

    #[test]
    fn _severity_used_in_tests() {
        // touch Severity so the import isn't flagged as unused
        let _ = Severity::Ok;
    }

    // ── flag parsing ────────────────────────────────────────────────────────

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn parse_defaults() {
        let o = parse_args(&[]).unwrap();
        assert!(!o.fast);
        assert_eq!(o.output, super::Output::Text);
        assert!(o.check.is_none());
    }

    #[test]
    fn parse_check_requires_argument() {
        let err = parse_args(&args(&["--check"])).unwrap_err();
        assert!(err.contains("requires"), "got: {err}");
    }

    #[test]
    fn parse_check_takes_name() {
        let o = parse_args(&args(&["--check", "load"])).unwrap();
        assert_eq!(o.check.as_deref(), Some("load"));
    }

    #[test]
    fn parse_value_requires_check() {
        let err = parse_args(&args(&["--value"])).unwrap_err();
        assert!(err.contains("requires --check"), "got: {err}");
    }

    #[test]
    fn parse_value_then_json_lets_json_win() {
        // Output is an enum, so last-wins is fine — no error
        let o = parse_args(&args(&["--check", "load", "--value", "--json"])).unwrap();
        assert_eq!(o.output, super::Output::Json);
    }

    #[test]
    fn parse_unknown_flag_errors() {
        let err = parse_args(&args(&["--bogus"])).unwrap_err();
        assert!(err.contains("unknown"), "got: {err}");
    }

    #[test]
    fn available_names_includes_core_six() {
        let n = available_names();
        for name in ["load", "memory", "disk", "uptime", "ip", "network"] {
            assert!(n.contains(&name), "missing {name} in {n:?}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn available_names_includes_macos_four_on_macos() {
        let n = available_names();
        for name in ["cpu_temp", "disk_health", "software_updates", "firewall"] {
            assert!(n.contains(&name), "missing {name} in {n:?}");
        }
    }
}
