#[must_use]
#[allow(clippy::too_many_lines)]
pub fn init_zsh() -> &'static str {
    r#"_chevron_preexec() {
    _chevron_cmd_start=$EPOCHREALTIME
    _chevron_cmd_title="$1"
}
_chevron_precmd() {
    local exit_status=$?
    local duration_ms=0
    if [[ -n "$_chevron_cmd_start" ]]; then
        duration_ms=$(( ($EPOCHREALTIME - _chevron_cmd_start) * 1000 ))
        duration_ms=${duration_ms%.*}
        unset _chevron_cmd_start
    fi
    # Post-execution duration tag: if the just-completed command took
    # longer than the threshold, emit a dim duration line below its
    # output and above the next prompt. This preserves timing info in
    # scrollback even after the transient prompt has collapsed. Opt out
    # via CHEVRON_TRANSIENT=0 (also disables the transient itself);
    # threshold configurable via CHEVRON_TRANSIENT_DURATION_MS (default
    # 2000ms). Printed in precmd, so it lands naturally between the
    # command output and the next PROMPT zsh is about to draw.
    if [[ "${CHEVRON_TRANSIENT:-1}" != "0" ]] && (( duration_ms >= ${CHEVRON_TRANSIENT_DURATION_MS:-2000} )); then
        if (( duration_ms >= 60000 )); then
            local _chevron_mins=$(( duration_ms / 60000 ))
            local _chevron_rem=$(( (duration_ms % 60000) / 1000 ))
            printf '\033[2m %dm %ds\033[0m\n' "$_chevron_mins" "$_chevron_rem"
        else
            local _chevron_secs=$(( duration_ms / 1000 ))
            local _chevron_frac=$(( (duration_ms % 1000) / 100 ))
            printf '\033[2m %d.%ds\033[0m\n' "$_chevron_secs" "$_chevron_frac"
        fi
    fi
    local job_count=${(%):-%j}
    local chevron_output=""
    # Async fast path (CHEVRON_ASYNC=1): try the cached prompt from the
    # previous render; on hit, set PROMPT immediately and spawn a
    # background refresh whose result will trigger a redraw via the
    # zle -F callback. On miss (cwd changed, first prompt of shell), fall
    # through to the synchronous path below.
    if [[ "${CHEVRON_ASYNC:-0}" != "0" && -r "$_chevron_cache_file" ]]; then
        local _chevron_cached_pwd
        IFS= read -r _chevron_cached_pwd < "$_chevron_cache_file"
        if [[ "$_chevron_cached_pwd" == "$PWD" ]]; then
            chevron_output=$(sed -n '2,$p' "$_chevron_cache_file" 2>/dev/null)
            if [[ -n "$chevron_output" ]]; then
                _chevron_start_async "$exit_status" "$duration_ms" "$job_count"
            fi
        fi
    fi
    if [[ -z "$chevron_output" ]]; then
        # Synchronous render. Sets CHEVRON_CACHE_FILE only when async is
        # enabled — no point in paying the I/O for users who'll never
        # read the cache.
        if [[ "${CHEVRON_ASYNC:-0}" != "0" ]]; then
            chevron_output="$(CHEVRON_CACHE_FILE="$_chevron_cache_file" chevron prompt 20 $exit_status $duration_ms $job_count)"
        else
            chevron_output="$(chevron prompt 20 $exit_status $duration_ms $job_count)"
        fi
    fi
    PROMPT="${chevron_output%%$'\n'*} "
    # Stash transient prompt for the accept-line widget. Colour reflects
    # the just-completed command's exit status, so scrollback retains a
    # visual cue (green/red chevron) even after the full prompt collapses.
    if (( exit_status == 0 )); then
        _chevron_transient_prompt='%F{green}❯%f '
    else
        _chevron_transient_prompt='%F{red}❯%f '
    fi
    if [[ -n "$TMUX" && "$chevron_output" == *$'\n'* ]]; then
        local tmux_title="${chevron_output#*$'\n'}"
        local priority=$(tmux show-options -w -v @priority_title 2>/dev/null)
        if [[ -z "$priority" ]]; then
            tmux set-option -p @custom_title "" \; set-option -p @dir_title "$tmux_title" \; rename-window "$tmux_title"
        fi
    fi
    unset _chevron_cmd_title
}
# Async fast-path machinery (chevron-7fs.5). Off by default; enable with
# CHEVRON_ASYNC=1. The background process renders a fresh prompt and
# writes it to its stdout (a pipe opened via process substitution). When
# the pipe becomes readable (i.e., chevron has finished + closed stdout),
# zsh fires _chevron_async_callback, which reads the fresh output, sets
# PROMPT, and triggers a redraw.
_chevron_start_async() {
    local exit_status=$1 duration_ms=$2 job_count=$3
    exec {_chevron_async_fd}< <(CHEVRON_CACHE_FILE="$_chevron_cache_file" chevron prompt 20 "$exit_status" "$duration_ms" "$job_count" 2>/dev/null)
    zle -F "$_chevron_async_fd" _chevron_async_callback
}
_chevron_async_callback() {
    local fd=$1
    local fresh
    fresh=$(cat <&$fd 2>/dev/null)
    zle -F "$fd"
    exec {fd}<&-
    if [[ -n "$fresh" ]]; then
        PROMPT="${fresh%%$'\n'*} "
        if [[ -n "$TMUX" && "$fresh" == *$'\n'* ]]; then
            local _chevron_tmux_title="${fresh#*$'\n'}"
            local _chevron_priority=$(tmux show-options -w -v @priority_title 2>/dev/null)
            if [[ -z "$_chevron_priority" ]]; then
                tmux set-option -p @custom_title "" \; set-option -p @dir_title "$_chevron_tmux_title" \; rename-window "$_chevron_tmux_title"
            fi
        fi
        zle reset-prompt
    fi
}
# Transient prompt: rewrite the just-issued prompt line to a minimal stub
# (single chevron) so scrollback stays clean and copy-paste captures only
# the command, not the prompt chrome. Disable with CHEVRON_TRANSIENT=0.
_chevron_accept_line() {
    PROMPT="$_chevron_transient_prompt"
    zle .reset-prompt
    # Chain through any prior accept-line override (zsh-autosuggest, etc.)
    # if one exists; otherwise fall back to the built-in.
    if [[ -n "$_chevron_orig_accept_line" ]]; then
        zle "$_chevron_orig_accept_line"
    else
        zle .accept-line
    fi
}
# Cache file for the async fast path. Owner-only directory so the cached
# prompt content (which may include path/branch info) isn't world-readable.
_chevron_cache_file="${XDG_RUNTIME_DIR:-/tmp}/chevron-${UID:-$(id -u)}/last-prompt"
[[ -d "${_chevron_cache_file:h}" ]] || mkdir -p -m 700 "${_chevron_cache_file:h}" 2>/dev/null
autoload -Uz add-zsh-hook
add-zsh-hook precmd _chevron_precmd
add-zsh-hook preexec _chevron_preexec
if [[ "${CHEVRON_TRANSIENT:-1}" != "0" ]]; then
    # Preserve any existing override (load order matters — initialise
    # chevron after other prompt tools for best compatibility).
    if [[ "${widgets[accept-line]}" == user:* ]]; then
        _chevron_orig_accept_line="${widgets[accept-line]#user:}"
    fi
    zle -N accept-line _chevron_accept_line
