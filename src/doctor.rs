//! `chevron doctor` — user-facing diagnostic subcommand. Bundles
//! environment / installation / integration / self-test checks into a
//! single report users can paste into bug reports.
//!
//! Composes on top of `health::Check` rather than duplicating the type.

use std::env;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use crate::health::{Check, Severity};

#[must_use]
pub fn run(args: &[String]) -> i32 {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("chevron doctor: {e}");
            print_usage();
            return 2;
        }
    };

    if opts.help {
        print_usage();
        return 0;
    }

    let sections = collect(opts.fast);

    if let Some(name) = &opts.check {
        let single: Vec<&Check> = sections
            .iter()
            .flat_map(|(_, checks)| checks.iter())
            .filter(|c| c.name == name)
            .collect();
        if single.is_empty() {
            eprintln!(
                "chevron doctor: unknown check '{name}'. Available: {}",
                available_names(&sections).join(", ")
            );
            return 2;
        }
        let owned: Vec<Check> = single.into_iter().cloned().collect();
        match opts.output {
            Output::Json => print!("{}", render_json_flat(&owned)),
            Output::Text => print!("{}", render_text_flat(&owned, opts.color)),
        }
        return exit_code_flat(&owned);
    }

    match opts.output {
        Output::Json => print!("{}", render_json(&sections)),
        Output::Text => print!("{}", render_text(&sections, opts.color)),
    }
    exit_code(&sections)
}

// ── CLI args ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Output {
    Text,
    Json,
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
    Ok(opts)
}

fn supports_color() -> bool {
    if env::var_os("NO_COLOR").is_some() {
        return false;
    }
    // SAFETY: isatty accepts any int fd; fd 1 (stdout) is always valid in a
    // running process. Return value is 0/1.
    unsafe { libc::isatty(1) != 0 }
}

fn print_usage() {
    eprintln!("Usage: chevron doctor [OPTIONS]");
    eprintln!();
    eprintln!("  --fast          Skip the self-test section (path/git render)");
    eprintln!("  --no-color      Disable ANSI color output");
    eprintln!("  --json          Emit machine-readable JSON");
    eprintln!("  --check <name>  Run only the named check (e.g. zshrc, libgit2)");
    eprintln!("  -h, --help      Show this help");
}

// ── sections ──────────────────────────────────────────────────────────────

type Section = (&'static str, Vec<Check>);

fn collect(fast: bool) -> Vec<Section> {
    let mut out = vec![
        ("Environment", environment_section()),
        ("Chevron", chevron_section()),
        ("Shell integration", shell_integration_section()),
    ];
    if !fast {
        out.push(("Self-test", self_test_section()));
    }
    out
}

fn available_names(sections: &[Section]) -> Vec<&'static str> {
    sections
        .iter()
        .flat_map(|(_, c)| c.iter().map(|c| c.name))
        .collect()
}

// ── Environment ───────────────────────────────────────────────────────────

fn environment_section() -> Vec<Check> {
    vec![
        os_check(),
        shell_check(),
        term_check(),
        locale_check(),
        glyph_check(),
    ]
}

fn os_check() -> Check {
    Check::ok(
        "os",
        "os",
        format!("{} ({})", env::consts::OS, env::consts::ARCH),
    )
}

fn shell_check() -> Check {
    match env::var("SHELL") {
        Ok(s) if !s.is_empty() => {
            let name = Path::new(&s)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or(s.as_str())
                .to_string();
            Check::ok("shell", "shell", name)
        }
        _ => Check::warn(
            "shell",
            "shell",
            "(unset)",
            "$SHELL is unset — chevron init script may not be loaded",
        ),
    }
}

