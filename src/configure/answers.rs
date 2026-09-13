//! The data carrier the wizard fills in. Pure data — no I/O. emit.rs
//! turns this into config.toml + a shell snippet.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerSet {
    pub icons: IconMode,
    pub color_scheme: ColorScheme,
    pub features: Features,
    pub languages: Vec<Language>,
    /// Segments in display order. Empty means "use chevron's default order".
    pub segment_order: Vec<&'static str>,
    /// Segments explicitly disabled. (Order omits these too if listed.)
    pub disabled_segments: Vec<&'static str>,
    /// Which preset produced this set — kept so the emitted TOML can label
    /// itself, but the wizard's other answers can diverge after the user
    /// customises. None means "manual / no preset selected".
    pub source_preset: Option<Preset>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Lean,
    Pure,
    Rainbow,
    Classic,
    Minimal,
}

impl Preset {
    pub fn name(self) -> &'static str {
        match self {
            Preset::Lean => "lean",
            Preset::Pure => "pure",
            Preset::Rainbow => "rainbow",
            Preset::Classic => "classic",
            Preset::Minimal => "minimal",
        }
    }

    pub fn tagline(self) -> &'static str {
        match self {
            Preset::Lean => "chevron default — full powerline, all segments",
            Preset::Pure => "minimal — path + git only, no powerline",
            Preset::Rainbow => "maximal color — all segments, every color",
            Preset::Classic => "user@host:path style — no powerline triangles",
            Preset::Minimal => "ASCII-only — path + character, no colors",
        }
    }

    pub fn all() -> [Preset; 5] {
        [
            Preset::Lean,
            Preset::Pure,
            Preset::Rainbow,
            Preset::Classic,
            Preset::Minimal,
        ]
    }

    pub fn from_name(s: &str) -> Option<Preset> {
        match s {
            "lean" => Some(Preset::Lean),
            "pure" => Some(Preset::Pure),
            "rainbow" => Some(Preset::Rainbow),
            "classic" => Some(Preset::Classic),
            "minimal" => Some(Preset::Minimal),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconMode {
    /// Powerline glyphs + Nerd Font icons (branch, etc.). Default.
    NerdFont,
    /// Unicode-only — no Nerd Font glyphs.
    Unicode,
    /// ASCII fallback — for terminals without UTF-8 or for pasteable output.
    Ascii,
}

impl IconMode {
    pub fn config_name(self) -> &'static str {
        match self {
            IconMode::NerdFont => "nerd",
            IconMode::Unicode => "unicode",
            IconMode::Ascii => "ascii",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    /// Powerline-style colored backgrounds. Chevron default.
    Powerline,
    /// Greyscale — same shape, neutral colors.
    Greyscale,
    /// Maximal color saturation across all segments.
    Rainbow,
    /// No colors at all (pasteable / monochrome terminals).
    NoColor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Five booleans matches the surface area of chevron's shell-init env vars
// (OSC133/TRANSIENT/ASYNC/HISTORY/LIVE). Splitting them into a sub-struct
// just to dodge this lint adds indirection without clarity.
#[allow(clippy::struct_excessive_bools)]
pub struct Features {
    pub osc133: bool,
    pub transient: bool,
    pub async_render: bool,
    pub history: bool,
    pub live: bool,
    pub capture: CaptureMode,
}

impl Default for Features {
    /// Matches chevron's shell-init defaults: most-likely-to-want = on,
    /// performance/opt-in features = off.
    fn default() -> Self {
        Features {
            osc133: true,
            transient: true,
            async_render: false,
            history: true,
            live: true,
            capture: CaptureMode::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Off,
    PerCwd,
    AlwaysOn,
}

impl CaptureMode {
    pub fn env_value(self) -> Option<&'static str> {
        match self {
            CaptureMode::Off => None, // omit from snippet
            CaptureMode::PerCwd => Some("per_cwd"),
            CaptureMode::AlwaysOn => Some("1"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Node,
    Python,
    Rust,
}

impl Language {
    pub fn segment_name(self) -> &'static str {
        match self {
            Language::Node => "node",
            Language::Python => "python",
            Language::Rust => "rust_toolchain",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Language::Node => "Node.js",
            Language::Python => "Python",
            Language::Rust => "Rust",
        }
    }

    pub fn all() -> [Language; 3] {
        [Language::Node, Language::Python, Language::Rust]
    }
}

impl Default for AnswerSet {
    /// The chevron-out-of-the-box experience — equivalent to `Preset::Lean`.
    fn default() -> Self {
        AnswerSet {
            icons: IconMode::NerdFont,
            color_scheme: ColorScheme::Powerline,
            features: Features::default(),
            languages: Language::all().to_vec(),
            segment_order: Vec::new(),
            disabled_segments: Vec::new(),
            source_preset: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_name_round_trip() {
        for p in Preset::all() {
            assert_eq!(Preset::from_name(p.name()), Some(p));
        }
    }

    #[test]
    fn preset_from_unknown_name_is_none() {
        assert_eq!(Preset::from_name("zzz"), None);
    }

    #[test]
    fn default_answers_equal_chevron_out_of_box() {
        let a = AnswerSet::default();
        assert_eq!(a.icons, IconMode::NerdFont);
        assert_eq!(a.color_scheme, ColorScheme::Powerline);
        assert!(a.features.osc133);
        assert!(a.features.transient);
        assert!(!a.features.async_render);
        assert!(a.features.history);
        assert!(a.features.live);
        assert_eq!(a.features.capture, CaptureMode::Off);
        assert!(a.segment_order.is_empty());
    }

    #[test]
    fn language_segment_name_matches_registry_keys() {
        // Wire-protocol check: these names must equal the keys in
        // segments::registry::segment_by_name.
        assert_eq!(Language::Node.segment_name(), "node");
        assert_eq!(Language::Python.segment_name(), "python");
        assert_eq!(Language::Rust.segment_name(), "rust_toolchain");
    }
}