fi
"#
}

#[must_use]
pub fn init_bash() -> &'static str {
    // Bash port intentionally skips the transient prompt that zsh and fish
    // implement. Bash has no clean equivalent of ZLE's accept-line override,
    // and the two viable workarounds (bind -x with cursor codes; DEBUG trap
    // with $BASH_COMMAND) both break on multi-line input, wrapped lines, and
    // compound commands. Same reason powerlevel10k stays zsh-only and
    // Starship's bash init doesn't transient-collapse. The post-exec
    // duration tag, however, ports cleanly and is enabled here.
    r#"_chevron_preexec() {
    [[ -n "$_chevron_in_precmd" ]] && return
    _chevron_cmd_start=${_chevron_cmd_start:-$EPOCHREALTIME}
}
_chevron_precmd() {
    local exit_status=$?
    _chevron_in_precmd=1
    local duration_ms=0
    if [[ -n "$_chevron_cmd_start" ]]; then
        duration_ms=$(LC_ALL=C awk "BEGIN { printf \"%d\", ($EPOCHREALTIME - $_chevron_cmd_start) * 1000 }")
        unset _chevron_cmd_start
    fi
    # Post-execution duration tag: a dim line shown between command output
    # and the next prompt when the command exceeded the threshold (default
    # 2000ms). Same logic and format as the zsh/fish init. Disable with
    # CHEVRON_TRANSIENT=0; threshold via CHEVRON_TRANSIENT_DURATION_MS.
    if [[ "${CHEVRON_TRANSIENT:-1}" != "0" ]] && (( duration_ms >= ${CHEVRON_TRANSIENT_DURATION_MS:-2000} )); then
        if (( duration_ms >= 60000 )); then
            local _chevron_mins=$(( duration_ms / 60000 ))
            local _chevron_rem=$(( (duration_ms % 60000) / 1000 ))
            printf '\033[2m %dm %ds\033[0m\n' "$_chevron_mins" "$_chevron_rem"
        else
            local _chevron_secs=$(( duration_ms / 1000 ))
            local _chevron_frac=$(( (duration_ms % 1000) / 100 ))
            printf '\033[2m %d.%ds\033[0m\n' "$_chevron_secs" "$_chevron_frac"
        fi
    fi
    local job_count=$(( $(jobs -p 2>/dev/null | wc -l) ))
    local chevron_output
    chevron_output="$(CHEVRON_SHELL=bash chevron prompt 20 $exit_status $duration_ms $job_count)"
    PS1="${chevron_output%%$'\n'*} "
    if [[ -n "$TMUX" && "$chevron_output" == *$'\n'* ]]; then
        local tmux_title="${chevron_output#*$'\n'}"
        local priority=$(tmux show-options -w -v @priority_title 2>/dev/null)
        if [[ -z "$priority" ]]; then
            tmux set-option -p @custom_title "" \; set-option -p @dir_title "$tmux_title" \; rename-window "$tmux_title"
        fi
    fi
    unset _chevron_in_precmd
}
trap '_chevron_preexec' DEBUG
PROMPT_COMMAND=_chevron_precmd
"#
}

