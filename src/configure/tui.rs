//! Inquire-based interactive question flow. Each `ask_*` function returns
//! `InquireResult<T>`; the top-level dispatcher propagates errors so the
//! caller can map Ctrl-C / cancellation to exit codes.

use inquire::{Confirm, InquireError, MultiSelect, Select};

use super::answers::{AnswerSet, CaptureMode, Features, IconMode, Language, Preset};
use super::presets;
use super::preview;

pub type InquireResult<T> = Result<T, InquireError>;

/// Run the full wizard interactively. Returns `Ok(Some(answers))` on
/// completion, `Ok(None)` if the user declined to continue at the welcome
/// screen, or an `InquireError` if a question was cancelled.
pub fn run_wizard() -> InquireResult<Option<AnswerSet>> {
    show_welcome();
    if !Confirm::new("Continue?").with_default(true).prompt()? {
        return Ok(None);
    }

    let mut answers = ask_preset()?;
    answers.icons = ask_icons(answers.icons)?;
    show_preview("Current preview:", &answers);

    if Confirm::new("Customise feature toggles? (defaults are sensible)")
        .with_default(false)
        .prompt()?
    {
        answers.features = ask_features(answers.features)?;
    }

    answers.languages = ask_languages(&answers.languages)?;

    Ok(Some(answers))
}

fn show_welcome() {
    println!();
    println!("chevron configure");
    println!();
    println!("This will write ~/.config/chevron/config.toml.");
    println!("An existing config will be backed up to config.toml.bak.");
    println!();
    println!("Font check — you should see a triangle, a logo, and a solid block:");
    println!("  \u{e0b0}  \u{f09b}  \u{2588}");
    println!("If any of those look wrong, abort (Ctrl-C) and install a Nerd Font.");
    println!();
}

fn ask_preset() -> InquireResult<AnswerSet> {
    println!();
    println!("Preview of each preset (rendered against your current directory):");
    println!();
    for p in Preset::all() {
        let preview = preview::render_preview(&presets::from_preset(p));
        // The preview already contains its own ANSI escapes, including a
        // trailing reset, so we don't need to wrap it.
        println!("  {:8}  {preview}", p.name());
        println!("            \x1b[2m{}\x1b[0m", p.tagline());
    }
    println!();

    // Map "lean — full powerline …" labels back to the Preset enum.
    let options: Vec<String> = Preset::all()
        .iter()
        .map(|p| format!("{} — {}", p.name(), p.tagline()))
        .collect();
    let choice = Select::new("Pick a starting preset:", options.clone())
        .with_starting_cursor(0)
        .with_help_message("This is just a starting point — you can customise next")
        .prompt()?;

    let idx = options.iter().position(|s| s == &choice).unwrap_or(0);
    let preset = Preset::all().get(idx).copied().unwrap_or(Preset::Lean);
    Ok(presets::from_preset(preset))
}

fn ask_icons(current: IconMode) -> InquireResult<IconMode> {
    let options = vec![
        "Nerd Font (recommended — requires a Nerd-Font terminal)",
        "Unicode (no Nerd Font glyphs)",
        "ASCII (pasteable / safe for any terminal)",
    ];
    let start = match current {
        IconMode::NerdFont => 0,
        IconMode::Unicode => 1,
        IconMode::Ascii => 2,
    };
    let chosen = Select::new("Icon set:", options)
        .with_starting_cursor(start)
        .with_help_message("v1 stores this in config; actual icon swapping is upcoming")
        .prompt()?;
    Ok(if chosen.starts_with("Nerd") {
        IconMode::NerdFont
    } else if chosen.starts_with("Unicode") {
        IconMode::Unicode
    } else {
        IconMode::Ascii
    })
}

fn ask_features(current: Features) -> InquireResult<Features> {
    let mut f = current;
    f.osc133 = Confirm::new("Emit OSC 133 terminal markers? (Ghostty, WezTerm, iTerm2, Kitty)")
        .with_default(f.osc133)
        .prompt()?;
    f.transient = Confirm::new("Enable transient prompt? (collapses to ❯ after Enter)")
        .with_default(f.transient)
        .prompt()?;
    f.async_render = Confirm::new("Enable async render? (instant prompt; experimental in zsh)")
        .with_default(f.async_render)
        .prompt()?;
    f.history = Confirm::new("Record command history to chevrond? (powers history/search)")
        .with_default(f.history)
        .prompt()?;
    f.live = Confirm::new("Enable live prompt updates? (refresh on filesystem change)")
        .with_default(f.live)
        .prompt()?;

    let capture_options = vec!["Off", "Per-directory (opt-in)", "Always-on"];
    let capture_start = match f.capture {
        CaptureMode::Off => 0,
        CaptureMode::PerCwd => 1,
        CaptureMode::AlwaysOn => 2,
    };
    let capture_chosen = Select::new("Command output capture:", capture_options)
        .with_starting_cursor(capture_start)
        .with_help_message(
            "Off = no capture; Per-dir = enable per-cwd; Always = capture every command",
        )
        .prompt()?;
    f.capture = match capture_chosen {
        "Per-directory (opt-in)" => CaptureMode::PerCwd,
        "Always-on" => CaptureMode::AlwaysOn,
        _ => CaptureMode::Off,
    };
    Ok(f)
}

fn ask_languages(current: &[Language]) -> InquireResult<Vec<Language>> {
    let all = Language::all();
    let options: Vec<&str> = all.iter().map(|l| l.display_name()).collect();
    let defaults: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, l)| current.contains(l))
        .map(|(i, _)| i)
        .collect();
    let chosen = MultiSelect::new("Language segments to enable (space to toggle):", options)
        .with_default(&defaults)
        .prompt()?;
    Ok(all
        .iter()
        .copied()
        .filter(|l| chosen.contains(&l.display_name()))
        .collect())
}

fn show_preview(heading: &str, answers: &AnswerSet) {
    println!();
    println!("{heading}");
    println!("  {}", preview::render_preview(answers));
    println!();
}