fn term_check() -> Check {
    let term = env::var("TERM").unwrap_or_default();
    let colorterm = env::var("COLORTERM").unwrap_or_default();
    let value = if colorterm.is_empty() {
        format!("TERM={term}")
    } else {
        format!("TERM={term}  COLORTERM={colorterm}")
    };
    if term.is_empty() || term == "dumb" {
        return Check::warn(
            "term",
            "term",
            value,
            "TERM is empty or 'dumb' — colors and powerline glyphs will not render",
        );
    }
    if !term.contains("256") && !term.contains("color") && colorterm.is_empty() {
        return Check::warn(
            "term",
            "term",
            value,
            "terminal may not support 256 colors — try TERM=xterm-256color",
        );
    }
    Check::ok("term", "term", value)
}

fn locale_check() -> Check {
    let raw = env::var("LC_ALL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| env::var("LANG").ok())
        .unwrap_or_default();
    if raw.is_empty() {
        return Check::warn(
            "locale",
            "locale",
            "(unset)",
            "LANG/LC_ALL not set — non-ASCII glyphs may render incorrectly",
        );
    }
    let upper = raw.to_uppercase();
    if !upper.contains("UTF-8") && !upper.contains("UTF8") {
        return Check::warn(
            "locale",
            "locale",
            raw,
            "locale is not UTF-8 — powerline glyphs may render as '?'",
        );
    }
    Check::ok("locale", "locale", raw)
}

fn glyph_check() -> Check {
    // Visual self-test: powerline separator, a 256-color block, and a
    // representative Nerd Font glyph. The renderer prints this verbatim
    // so the user can eyeball whether their terminal + font support what
    // chevron needs. Severity::Info because we cannot programmatically
    // verify "looks correct" — the user has to look.
    let arrow = "\u{e0b0}"; // powerline right separator
    let nerd = "\u{f09b}"; // octicon-mark-github
    let block = "\u{2588}"; // full block
    let value = format!("{arrow}  {nerd}  {block}  (separator / nerd-font / block)");
    Check::info_hint(
        "glyph",
        "glyph",
        value,
        "if these don't render as a triangle, github logo, and a solid block, install a Nerd Font",
    )
}

// ── Chevron ───────────────────────────────────────────────────────────────

fn chevron_section() -> Vec<Check> {
    vec![
        Check::ok("version", "version", env!("CARGO_PKG_VERSION")),
        chevron_path_check(),
        libgit2_check(),
        daemon_feature_check(),
    ]
}

fn chevron_path_check() -> Check {
    env::current_exe().map_or_else(
        |e| {
            Check::warn(
                "path",
                "path",
                "(unknown)",
                format!("could not resolve binary path: {e}"),
            )
        },
        |p| Check::ok("path", "path", p.to_string_lossy().to_string()),
    )
}

fn libgit2_check() -> Check {
    let v = git2::Version::get();
    let (maj, min, patch) = v.libgit2_version();
    Check::ok("libgit2", "libgit2", format!("{maj}.{min}.{patch}"))
}

fn daemon_feature_check() -> Check {
    let value = if cfg!(feature = "daemon") {
        "built-in"
    } else {
        "not compiled in (build with --features daemon)"
    };
    Check::info("daemon_feature", "daemon", value)
}

// ── Shell integration ─────────────────────────────────────────────────────

fn shell_integration_section() -> Vec<Check> {
    let home = env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        return vec![Check::warn(
            "home",
            "home",
            "(unset)",
            "HOME is unset — cannot locate shell init files",
        )];
    }
    let shell_name = env::var("SHELL")
        .ok()
        .and_then(|s| {
            Path::new(&s)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .map(str::to_string)
        })
        .unwrap_or_default();

    vec![
        check_shell_init(&home, &shell_name, ShellTarget::Zsh),
        check_shell_init(&home, &shell_name, ShellTarget::Bash),
        check_shell_init(&home, &shell_name, ShellTarget::Fish),
        check_starship(&home),
        check_tmux(&home),
    ]
}

#[derive(Debug, Clone, Copy)]
enum ShellTarget {
    Zsh,
    Bash,
    Fish,
}