#[must_use]
pub fn init_fish() -> &'static str {
    r#"function fish_prompt
    # Transient branch: short prompt with exit-coloured chevron. Triggered
    # when the user hits Enter — _chevron_transient_enter sets the flag and
    # forces a repaint before executing the command. We unset the flag here
    # so the next full prompt (after the command runs) renders normally.
    if set -q _chevron_transient_show
        set -e _chevron_transient_show
        set -l last_exit 0
        set -q _chevron_last_exit; and set last_exit $_chevron_last_exit
        if test $last_exit -eq 0
            set_color green
        else
            set_color red
        end
        echo -n '❯ '
        set_color normal
        return
    end

    set -l exit_status $status
    set -g _chevron_last_exit $exit_status
    set -l duration_ms $CMD_DURATION
    set -l job_count (count (jobs -p 2>/dev/null))
    set -l chevron_output (CHEVRON_SHELL=fish command chevron prompt 20 $exit_status $duration_ms $job_count)
    set -l lines (string split \n -- $chevron_output)
    echo -n "$lines[1] "
    if set -q TMUX; and test (count $lines) -gt 1
        set -l priority (tmux show-options -w -v @priority_title 2>/dev/null)
        if test -z "$priority"
            tmux set-option -p @custom_title "" \; set-option -p @dir_title "$lines[2]" \; rename-window "$lines[2]"
        end
    end
end

# Post-execution duration tag: fish exposes $CMD_DURATION directly, so we
# can hook fish_postexec without the timestamp arithmetic zsh needs.
function _chevron_postexec --on-event fish_postexec
    test "$CHEVRON_TRANSIENT" = "0"; and return
    set -l threshold $CHEVRON_TRANSIENT_DURATION_MS
    test -z "$threshold"; and set threshold 2000
    test $CMD_DURATION -lt $threshold; and return
    if test $CMD_DURATION -ge 60000
        set -l mins (math "$CMD_DURATION / 60000")
        set -l rem (math "($CMD_DURATION % 60000) / 1000")
        printf '\033[2m %dm %ds\033[0m\n' $mins $rem
    else
        set -l secs (math "$CMD_DURATION / 1000")
        set -l frac (math "($CMD_DURATION % 1000) / 100")
        printf '\033[2m %d.%ds\033[0m\n' $secs $frac
    end
