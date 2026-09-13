//! Executable regressions against the generated Zsh functions. Only external
//! rendering and ZLE UI operations are stubbed; filtering and lifecycle logic
//! are the production code.
use std::process::Command;

fn function(name: &str) -> String {
    let script = super::init_zsh();
    let start = script.find(&format!("{name}() {{")).unwrap();
    let end = start + script[start..].find("\n}\n").unwrap() + 3;
    script[start..end].to_string()
}

fn zsh(script: &str, cwd: &std::path::Path) -> String {
    let output = Command::new("zsh")
        .args(["-f", "-c", script])
        .current_dir(cwd)
        .env_remove("TMUX")
        .output()
        .expect("regression requires zsh");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
#[ignore = "known bug beads_plx-ntc: run explicitly until fixed"]
fn older_async_render_cannot_overwrite_new_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let script = format!(
        "{}\n{}\n{}",
        function("_chevron_start_async"),
        function("_chevron_async_callback"),
        r#"
zle() { :; }
chevron() { print -r -- "generation-$_chevron_async_gen"; }
_chevron_make_prompt() { REPLY=$1; }
_chevron_async_gen=1
_chevron_start_async 0 0 0
oldfd=$_chevron_async_fd
_chevron_async_gen=2
_chevron_start_async 0 0 0
newfd=$_chevron_async_fd
# Deliver completions in reverse order deterministically, without sleeps.
_chevron_async_callback $newfd
print -r -- "$PROMPT"
_chevron_async_callback $oldfd
print -r -- "$PROMPT"
"#
    );
    assert_eq!(zsh(&script, tmp.path()), "generation-2\ngeneration-2\n");
}

#[test]
#[ignore = "known bug beads_plx-8kl: run explicitly until fixed"]
fn live_event_cwd_preserves_literal_backslash() {
    let tmp = tempfile::tempdir().unwrap();
    // Positive control rules out a broken harness or ordinary percent decoding.
    for name in ["repo withspace", r"repo\name withspace"] {
        let cwd = tmp.path().join(name);
        std::fs::create_dir(&cwd).unwrap();
        let script = format!(
            "{}\n{}",
            function("_chevron_live_callback"),
            r#"
_chevron_start_async() { print RENDER; }
EPOCHREALTIME=10
CHEVRON_LIVE_SCOPE=cwd
# The protocol leaves literal backslashes intact and encodes spaces.
wire=${PWD:A}
wire=${wire// /%20}
exec 3< <(print -r -- "EVENT cwd=$wire")
_chevron_live_callback 3
# A filtered event returns nonzero; assert observable render, not exit status.
:
"#
        );
        assert_eq!(zsh(&script, &cwd), "RENDER\n", "cwd={name:?}");
    }
}