impl ShellTarget {
    fn name(self) -> &'static str {
        match self {
            ShellTarget::Zsh => "zsh",
            ShellTarget::Bash => "bash",
            ShellTarget::Fish => "fish",
        }
    }

    fn check_id(self) -> &'static str {
        match self {
            ShellTarget::Zsh => "zshrc",
            ShellTarget::Bash => "bashrc",
            ShellTarget::Fish => "fishrc",
        }
    }

    fn candidates(self) -> &'static [&'static str] {
        match self {
            ShellTarget::Zsh => &[".zshrc", ".zshenv", ".zprofile"],
            ShellTarget::Bash => &[".bashrc", ".bash_profile", ".profile"],
            ShellTarget::Fish => &[".config/fish/config.fish"],
        }
    }
}

fn check_shell_init(home: &str, current_shell: &str, target: ShellTarget) -> Check {
    let is_current = current_shell == target.name();
    // Find the first candidate file that exists for this shell.
    let found = target
        .candidates()
        .iter()
        .map(|c| PathBuf::from(home).join(c))
        .find(|p| p.exists());

    let Some(path) = found else {
        let value = format!("no {} init file found", target.name());
        return if is_current {
            Check::warn(
                target.check_id(),
                target.check_id(),
                value,
                format!(
                    "no init file for your current shell — add `eval \"$(chevron init {})\"`",
                    target.name()
                ),
            )
        } else {
            Check::info(target.check_id(), target.check_id(), value)
        };
    };

    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    let pretty = friendly_path(&path, home);
    if contents.contains("chevron init") {
        Check::ok(
            target.check_id(),
            target.check_id(),
            format!("{pretty} loads chevron"),
        )
    } else if is_current {
        Check::warn(
            target.check_id(),
            target.check_id(),
            format!("{pretty} does not reference `chevron init`"),
            format!(
                "add `eval \"$(chevron init {})\"` to {pretty}",
                target.name()
            ),
        )
    } else {
        Check::info(
            target.check_id(),
            target.check_id(),
            format!("{pretty} (not your current shell)"),
        )
    }
}

fn check_starship(home: &str) -> Check {
    let configured = env::var("STARSHIP_CONFIG").ok().filter(|s| !s.is_empty());
    let path = configured.map_or_else(
        || PathBuf::from(home).join(".config/starship.toml"),
        PathBuf::from,
    );
    if !path.exists() {
        return Check::info("starship", "starship", "not configured");
    }
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    let pretty = friendly_path(&path, home);
    if contents.contains("chevron") {
        Check::ok(
            "starship",
            "starship",
            format!("{pretty} references chevron"),
        )
    } else {
        Check::info(
            "starship",
            "starship",
            format!("{pretty} present (no chevron reference)"),
        )
    }
}

fn check_tmux(home: &str) -> Check {
    let candidates = [".tmux.conf", ".config/tmux/tmux.conf"];
    for c in &candidates {
        let p = PathBuf::from(home).join(c);
        if !p.exists() {
            continue;
        }
        let contents = std::fs::read_to_string(&p).unwrap_or_default();
        let pretty = friendly_path(&p, home);
        if contents.contains("chevron tmux-title") {
            return Check::ok("tmux", "tmux", format!("{pretty} uses chevron tmux-title"));
        }
        return Check::info(
            "tmux",
            "tmux",
            format!("{pretty} present (no chevron tmux-title reference)"),
        );
    }
    Check::info("tmux", "tmux", "no tmux config found")
}

fn friendly_path(path: &Path, home: &str) -> String {
    let s = path.to_string_lossy();
    if !home.is_empty() && s.starts_with(home) {
        format!("~{}", &s[home.len()..])
    } else {
        s.into_owned()
    }
}

// ── Self-test ─────────────────────────────────────────────────────────────

fn self_test_section() -> Vec<Check> {
    let home = env::var("HOME").unwrap_or_default();
    let pwd = env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path_out = crate::segments::path::render_aware(&home, &pwd, None);
    let git_out = crate::segments::git::render(Path::new("."));
    let git_value = if git_out.is_empty() {
        "(not in a git repository)".to_string()
    } else {
        git_out
    };
    vec![
        Check::info("path_render", "path render", path_out),
        Check::info("git_render", "git render", git_value),
    ]
}

