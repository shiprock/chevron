//! Preset definitions. Each preset is a constructor producing a
//! distinct `AnswerSet`. The wizard's user picks a preset as the
//! starting point, then customizes individual choices.

use super::answers::{AnswerSet, ColorScheme, Features, IconMode, Language, Preset};

#[must_use]
pub fn from_preset(preset: Preset) -> AnswerSet {
    match preset {
        Preset::Lean => lean(),
        Preset::Pure => pure(),
        Preset::Rainbow => rainbow(),
        Preset::Classic => classic(),
        Preset::Minimal => minimal(),
    }
}

/// Chevron's default — full powerline, all language detectors on.
fn lean() -> AnswerSet {
    AnswerSet {
        icons: IconMode::NerdFont,
        color_scheme: ColorScheme::Powerline,
        features: Features::default(),
        languages: Language::all().to_vec(),
        segment_order: Vec::new(),
        disabled_segments: Vec::new(),
        source_preset: Some(Preset::Lean),
    }
}

/// Minimalist path-and-git only. Inspired by `pure-prompt`.
fn pure() -> AnswerSet {
    AnswerSet {
        icons: IconMode::Unicode,
        color_scheme: ColorScheme::Greyscale,
        features: Features {
            transient: false, // pure prompts don't collapse
            ..Features::default()
        },
        languages: Vec::new(),
        segment_order: vec!["path", "git", "character"],
        disabled_segments: vec![
            "venv",
            "username",
            "hostname",
            "nix_shell",
            "aws",
            "k8s",
            "node",
            "python",
            "rust_toolchain",
            "status",
            "cmd_duration",
            "jobs",
        ],
        source_preset: Some(Preset::Pure),
    }
}

/// Maximal: all segments + saturated colors + Nerd Font.
fn rainbow() -> AnswerSet {
    AnswerSet {
        icons: IconMode::NerdFont,
        color_scheme: ColorScheme::Rainbow,
        features: Features::default(),
        languages: Language::all().to_vec(),
        segment_order: Vec::new(),
        disabled_segments: Vec::new(),
        source_preset: Some(Preset::Rainbow),
    }
}

/// `user@host:path $` — non-powerline traditional shell prompt look.
fn classic() -> AnswerSet {
    AnswerSet {
        icons: IconMode::Unicode,
        color_scheme: ColorScheme::Powerline, // still color, just no triangles
        features: Features {
            transient: false,
            ..Features::default()
        },
        languages: vec![Language::Node, Language::Python, Language::Rust],
        segment_order: vec!["username", "hostname", "path", "git", "character"],
        disabled_segments: vec![
            "venv",
            "nix_shell",
            "aws",
            "k8s",
            "status",
            "cmd_duration",
            "jobs",
        ],
        source_preset: Some(Preset::Classic),
    }
}

/// ASCII-only safe-mode for broken terminals or scripted output.
fn minimal() -> AnswerSet {
    AnswerSet {
        icons: IconMode::Ascii,
        color_scheme: ColorScheme::NoColor,
        features: Features {
            transient: false,
            osc133: false,
            // Safe-mode / scripted output: no background live subscriber.
            live: false,
            ..Features::default()
        },
        languages: Vec::new(),
        segment_order: vec!["path", "character"],
        disabled_segments: vec![
            "venv",
            "username",
            "hostname",
            "nix_shell",
            "aws",
            "k8s",
            "git",
            "node",
            "python",
            "rust_toolchain",
            "status",
            "cmd_duration",
            "jobs",
        ],
        source_preset: Some(Preset::Minimal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_produces_distinct_answers() {
        // Sanity: no preset accidentally aliases another. Compare on the
        // (icons, color_scheme, segment_order) tuple which captures the
        // visible identity of each style.
        let answers: Vec<_> = Preset::all().iter().map(|p| from_preset(*p)).collect();
        for i in 0..answers.len() {
            for j in (i + 1)..answers.len() {
                let a = &answers[i];
                let b = &answers[j];
                let identity_a = (a.icons, a.color_scheme, a.segment_order.clone());
                let identity_b = (b.icons, b.color_scheme, b.segment_order.clone());
                assert_ne!(
                    identity_a,
                    identity_b,
                    "presets {:?} and {:?} have identical identity",
                    Preset::all()[i],
                    Preset::all()[j],
                );
            }
        }
    }

    #[test]
    fn lean_matches_chevron_defaults() {
        let a = from_preset(Preset::Lean);
        assert!(a.segment_order.is_empty(), "lean uses default order");
        assert!(a.disabled_segments.is_empty(), "lean disables nothing");
        assert_eq!(a.languages.len(), 3, "lean enables all languages");
        assert_eq!(a.icons, IconMode::NerdFont);
    }

    #[test]
    fn pure_disables_most_segments() {
        let a = from_preset(Preset::Pure);
        assert_eq!(a.segment_order, ["path", "git", "character"]);
        assert!(
            a.languages.is_empty(),
            "pure does not show language segments"
        );
        assert_eq!(a.color_scheme, ColorScheme::Greyscale);
    }

    #[test]
    fn minimal_is_ascii_no_color() {
        let a = from_preset(Preset::Minimal);
        assert_eq!(a.icons, IconMode::Ascii);
        assert_eq!(a.color_scheme, ColorScheme::NoColor);
        assert!(!a.features.osc133, "minimal opts out of OSC 133");
    }

    #[test]
    fn every_preset_records_its_source() {
        for p in Preset::all() {
            let a = from_preset(p);
            assert_eq!(a.source_preset, Some(p));
        }
    }
}
