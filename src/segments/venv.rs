use crate::color::{RST, fg};

#[must_use]
pub fn render_prefix() -> String {
    let Some(path) = std::env::var("VIRTUAL_ENV").ok().filter(|v| !v.is_empty()) else {
        return String::new();
    };

    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    if name.is_empty() {
        return String::new();
    }

    format!("{}({name}) {RST}", fg(2))
}

#[cfg(test)]
mod tests {
    use super::render_prefix;
    use crate::color::{RST, fg};
    use serial_test::serial;

    #[test]
    #[serial]
    fn empty_when_unset() {
        // SAFETY: test-only
        unsafe { std::env::remove_var("VIRTUAL_ENV") };
        assert_eq!(render_prefix(), "");
    }

    #[test]
    #[serial]
    fn renders_venv_name() {
        // SAFETY: test-only
        unsafe { std::env::set_var("VIRTUAL_ENV", "/home/user/.venvs/myenv") };
        let out = render_prefix();
        assert!(out.contains("(myenv)"), "expected (myenv) in: {out}");
        assert!(out.contains(fg(2)), "expected green fg(2) in: {out}");
        assert!(out.contains(RST), "expected reset in: {out}");
        unsafe { std::env::remove_var("VIRTUAL_ENV") };
    }

    #[test]
    #[serial]
    fn empty_when_set_but_empty() {
        // SAFETY: test-only
        unsafe { std::env::set_var("VIRTUAL_ENV", "") };
        assert_eq!(render_prefix(), "");
        unsafe { std::env::remove_var("VIRTUAL_ENV") };
    }
}