// ── Rendering: text ───────────────────────────────────────────────────────

fn render_text(sections: &[Section], color: bool) -> String {
    let mut out = String::with_capacity(2048);
    let _ = writeln!(out, "chevron doctor {}", env!("CARGO_PKG_VERSION"));
    for (name, checks) in sections {
        render_text_section(&mut out, name, checks, color);
    }
    render_summary(&mut out, sections, color);
    out
}

fn render_text_flat(checks: &[Check], color: bool) -> String {
    let mut out = String::with_capacity(256);
    let label_width = max_label_width(checks);
    for c in checks {
        render_check_line(&mut out, c, label_width, color);
    }
    out
}

fn render_text_section(out: &mut String, name: &str, checks: &[Check], color: bool) {
    let _ = writeln!(out);
    let (bold_on, bold_off) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let _ = writeln!(out, "{bold_on}{name}{bold_off}");
    let label_width = max_label_width(checks);
    for c in checks {
        render_check_line(out, c, label_width, color);
    }
}

fn render_check_line(out: &mut String, c: &Check, label_width: usize, color: bool) {
    let tag = severity_tag(c.severity, color);
    let label_pad = " ".repeat(label_width.saturating_sub(c.label.len()));
    let _ = writeln!(out, "  {tag}  {}{}  {}", c.label, label_pad, c.value);
    if let Some(hint) = &c.hint {
        // Continuation: keep aligned under the value column.
        let hint_color = if color { "\x1b[2m" } else { "" };
        let hint_reset = if color { "\x1b[0m" } else { "" };
        // Tag is 6 visible chars + 2 leading spaces + 2 between tag+label
        // + label_width + 2 between label+value = 12 + label_width.
        let indent = " ".repeat(12 + label_width);
        let _ = writeln!(out, "{indent}{hint_color}hint: {hint}{hint_reset}");
    }
}

fn max_label_width(checks: &[Check]) -> usize {
    checks.iter().map(|c| c.label.len()).max().unwrap_or(0)
}

/// Severity tag with fixed 6-column visible width so labels align across rows.
/// Color escapes are zero-width, so visible padding stays correct.
fn severity_tag(sev: Severity, color: bool) -> String {
    let (visible, ansi_color) = match sev {
        Severity::Ok => ("[ok]  ", "\x1b[32m"),
        Severity::Warn => ("[warn]", "\x1b[33m"),
        Severity::Critical => ("[fail]", "\x1b[31m"),
        Severity::Info => ("[info]", "\x1b[36m"),
        Severity::Unknown => ("[?]   ", "\x1b[2m"),
    };
    if color {
        format!("{ansi_color}{visible}\x1b[0m")
    } else {
        visible.to_string()
    }
}

fn render_summary(out: &mut String, sections: &[Section], color: bool) {
    let mut critical = 0_u32;
    let mut warn = 0_u32;
    let mut ok = 0_u32;
    let mut info = 0_u32;
    for (_, checks) in sections {
        for c in checks {
            match c.severity {
                Severity::Critical => critical += 1,
                Severity::Warn => warn += 1,
                Severity::Ok => ok += 1,
                Severity::Info | Severity::Unknown => info += 1,
            }
        }
    }
    let (bold_on, bold_off) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{bold_on}Summary{bold_off}: {critical} critical, {warn} warning(s), {ok} ok, {info} info"
    );
}

// ── Rendering: json ───────────────────────────────────────────────────────

fn render_json(sections: &[Section]) -> String {
    let mut out = String::with_capacity(1024);
    let _ = write!(
        out,
        "{{\"version\":\"{}\",\"sections\":[",
        env!("CARGO_PKG_VERSION")
    );
    let mut first = true;
    for (name, checks) in sections {
        if !first {
            out.push(',');
        }
        first = false;
        let _ = write!(out, "{{\"name\":\"{}\",\"checks\":[", json_escape(name));
        let mut cfirst = true;
        for c in checks {
            if !cfirst {
                out.push(',');
            }
            cfirst = false;
            out.push_str(&check_to_json(c));
        }
        out.push_str("]}");
    }
    out.push_str("],");
    let (critical, warn, ok, info) = counts(sections);
    let _ = writeln!(
        out,
        "\"summary\":{{\"critical\":{critical},\"warn\":{warn},\"ok\":{ok},\"info\":{info}}}}}"
    );
    out
}

