// Test modules contain many `unsafe { std::env::set_var / remove_var }` blocks
// (mandatory unsafe in Rust 2024 edition). Per-block SAFETY comments would be
// noise; the mutations are made race-free by `#[serial]` annotations on the
// env-var-touching tests. Production code is still subject to the lint.
#![cfg_attr(test, allow(clippy::undocumented_unsafe_blocks))]

#[cfg(feature = "banner")]
mod banner;
mod color;
mod config;
mod health;
mod repo_status;
mod segments;
mod shell;
mod sysinfo;
#[cfg(feature = "weather")]
mod weather;

use std::env;
use std::path::Path;

fn main() {
    let args: Vec<String> = env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("path") => {
            let home = env::var("HOME").unwrap_or_default();
            let pwd = env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let max_dir_size = args.get(2).and_then(|s| s.parse::<usize>().ok());
            print!("{}", segments::path::render(&home, &pwd, max_dir_size));
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
            let shell = env::var("PLX_SHELL").unwrap_or_default();
            // Only the prompt line gets escape wrapping; tmux title is plain.
            if let Some((prompt, title)) = raw.split_once('\n') {
                print!("{}\n{}", color::wrap_for_shell(&shell, prompt), title);
            } else {
                print!("{}", color::wrap_for_shell(&shell, &raw));
            }
        }
        Some("tmux-title") => {
            let home = env::var("HOME").unwrap_or_default();
            let pwd = env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            println!("{}", segments::tmux_title::render(&home, &pwd));
        }
        Some("init") => match args.get(2).map(String::as_str) {
            Some("zsh") => print!("{}", shell::init_zsh()),
            Some("bash") => print!("{}", shell::init_bash()),
            Some("fish") => print!("{}", shell::init_fish()),
            _ => {
                eprintln!("Usage: plx init <zsh|bash|fish>");
                std::process::exit(1);
            }
        },
        Some("status") => repo_status::run(),
        Some("health") => std::process::exit(health::run(&args[2..])),
        Some("version" | "--version" | "-V") => {
            println!("plx {}", env!("CARGO_PKG_VERSION"));
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
                "version",
                #[cfg(feature = "banner")]
                "banner",
                #[cfg(feature = "weather")]
                "weather",
            ];
            eprintln!("Usage: plx <{}>", features.join("|"));
            std::process::exit(1);
        }
    }
}
