//! `chevron configure` — interactive wizard that writes config.toml and
//! prints a shell-rc snippet. The wizard's data layer is split into
//! `answers` (the data carrier), `presets` (starting points), and `emit`
//! (the pure functional core, snapshot-tested). The TUI layer in `tui`
//! is a thin inquire-based wrapper that produces an `AnswerSet`.

mod answers;
mod emit;
mod presets;
mod preview;
mod tui;
mod writer;

use std::env;
use std::path::PathBuf;

use answers::{AnswerSet, Preset};

#[must_use]
pub fn run(args: &[String]) -> i32 {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("chevron configure: {e}");
            print_usage();
            return 2;
        }
    };

    if opts.help {
        print_usage();
        return 0;
    }

    let answers = match collect_answers(&opts) {
        Ok(Some(a)) => a,
        Ok(None) => {
            println!("aborted — nothing written.");
            return 0;
        }
        Err(code) => return code,
    };

    let toml = emit::to_config_toml(&answers);
    let shell = detect_shell();
    let snippet = emit::to_shell_snippet(&answers, &shell);

    if opts.dry_run {
        println!("# {}", config_path().display());
        println!("{toml}");
        println!("# Shell snippet for {shell}:");
        println!("{snippet}");
        return 0;
    }

    let path = config_path();
    match writer::write_with_backup(&path, &toml) {
        Ok(res) => {
            if let Some(bak) = res.backed_up {
                println!("Backed up previous config to {}", bak.display());
            }
            println!("Wrote {}", res.written.display());
        }
        Err(e) => {
            eprintln!("chevron configure: failed to write config: {e}");
            return 1;
        }
    }

    println!();
    println!("Add this to your shell rc to activate:");
    println!();
    for line in snippet.lines() {
        println!("  {line}");
    }
    println!();
    println!("Then start a new shell. Run `chevron doctor` to verify.");
    0
}

fn collect_answers(opts: &Opts) -> Result<Option<AnswerSet>, i32> {
    if let Some(name) = &opts.preset {
        let Some(p) = Preset::from_name(name) else {
            eprintln!(
                "chevron configure: unknown preset '{name}'. Available: {}",
                Preset::all().map(Preset::name).join(", ")
            );
            return Err(2);
        };
        return Ok(Some(presets::from_preset(p)));
    }
    match tui::run_wizard() {
        Ok(maybe) => Ok(maybe),
        Err(
            inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted,
        ) => {
            eprintln!("aborted.");
            Err(130) // conventional SIGINT exit code
        }
        Err(e) => {
            eprintln!("chevron configure: {e}");
            Err(1)
        }
    }
}

// ── CLI args ──────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct Opts {
    preset: Option<String>,
    dry_run: bool,
    help: bool,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dry-run" => opts.dry_run = true,
            "--preset" => {
                let name = iter
                    .next()
                    .ok_or_else(|| "--preset requires a name".to_string())?;
                opts.preset = Some(name.clone());
            }
            "-h" | "--help" => opts.help = true,
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(opts)
}

fn print_usage() {
    eprintln!("Usage: chevron configure [OPTIONS]");
    eprintln!();
    eprintln!(
        "  --preset <name>  Skip the wizard, apply preset (lean|pure|rainbow|classic|minimal)"
    );
    eprintln!("  --dry-run        Print what would be written, do not modify any files");
    eprintln!("  -h, --help       Show this help");
}

// ── paths / shell detection ───────────────────────────────────────────────

fn config_path() -> PathBuf {
    if let Ok(path) = env::var("CHEVRON_CONFIG") {
        return PathBuf::from(path);
    }
    let base = env::var("XDG_CONFIG_HOME").map_or_else(
        |_| {
            let home = env::var("HOME").unwrap_or_else(|_| String::from("."));
            PathBuf::from(home).join(".config")
        },
        PathBuf::from,
    );
    base.join("chevron").join("config.toml")
}

fn detect_shell() -> String {
    let s = env::var("SHELL").unwrap_or_default();
    let name = std::path::Path::new(&s)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("");
    match name {
        "bash" | "fish" | "zsh" => name.to_string(),
        _ => "zsh".to_string(), // safe default for the snippet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn parse_defaults() {
        let o = parse_args(&[]).unwrap();
        assert!(!o.dry_run);
        assert!(o.preset.is_none());
    }

    #[test]
    fn parse_preset_requires_name() {
        let err = parse_args(&args(&["--preset"])).unwrap_err();
        assert!(err.contains("requires"));
    }

    #[test]
    fn parse_preset_with_name() {
        let o = parse_args(&args(&["--preset", "lean"])).unwrap();
        assert_eq!(o.preset.as_deref(), Some("lean"));
    }

    #[test]
    fn parse_dry_run() {
        let o = parse_args(&args(&["--dry-run"])).unwrap();
        assert!(o.dry_run);
    }

    #[test]
    fn parse_unknown_flag_errors() {
        let err = parse_args(&args(&["--bogus"])).unwrap_err();
        assert!(err.contains("unknown"));
    }

    #[test]
    fn detect_shell_falls_back_to_zsh() {
        // serial_test would be ideal but we just want to verify the
        // fallback path. Save + restore SHELL.
        let saved = env::var("SHELL").ok();
        // SAFETY: test-only env manipulation; flagged with no env-racing tests
        // in this module.
        unsafe { env::set_var("SHELL", "/bin/weird") };
        assert_eq!(detect_shell(), "zsh");
        // SAFETY: see above
        unsafe {
            if let Some(v) = saved {
                env::set_var("SHELL", v);
            } else {
                env::remove_var("SHELL");
            }
        }
    }
}