fn render_json_flat(checks: &[Check]) -> String {
    let mut out = String::with_capacity(256);
    out.push_str("{\"checks\":[");
    let mut first = true;
    for c in checks {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&check_to_json(c));
    }
    out.push_str("]}\n");
    out
}

fn counts(sections: &[Section]) -> (u32, u32, u32, u32) {
    let mut critical = 0;
    let mut warn = 0;
    let mut ok = 0;
    let mut info = 0;
    for (_, checks) in sections {
        for c in checks {
            match c.severity {
                Severity::Critical => critical += 1,
                Severity::Warn => warn += 1,
                Severity::Ok => ok += 1,
                Severity::Info | Severity::Unknown => info += 1,
            }
        }
    }
    (critical, warn, ok, info)
}

fn check_to_json(c: &Check) -> String {
    let sev = match c.severity {
        Severity::Ok => "ok",
        Severity::Warn => "warn",
        Severity::Critical => "critical",
        Severity::Info => "info",
        Severity::Unknown => "unknown",
    };
    let hint = match &c.hint {
        Some(h) => format!("\"{}\"", json_escape(h)),
        None => "null".to_string(),
    };
    format!(
        "{{\"name\":\"{}\",\"label\":\"{}\",\"value\":\"{}\",\"severity\":\"{}\",\"hint\":{}}}",
        json_escape(c.name),
        json_escape(c.label),
        json_escape(&c.value),
        sev,
        hint,
    )
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

// ── Exit codes ────────────────────────────────────────────────────────────

fn exit_code(sections: &[Section]) -> i32 {
    sections
        .iter()
        .flat_map(|(_, c)| c.iter())
        .fold(0_i32, |acc, c| acc.max(severity_to_exit(c.severity)))
}

fn exit_code_flat(checks: &[Check]) -> i32 {
    checks
        .iter()
        .fold(0_i32, |acc, c| acc.max(severity_to_exit(c.severity)))
}

fn severity_to_exit(sev: Severity) -> i32 {
    match sev {
        Severity::Critical => 2,
        Severity::Warn => 1,
        Severity::Ok | Severity::Info | Severity::Unknown => 0,
    }
}

// ── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn parse_defaults() {
        let o = parse_args(&[]).unwrap();
        assert!(!o.fast);
        assert_eq!(o.output, Output::Text);
        assert!(o.check.is_none());
        assert!(!o.help);
    }

    #[test]
    fn parse_fast_and_json() {
        let o = parse_args(&args(&["--fast", "--json"])).unwrap();
        assert!(o.fast);
        assert_eq!(o.output, Output::Json);
    }

    #[test]
    fn parse_check_requires_argument() {
        let err = parse_args(&args(&["--check"])).unwrap_err();
        assert!(err.contains("requires"), "got: {err}");
    }

    #[test]
    fn parse_check_takes_name() {
        let o = parse_args(&args(&["--check", "libgit2"])).unwrap();
        assert_eq!(o.check.as_deref(), Some("libgit2"));
    }

    #[test]
    fn parse_unknown_flag_errors() {
        let err = parse_args(&args(&["--bogus"])).unwrap_err();
        assert!(err.contains("unknown"), "got: {err}");
    }

    #[test]
    fn parse_no_color_disables_color() {
        let o = parse_args(&args(&["--no-color"])).unwrap();
        assert!(!o.color);
    }

    // ── severity tag ──────────────────────────────────────────────────────

    #[test]
    fn severity_tag_visible_width_no_color() {
        // All tags must occupy exactly 6 visible columns so labels align.
        for sev in [
            Severity::Ok,
            Severity::Warn,
            Severity::Critical,
            Severity::Info,
            Severity::Unknown,
        ] {
            let t = severity_tag(sev, false);
            assert_eq!(t.chars().count(), 6, "{sev:?} tag must be 6 cols: {t:?}");
        }
    }

    #[test]
    fn severity_tag_color_wraps_in_escapes() {
        let t = severity_tag(Severity::Warn, true);
        assert!(t.starts_with("\x1b["));
        assert!(t.contains("[warn]"));
        assert!(t.ends_with("\x1b[0m"));
    }

    #[test]
    fn severity_tag_no_color_has_no_escapes() {
        let t = severity_tag(Severity::Ok, false);
        assert!(!t.contains('\x1b'));
    }

    // ── exit codes ────────────────────────────────────────────────────────

    #[test]
    fn exit_code_zero_on_all_ok() {
        let s: Vec<Section> = vec![("Env", vec![Check::ok("a", "a", "x")])];
        assert_eq!(exit_code(&s), 0);
    }

    #[test]
    fn exit_code_one_on_warn() {
        let s: Vec<Section> = vec![(
            "Env",
            vec![Check::ok("a", "a", "x"), Check::warn("b", "b", "y", "h")],
        )];
        assert_eq!(exit_code(&s), 1);
    }

    #[test]
    fn exit_code_two_on_critical() {
        let s: Vec<Section> = vec![(
            "Env",
            vec![
                Check::warn("a", "a", "x", "h"),
                Check::critical("b", "b", "y", "h"),
            ],
        )];
        assert_eq!(exit_code(&s), 2);
    }

    #[test]
    fn info_does_not_affect_exit_code() {
        let s: Vec<Section> = vec![("Env", vec![Check::info("a", "a", "x")])];
        assert_eq!(exit_code(&s), 0);
    }

    // ── glyph check ───────────────────────────────────────────────────────

    #[test]
    fn glyph_check_includes_powerline_arrow() {
        let c = glyph_check();
        assert!(c.value.contains('\u{e0b0}'), "missing powerline U+E0B0");
        assert_eq!(c.severity, Severity::Info);
        assert!(c.hint.is_some(), "glyph check should hint about fonts");
    }

    // ── friendly_path ────────────────────────────────────────────────────

    #[test]
    fn friendly_path_collapses_home() {
        let p = PathBuf::from("/Users/mim/.zshrc");
        assert_eq!(friendly_path(&p, "/Users/mim"), "~/.zshrc");
    }

    #[test]
    fn friendly_path_passthrough_when_outside_home() {
        let p = PathBuf::from("/etc/zshrc");
        assert_eq!(friendly_path(&p, "/Users/mim"), "/etc/zshrc");
    }

    #[test]
    fn friendly_path_empty_home_returns_full() {
        let p = PathBuf::from("/anywhere/foo");
        assert_eq!(friendly_path(&p, ""), "/anywhere/foo");
    }

    // ── shell-integration check (with tempdir fixtures) ───────────────────

    fn write_temp(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn shell_init_detects_chevron_init_line() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        write_temp(&tmp.path().join(".zshrc"), "eval \"$(chevron init zsh)\"\n");
        let c = check_shell_init(&home, "zsh", ShellTarget::Zsh);
        assert_eq!(c.severity, Severity::Ok);
        assert!(c.value.contains("loads chevron"));
    }

    #[test]
    fn shell_init_warns_when_current_shell_missing_chevron() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        write_temp(&tmp.path().join(".zshrc"), "# nothing chevron-related\n");
        let c = check_shell_init(&home, "zsh", ShellTarget::Zsh);
        assert_eq!(c.severity, Severity::Warn);
        assert!(c.hint.as_deref().unwrap_or("").contains("chevron init zsh"));
    }

    #[test]
    fn shell_init_info_when_not_current_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        write_temp(&tmp.path().join(".bashrc"), "# no chevron\n");
        // User is on zsh; bashrc absent reference is just informational.
        let c = check_shell_init(&home, "zsh", ShellTarget::Bash);
        assert_eq!(c.severity, Severity::Info);
    }

    #[test]
    fn shell_init_warns_when_current_shell_has_no_init_file() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        // No file written; user is on zsh, so missing zshrc is a warning.
        let c = check_shell_init(&home, "zsh", ShellTarget::Zsh);
        assert_eq!(c.severity, Severity::Warn);
    }

    #[test]
    fn starship_info_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let c = check_starship(&home);
        assert_eq!(c.severity, Severity::Info);
        assert!(c.value.contains("not configured"));
    }

    #[test]
    fn starship_ok_when_references_chevron() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        write_temp(
            &tmp.path().join(".config/starship.toml"),
            "[custom.chevron]\ncommand = \"chevron path\"\n",
        );
        let c = check_starship(&home);
        assert_eq!(c.severity, Severity::Ok);
    }

    #[test]
    fn tmux_ok_when_uses_chevron_tmux_title() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        write_temp(
            &tmp.path().join(".tmux.conf"),
            "set -g status-left '#(chevron tmux-title)'\n",
        );
        let c = check_tmux(&home);
        assert_eq!(c.severity, Severity::Ok);
    }

    #[test]
    fn tmux_info_when_no_config() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let c = check_tmux(&home);
        assert_eq!(c.severity, Severity::Info);
    }

    // ── chevron section ──────────────────────────────────────────────────

    #[test]
    fn libgit2_check_returns_version_string() {
        let c = libgit2_check();
        assert_eq!(c.severity, Severity::Ok);
        // libgit2 version is "major.minor.patch" — must contain two dots.
        assert_eq!(c.value.matches('.').count(), 2, "got {:?}", c.value);
    }

    #[test]
    fn chevron_section_includes_version_path_libgit2_daemon() {
        let s = chevron_section();
        let names: Vec<&str> = s.iter().map(|c| c.name).collect();
        assert!(names.contains(&"version"));
        assert!(names.contains(&"path"));
        assert!(names.contains(&"libgit2"));
        assert!(names.contains(&"daemon_feature"));
    }

    // ── rendering ────────────────────────────────────────────────────────

    #[test]
    fn render_text_includes_section_names_and_summary() {
        let s: Vec<Section> = vec![
            ("Environment", vec![Check::ok("a", "a", "x")]),
            ("Chevron", vec![Check::info("b", "b", "y")]),
        ];
        let out = render_text(&s, false);
        assert!(out.contains("Environment"));
        assert!(out.contains("Chevron"));
        assert!(out.contains("Summary"));
        assert!(out.contains("0 critical"));
        assert!(!out.contains('\x1b'), "no-color must strip escapes");
    }

    #[test]
    fn render_text_includes_color_escapes_when_enabled() {
        let s: Vec<Section> = vec![("Env", vec![Check::ok("a", "a", "x")])];
        let out = render_text(&s, true);
        assert!(out.contains('\x1b'));
    }

    #[test]
    fn render_text_hint_line_aligned_under_value() {
        let s: Vec<Section> = vec![("Env", vec![Check::warn("a", "a", "x", "fix it")])];
        let out = render_text(&s, false);
        assert!(out.contains("hint: fix it"));
    }

    #[test]
    fn render_json_is_valid_shape() {
        let s: Vec<Section> = vec![("Environment", vec![Check::ok("a", "a", "x")])];
        let out = render_json(&s);
        assert!(out.starts_with("{\"version\":\""));
        assert!(out.contains("\"sections\":["));
        assert!(out.contains("\"summary\":{"));
        assert!(out.contains("\"name\":\"Environment\""));
        assert!(out.ends_with("}\n"));
    }

    #[test]
    fn render_json_escapes_quotes_in_values() {
        let s: Vec<Section> = vec![("E", vec![Check::ok("a", "a", "x\"y")])];
        let out = render_json(&s);
        assert!(out.contains("\"value\":\"x\\\"y\""));
    }

    #[test]
    fn json_escape_handles_control_chars() {
        assert_eq!(json_escape("a\nb"), "a\\nb");
        assert_eq!(json_escape("a\x01b"), "a\\u0001b");
    }
}
