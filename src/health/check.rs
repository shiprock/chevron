#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Critical,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Check {
    /// Machine-readable name (used by phase-3 `--check <name>` and `--json`).
    #[allow(dead_code)]
    pub name: &'static str,
    pub label: &'static str,
    pub value: String,
    pub severity: Severity,
    pub hint: Option<String>,
}

impl Check {
    pub fn ok(name: &'static str, label: &'static str, value: impl Into<String>) -> Self {
        Self {
            name,
            label,
            value: value.into(),
            severity: Severity::Ok,
            hint: None,
        }
    }

    pub fn warn(
        name: &'static str,
        label: &'static str,
        value: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            name,
            label,
            value: value.into(),
            severity: Severity::Warn,
            hint: Some(hint.into()),
        }
    }

    pub fn critical(
        name: &'static str,
        label: &'static str,
        value: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            name,
            label,
            value: value.into(),
            severity: Severity::Critical,
            hint: Some(hint.into()),
        }
    }

    pub fn unknown(name: &'static str, label: &'static str, value: impl Into<String>) -> Self {
        Self {
            name,
            label,
            value: value.into(),
            severity: Severity::Unknown,
            hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Check, Severity};

    #[test]
    fn ok_has_no_hint() {
        let c = Check::ok("load", "System Load", "0.3");
        assert_eq!(c.severity, Severity::Ok);
        assert!(c.hint.is_none());
    }

    #[test]
    fn warn_carries_hint() {
        let c = Check::warn("memory", "Memory", "95%", "free up memory");
        assert_eq!(c.severity, Severity::Warn);
        assert_eq!(c.hint.as_deref(), Some("free up memory"));
    }

    #[test]
    fn critical_is_distinct_from_warn() {
        let c = Check::critical("disk", "Disk", "99%", "clean up");
        assert_eq!(c.severity, Severity::Critical);
    }
}
