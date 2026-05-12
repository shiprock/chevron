mod check;
mod checks;
mod render;

use crate::sysinfo::SystemInfo;

use check::Check;

pub fn run(args: &[String]) -> i32 {
    let mut fast = false;
    let mut color = supports_color();
    for arg in args {
        match arg.as_str() {
            "--fast" => fast = true,
            "--no-color" => color = false,
            "-h" | "--help" => {
                print_usage();
                return 0;
            }
            other => {
                eprintln!("plx health: unknown argument '{other}'");
                print_usage();
                return 2;
            }
        }
    }

    let info = SystemInfo::gather();
    let collected = collect(&info, fast);
    print!("{}", render::render(&collected, color));
    exit_code(&collected)
}

fn collect(info: &SystemInfo, fast: bool) -> Vec<Check> {
    let mut out = vec![checks::load(info), checks::memory(info), checks::disk(info)];
    if !fast {
        out.push(checks::uptime(info));
        out.push(checks::network());
    }
    out
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
    // libc::isatty(1) — stdout is fd 1
    unsafe { libc::isatty(1) != 0 }
}

fn print_usage() {
    eprintln!("Usage: plx health [--fast] [--no-color]");
    eprintln!();
    eprintln!("  --fast       Skip slower checks (uptime, network)");
    eprintln!("  --no-color   Disable ANSI color output");
}

#[cfg(test)]
mod tests {
    use super::check::Check;
    use super::exit_code;

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
}
