use std::env;
use std::path::Path;

#[cfg(feature = "banner")]
use chevron::banner;
#[cfg(feature = "weather")]
use chevron::weather;
use chevron::{color, configure, doctor, health, repo_status, segments, shell};

#[allow(clippy::too_many_lines)]
fn main() {
    let args: Vec<String> = env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("path") => {
            let home = env::var("HOME").unwrap_or_default();
            let pwd = env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let max_dir_size = args.get(2).and_then(|s| s.parse::<usize>().ok());
            // render_aware discovers any git repo so the path collapses
            // to a repo-relative form when applicable (chevron-ir6).
            print!(
                "{}",
                segments::path::render_aware(&home, &pwd, max_dir_size)
            );
        }
        Some("git") => print!("{}", segments::git::render(Path::new("."))),
        Some("nix-shell") => print!("{}", segments::nix_shell::render()),
        Some("aws") => print!("{}", segments::aws::render()),
        Some("prompt") => {
            let max_dir_size = args.get(2).and_then(|s| s.parse().ok());
            let exit_status = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let duration_ms = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            let job_count = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
            let mut ctx = segments::prompt::PromptContext::gather(
                max_dir_size,
                exit_status,
                duration_ms,
                job_count,
            );
            let raw = segments::prompt::render(&mut ctx);
            let shell = env::var("CHEVRON_SHELL").unwrap_or_default();
            // Only the prompt line gets escape wrapping; tmux title is plain.
            let printed = if let Some((prompt, title)) = raw.split_once('\n') {
                format!("{}\n{}", color::wrap_for_shell(&shell, prompt), title)
            } else {
                color::wrap_for_shell(&shell, &raw)
            };
            print!("{printed}");
        }
        Some("tmux-title") => {
            let home = env::var("HOME").unwrap_or_default();
            let pwd = env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            println!("{}", segments::tmux_title::render(&home, &pwd));
        }
        Some("init") => {
            // `--instant-prompt` short-circuits config loading — the snippet
            // is pure shell with no config dependence.
            if args.get(3).map(String::as_str) == Some("--instant-prompt") {
                if args.get(2).map(String::as_str) == Some("zsh") {
                    print!("{}", shell::init_zsh_instant_prompt());
                } else {
                    eprintln!("Usage: chevron init zsh --instant-prompt (zsh-only feature)");
                    std::process::exit(1);
                }
            } else {
                retire_legacy_prompt_cache();
                // Read config so the init script's `[shell]` defaults are
                // baked in. Env vars set before sourcing still win.
                let cfg = chevron::config::Config::load();
                match args.get(2).map(String::as_str) {
                    Some("zsh") => print!("{}", shell::init_zsh_with(&cfg.shell)),
                    Some("bash") => print!("{}", shell::init_bash_with(&cfg.shell)),
                    Some("fish") => print!("{}", shell::init_fish_with(&cfg.shell)),
                    _ => {
                        eprintln!("Usage: chevron init <zsh|bash|fish>");
                        std::process::exit(1);
                    }
                }
            }
        }
        Some("status") => repo_status::run(),
        Some("health") => std::process::exit(health::run(&args[2..])),
        Some("doctor") => std::process::exit(doctor::run(&args[2..])),
        Some("configure") => std::process::exit(configure::run(&args[2..])),
        #[cfg(feature = "daemon")]
        Some("capture") => std::process::exit(chevron::capture::run(&args[2..])),
        #[cfg(feature = "daemon")]
        Some("event") => std::process::exit(chevron::event::run(&args[2..])),
        #[cfg(feature = "daemon")]
        Some("history") => std::process::exit(chevron::history::run(&args[2..])),
        #[cfg(feature = "daemon")]
        Some("subscribe") => std::process::exit(chevron::subscribe::run(&args[2..])),
        #[cfg(feature = "daemon")]
        Some("daemon") => match args.get(2).map(String::as_str) {
            Some("serve") => {
                if let Err(e) = chevron::daemon::lifecycle::serve() {
                    eprintln!("chevrond: {e}");
                    std::process::exit(1);
                }
            }
            Some("start") => {
                chevron::daemon::client::try_spawn_async();
            }
            Some("stop") => std::process::exit(chevron::daemon::lifecycle::stop()),
            Some("status") => std::process::exit(chevron::daemon::lifecycle::status()),
            Some("version") => std::process::exit(chevron::daemon::client::print_version()),
            _ => {
                eprintln!("Usage: chevron daemon <serve|start|stop|status|version>");
                std::process::exit(1);
            }
        },
        #[cfg(feature = "host")]
        Some("host") => std::process::exit(chevron::host::run(&args[2..])),
        Some("version" | "--version" | "-V") => {
            println!(
                "chevron {} ({})",
                env!("CARGO_PKG_VERSION"),
                env!("CHEVRON_BUILD_ID")
            );
        }
        #[cfg(feature = "banner")]
        Some("banner") => {
            let scale = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2u32);
            let palette = args.get(3).map_or("cyber", String::as_str);
            let banner_type = args.get(4).map(String::as_str);
            let title = args.get(5).map(String::as_str);
            banner::generate(scale, palette, banner_type, title);
        }
        #[cfg(feature = "weather")]
        Some("weather") => weather::run(&args[2..]),
        _ => {
            let features: &[&str] = &[
                "path",
                "git",
                "nix-shell",
                "aws",
                "prompt",
                "tmux-title",
                "init",
                "status",
                "health",
                "doctor",
                "configure",
                #[cfg(feature = "daemon")]
                "daemon",
                #[cfg(feature = "daemon")]
                "capture",
                #[cfg(feature = "daemon")]
                "event",
                #[cfg(feature = "daemon")]
                "history",
                #[cfg(feature = "daemon")]
                "subscribe",
                #[cfg(feature = "host")]
                "host",
                "version",
                #[cfg(feature = "banner")]
                "banner",
                #[cfg(feature = "weather")]
                "weather",
            ];
            eprintln!("Usage: chevron <{}>", features.join("|"));
            std::process::exit(1);
        }
    }
}

/// Remove the retired v1 rendered-prompt cache on every shell start so a
/// still-pasted v1 instant-prompt block finds nothing to paint. Silent except
/// for a parent directory the cleanup refuses to touch: on that host another
/// user may control what that block paints, which is worth one line of stderr
/// per shell start. `CHEVRON_NOTICE=0` silences the line; `chevron doctor`
/// carries the detail either way.
fn retire_legacy_prompt_cache() {
    if let chevron::legacy_cache::Cleanup::UnsafeParent(reason) = chevron::legacy_cache::retire()
        && env::var("CHEVRON_NOTICE").ok().as_deref() != Some("0")
    {
        eprintln!("chevron: {reason}; run `chevron doctor` for details");
    }
}
