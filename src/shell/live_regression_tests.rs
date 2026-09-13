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

fn live_functions() -> String {
    [
        "_chevron_live_callback",
        "_chevron_live_flush",
        "_chevron_live_render",
    ]
    .into_iter()
    .map(function)
    .collect::<Vec<_>>()
    .join("\n")
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
fn older_async_render_cannot_overwrite_new_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let script = format!(
        "{}\n{}\n{}",
        function("_chevron_start_async"),
        function("_chevron_async_callback"),
        r#"
zle() { :; }
chevron() { print -r -- "generation-$_chevron_async_request_id"; }
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
    // Live updates can overlap without a new precmd cycle as well.
    let same_cycle = script.replace("_chevron_async_gen=2", "_chevron_async_gen=1");
    assert_eq!(zsh(&same_cycle, tmp.path()), "generation-2\ngeneration-2\n");
}

#[test]
fn live_event_cwd_preserves_literal_backslash() {
    let tmp = tempfile::tempdir().unwrap();
    // Positive control rules out a broken harness or ordinary percent decoding.
    for name in [
        "repo withspace",
        r"repo\name withspace",
        "repo%name",
        "repo\n",
    ] {
        let cwd = tmp.path().join(name);
        std::fs::create_dir(&cwd).unwrap();
        let script = format!(
            "{}\n{}",
            live_functions(),
            r#"
_chevron_start_async() { print RENDER; }
EPOCHREALTIME=10
CHEVRON_LIVE_SCOPE=cwd
# The protocol leaves literal backslashes intact and encodes spaces.
wire=${PWD:A}
wire=${wire//\%/%25}
wire=${wire// /%20}
wire=${wire//$'\n'/%0A}
exec 3< <(print -r -- "EVENT cwd=$wire")
_chevron_live_callback 3
# A filtered event returns nonzero; assert observable render, not exit status.
:
"#
        );
        assert_eq!(zsh(&script, &cwd), "RENDER\n", "cwd={name:?}");
    }
}

#[test]
fn live_burst_eventually_renders_final_state() {
    let tmp = tempfile::tempdir().unwrap();
    let mut script = live_functions();
    script.push_str(
        r#"
# Deterministic event-loop harness: record timer registration, then deliver
# its readiness after the burst. Rendering completes synchronously here.
zle() { if [[ $1 == -F && -n $3 ]]; then timer_fd=$2; timer_callback=$3; fi; }
sleep() { :; }
_chevron_start_async() { rendered=$state; (( renders++ )); }
CHEVRON_LIVE_SCOPE=all
CHEVRON_LIVE=1
state=initial
renders=0
EPOCHREALTIME=10
exec 3< <(print -r -- 'EVENT topic=cmd cwd=/repo')
_chevron_live_callback 3
print -r -- "first:$rendered"
EPOCHREALTIME=10.05
state=intermediate
exec 4< <(print -r -- 'EVENT topic=git cwd=/repo')
_chevron_live_callback 4
state=final
exec 5< <(print -r -- 'EVENT topic=cmd cwd=/repo')
_chevron_live_callback 5
EPOCHREALTIME=10.15
if [[ -n $timer_callback ]]; then "$timer_callback" "$timer_fd"; fi
print -r -- "last:$rendered renders:$renders"
"#,
    );
    assert_eq!(
        zsh(&script, tmp.path()),
        "first:initial\nlast:final renders:2\n"
    );
}

#[test]
fn stopping_live_cancels_trailing_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let script = format!(
        "{}\n{}\n{}",
        live_functions(),
        function("_chevron_stop_live"),
        r#"
zle() { :; }
sleep() { :; }
_chevron_start_async() { print UNEXPECTED_RENDER; }
CHEVRON_LIVE=1
CHEVRON_LIVE_SCOPE=all
_chevron_live_last_ms=10000
EPOCHREALTIME=10.05
exec 3< <(print -r -- 'EVENT cwd=/repo')
_chevron_live_callback 3
old_timer=$_chevron_live_timer_fd
[[ -n $old_timer ]] || exit 2
_chevron_stop_live
[[ -z $_chevron_live_timer_fd ]] || exit 3
# A callback already queued by ZLE must also become harmless.
_chevron_live_flush "$old_timer"
print STOPPED
"#
    );
    assert_eq!(zsh(&script, tmp.path()), "STOPPED\n");
}