end

# Transient prompt: bind Enter to a wrapper that flips the flag,
# repaints (which re-runs fish_prompt -> short form), and then executes.
# The deferred-event ordering matters here: `commandline -f` schedules
# operations to run after the current binding returns, so the flag stays
# set when fish_prompt fires for the repaint, then the execute follows.
function _chevron_transient_enter
    set -g _chevron_transient_show 1
    commandline -f repaint
    commandline -f execute
end
if test "$CHEVRON_TRANSIENT" != "0"
    bind \r _chevron_transient_enter
    bind \n _chevron_transient_enter
end
"#
}

#[cfg(test)]
mod tests {
    use super::{init_bash, init_fish, init_zsh};

    #[test]
    fn zsh_contains_hooks() {
        let out = init_zsh();
        assert!(out.contains("add-zsh-hook precmd _chevron_precmd"));
        assert!(out.contains("add-zsh-hook preexec _chevron_preexec"));
        assert!(out.contains("EPOCHREALTIME"));
        assert!(out.contains("chevron prompt"));
        assert!(
            out.contains("@priority_title"),
            "should check priority title"
        );
        assert!(out.contains("rename-window"), "should rename tmux window");
    }

    #[test]
    fn zsh_registers_transient_accept_line_widget() {
        let out = init_zsh();
        assert!(
            out.contains("zle -N accept-line _chevron_accept_line"),
            "expected zle widget registration"
        );
        assert!(
            out.contains("zle .reset-prompt"),
            "transient widget must reset-prompt before accept"
        );
    }

    #[test]
    fn zsh_transient_respects_opt_out_env_var() {
        let out = init_zsh();
        assert!(
            out.contains("CHEVRON_TRANSIENT"),
            "expected opt-out env var check"
        );
    }

    #[test]
    fn zsh_transient_prompt_reflects_exit_status() {
        let out = init_zsh();
        assert!(
            out.contains("%F{green}"),
            "success path should use green chevron"
        );
        assert!(
            out.contains("%F{red}"),
            "failure path should use red chevron"
        );
    }

    #[test]
    fn zsh_emits_duration_tag_for_slow_commands() {
        let out = init_zsh();
        assert!(
            out.contains("CHEVRON_TRANSIENT_DURATION_MS"),
            "expected configurable duration threshold"
        );
        // The dim ANSI escape (CSI 2m) is the marker for the duration line.
        assert!(
            out.contains(r"\033[2m"),
            "duration line should use dim styling"
        );
    }

    #[test]
    fn zsh_duration_tag_respects_minute_threshold() {
        // For >= 60s commands the format switches to `Xm Ys` so the line
        // stays compact (`125.0s` is harder to scan than `2m 5s`).
        let out = init_zsh();
        assert!(out.contains("60000"), "expected the 60s pivot");
        assert!(out.contains("%dm %ds"), "expected minutes/seconds format");
        assert!(out.contains("%d.%ds"), "expected sub-minute decimal format");
    }

