//! Build a synthetic `PromptContext` and call the real renderer so the
//! preview matches exactly what the user will see at their next prompt.
//! Uses the user's real $HOME and $PWD so the preview reflects their
//! actual environment (the killer wizard UX trick).

use std::env;

use crate::config::Config;
use crate::segments::prompt::{self, PromptContext};

use super::answers::AnswerSet;

/// Render a one-line preview of what the prompt would look like with these
/// answers. The preview runs the real render pipeline (libgit2 + segments),
/// just with a synthetic `PromptContext` whose `config` is built from
/// `answers`. The user's real cwd / home are used so a git repo shows their
/// actual branch.
#[must_use]
pub fn render_preview(answers: &AnswerSet) -> String {
    let cfg = build_config(answers);
    let home = env::var("HOME").unwrap_or_default();
    let pwd =
        env::current_dir().map_or_else(|_| home.clone(), |p| p.to_string_lossy().into_owned());
    let mut ctx = PromptContext {
        home,
        pwd,
        max_dir_size: Some(20),
        exit_status: 0,
        duration_ms: 0,
        job_count: 0,
        in_tmux: false, // suppress tmux title even when running inside tmux
        repo_status: None,
        config: cfg,
    };
    let rendered = prompt::render(&mut ctx);
    // The renderer never emits newlines in non-tmux mode, but defend against
    // future changes — a stray \n would mangle the preset list.
    rendered.split('\n').next().unwrap_or("").to_string()
}

/// Build a `Config` from an `AnswerSet` by routing through the same TOML
/// emitter we use for the on-disk file. This guarantees preview and written
/// config can never diverge.
fn build_config(answers: &AnswerSet) -> Config {
    let toml_str = super::emit::to_config_toml(answers);
    toml::from_str(&toml_str).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::answers::Preset;
    use super::super::presets::from_preset;
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn preview_returns_a_single_line() {
        // SAFETY: test-only env manipulation, serialised via #[serial].
        unsafe { env::remove_var("TMUX") };
        let s = render_preview(&from_preset(Preset::Lean));
        assert!(!s.contains('\n'), "preview must be single line: {s:?}");
        assert!(!s.is_empty(), "preview should not be empty");
    }

    #[test]
    #[serial]
    fn preview_is_non_empty_for_every_preset() {
        unsafe { env::remove_var("TMUX") };
        for p in Preset::all() {
            let s = render_preview(&from_preset(p));
            assert!(!s.is_empty(), "{p:?} produced empty preview");
        }
    }

    #[test]
    #[serial]
    fn build_config_round_trips_through_toml() {
        // Every preset's TOML must parse back into a valid Config.
        for p in Preset::all() {
            let cfg = build_config(&from_preset(p));
            if p == Preset::Lean || p == Preset::Rainbow {
                assert!(cfg.segments.order.is_empty(), "{p:?} should not pin order");
            } else {
                assert!(!cfg.segments.order.is_empty(), "{p:?} should pin order");
            }
        }
    }
}