    #[test]
    fn zsh_duration_tag_disabled_with_transient() {
        // CHEVRON_TRANSIENT=0 turns off both transient AND duration. The
        // tag's emission is guarded by the same env check; check the
        // duration block sits inside that guard.
        let out = init_zsh();
        assert!(
            out.contains(r#"[[ "${CHEVRON_TRANSIENT:-1}" != "0" ]] && (( duration_ms >="#),
            "duration tag should be guarded by CHEVRON_TRANSIENT"
        );
    }

    #[test]
    fn zsh_async_fast_path_reads_cache_when_enabled() {
        let out = init_zsh();
        assert!(
            out.contains("${CHEVRON_ASYNC:-0}"),
            "async fast path should be gated by CHEVRON_ASYNC env var (off by default)"
        );
        assert!(
            out.contains("$_chevron_cache_file"),
            "async path should read from the cache file"
        );
        assert!(
            out.contains("_chevron_cached_pwd") && out.contains("$PWD"),
            "should validate cached PWD before using cached output"
        );
    }

    #[test]
    fn zsh_async_passes_cache_env_var_to_chevron() {
        let out = init_zsh();
        assert!(
            out.contains(r#"CHEVRON_CACHE_FILE="$_chevron_cache_file""#),
            "async render must set CHEVRON_CACHE_FILE so chevron writes the cache"
        );
    }

    #[test]
    fn zsh_async_registers_zle_fd_callback() {
        let out = init_zsh();
        assert!(
            out.contains("zle -F"),
            "async machinery uses zle -F to watch the background process's stdout fd"
        );
        assert!(
            out.contains("_chevron_async_callback"),
            "callback function must be defined and registered"
        );
        assert!(
            out.contains("zle reset-prompt"),
            "callback must reset-prompt to make the redraw visible"
        );
    }

    #[test]
    fn zsh_async_uses_process_substitution_for_pipe() {
        // Process substitution is the simplest way to get a single fd
        // attached to the background process's stdout. Avoids mkfifo + fd
        // open ordering issues.
        let out = init_zsh();
        assert!(
            out.contains("exec {_chevron_async_fd}< <("),
            "should use `exec {{fd}}< <(cmd)` process substitution"
        );
    }

    #[test]
    fn zsh_cache_file_in_user_owned_dir() {
        let out = init_zsh();
        assert!(
            out.contains("XDG_RUNTIME_DIR"),
            "cache dir should respect XDG_RUNTIME_DIR"
        );
        assert!(
            out.contains("mkdir -p -m 700"),
            "cache dir should be created with owner-only mode"
        );
    }

    #[test]
    fn zsh_sync_path_unaffected_when_async_off() {
        // When CHEVRON_ASYNC is 0 (the default), the sync render call
        // should NOT carry CHEVRON_CACHE_FILE — no point paying the I/O
        // for a cache that'll never be read.
        let out = init_zsh();
        // The sync arm exists for both async and non-async modes; the
        // CHEVRON_ASYNC check above the sync render must distinguish.
        let sync_idx = out
            .find("chevron prompt 20 $exit_status $duration_ms $job_count)\"")
            .expect("sync render call should exist");
        let preceding = &out[..sync_idx];
        // The CHEVRON_CACHE_FILE-wrapped sync call should come BEFORE the
        // plain one, conditional on CHEVRON_ASYNC.
        assert!(
            preceding.contains(r#"if [[ "${CHEVRON_ASYNC:-0}" != "0" ]]; then"#),
            "sync render should branch on CHEVRON_ASYNC to decide whether to set CHEVRON_CACHE_FILE"
        );
    }

    #[test]
    fn zsh_transient_chains_prior_accept_line_override() {
        // If another prompt tool (zsh-autosuggest etc.) already wrapped
        // accept-line, we must call through to it instead of clobbering.
        let out = init_zsh();
        assert!(
            out.contains("_chevron_orig_accept_line"),
            "should preserve prior accept-line override"
        );
        assert!(
            out.contains("widgets[accept-line]"),
            "should inspect the existing widget binding"
        );
    }

    #[test]
    fn bash_contains_prompt_command() {
        let out = init_bash();
        assert!(
            out.contains("PROMPT_COMMAND=_chevron_precmd"),
            "expected PROMPT_COMMAND"
        );
        assert!(
            out.contains("trap '_chevron_preexec' DEBUG"),
            "expected DEBUG trap"
        );
        assert!(
            out.contains("CHEVRON_SHELL=bash"),
            "expected CHEVRON_SHELL=bash"
        );
        assert!(
            out.contains("chevron prompt"),
            "expected chevron prompt call"
        );
        assert!(out.contains("EPOCHREALTIME"), "expected EPOCHREALTIME");
        assert!(out.contains("rename-window"), "should rename tmux window");
    }

    #[test]
    fn bash_guards_against_precmd_reentry() {
        let out = init_bash();
        assert!(
            out.contains("_chevron_in_precmd"),
            "expected reentry guard in bash init"
        );
    }

    #[test]
    fn bash_emits_duration_tag_for_slow_commands() {
        let out = init_bash();
        assert!(
            out.contains("CHEVRON_TRANSIENT_DURATION_MS"),
            "expected configurable duration threshold"
        );
        assert!(
            out.contains(r"\033[2m"),
            "duration line should use dim styling"
        );
    }

    #[test]
    fn bash_duration_tag_respects_minute_threshold() {
        let out = init_bash();
        assert!(out.contains("60000"), "expected 60s pivot");
        assert!(out.contains("%dm %ds"), "expected minutes/seconds format");
        assert!(out.contains("%d.%ds"), "expected sub-minute decimal format");
    }

    #[test]
    fn bash_duration_tag_disabled_with_transient() {
        let out = init_bash();
        assert!(
            out.contains(r#"[[ "${CHEVRON_TRANSIENT:-1}" != "0" ]] && (( duration_ms >="#),
            "duration tag should be guarded by CHEVRON_TRANSIENT"
        );
    }

    #[test]
    fn bash_deliberately_does_not_ship_transient_prompt() {
        // Bash transient via `bind -x` or DEBUG-trap cursor codes breaks on
        // multi-line input, wrapped lines, and compound commands. We
        // deliberately don't ship it. This test locks in that decision so
        // a future change that adds it must also remove this test (and the
        // comment in init_bash) — forcing the author to confront the
        // edge-case story.
        let out = init_bash();
        assert!(
            !out.contains("bind -x"),
            "bash init should not bind -x — no clean transient possible"
        );
        assert!(
            !out.contains("_chevron_transient_show") && !out.contains("_chevron_accept_line"),
            "no zsh/fish-style transient flag in bash init"
        );
    }

    #[test]
    fn fish_contains_fish_prompt() {
        let out = init_fish();
        assert!(
            out.contains("function fish_prompt"),
            "expected fish_prompt function"
        );
        assert!(
            out.contains("CHEVRON_SHELL=fish"),
            "expected CHEVRON_SHELL=fish"
        );
        assert!(
            out.contains("CMD_DURATION"),
            "expected CMD_DURATION for timing"
        );
        assert!(
            out.contains("chevron prompt"),
            "expected chevron prompt call"
        );
        assert!(out.contains("rename-window"), "should rename tmux window");
    }

    #[test]
    fn fish_binds_enter_for_transient_prompt() {
        let out = init_fish();
        assert!(
            out.contains("bind \\r _chevron_transient_enter")
                && out.contains("bind \\n _chevron_transient_enter"),
            "expected Enter and Ctrl-J bound to the transient wrapper"
        );
        assert!(
            out.contains("commandline -f repaint") && out.contains("commandline -f execute"),
            "transient wrapper should repaint then execute"
        );
    }

    #[test]
    fn fish_transient_branch_uses_short_chevron() {
        let out = init_fish();
        assert!(
            out.contains("set -q _chevron_transient_show"),
            "fish_prompt should branch on the transient flag"
        );
        assert!(
            out.contains("set -e _chevron_transient_show"),
            "transient branch must unset the flag so the next full prompt isn't transient too"
        );
        // Green/red branch on exit status.
        assert!(out.contains("set_color green"));
        assert!(out.contains("set_color red"));
    }

    #[test]
    fn fish_transient_respects_opt_out_env_var() {
        let out = init_fish();
        assert!(
            out.contains(r#"if test "$CHEVRON_TRANSIENT" != "0""#),
            "Enter binding should be skipped when CHEVRON_TRANSIENT=0"
        );
    }

    #[test]
    fn fish_postexec_emits_duration_tag() {
        let out = init_fish();
        assert!(
            out.contains("--on-event fish_postexec"),
            "should use fish's postexec event"
        );
        assert!(
            out.contains("CHEVRON_TRANSIENT_DURATION_MS"),
            "duration threshold configurable via env var"
        );
        assert!(out.contains(r"\033[2m"), "duration line uses dim styling");
        assert!(
            out.contains("CMD_DURATION"),
            "should read fish's $CMD_DURATION"
        );
    }

    #[test]
    fn fish_duration_format_pivots_at_minute() {
        let out = init_fish();
        assert!(out.contains("60000"), "60s pivot present");
        assert!(out.contains("%dm %ds"), "minute format present");
        assert!(out.contains("%d.%ds"), "sub-minute decimal format present");
    }
}
