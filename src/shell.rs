#[must_use]
#[allow(clippy::too_many_lines)]
pub fn init_zsh() -> &'static str {
    r#"_chevron_preexec() {
    _chevron_cmd_start=$EPOCHREALTIME
    _chevron_cmd_title="$1"
    # OSC 133 C: command output is about to start. Modern terminals
    # (Ghostty, WezTerm, iTerm2, Kitty, VS Code, Windows Terminal) use
    # this to mark the boundary between prompt and command output for
    # navigation, grouping, and "copy output only". Older terminals
    # silently ignore unknown OSC sequences.
    [[ "${CHEVRON_OSC133:-1}" != "0" ]] && printf '\e]133;C\a'
    # DECSC: save cursor position for the precmd transient-colour
    # rewrite. At preexec time the transient has actually been painted
    # (unlike inside the accept-line widget, where ZLE buffers the
    # redraw and our save would land at the pre-collapse position) and
    # the cursor sits one line below the transient. precmd does \e8
    # (DECRC) + \e[A to step up onto the transient line.
    printf '\e7' > /dev/tty 2>/dev/null
    _chevron_transient_pending=1
}
_chevron_precmd() {
    local exit_status=$?
    # OSC 133 D: the just-completed command finished with $exit_status.
    # Emitted first so it closes out the previous command region before
    # we print the duration tag and next prompt's A marker.
    [[ "${CHEVRON_OSC133:-1}" != "0" ]] && printf '\e]133;D;%d\a' $exit_status
    # Color-correct the transient prompt (chevron-4xq). preexec saved
    # the cursor at the line BELOW the painted transient; this rewrites
    # the transient line itself with the now-known exit-status colour.
    #   \e[s    SCOSC: stash current cursor (next-prompt row, where
    #           zsh will draw PROMPT after precmd returns).
    #   \e8     DECRC: jump to preexec's saved position (line below the
    #           transient).
    #   \e[A\r  step UP one row onto the transient line, return to col 0.
    #   \e[2K   erase that line.
    #   \e[3<n>m❯ \e[0m <cmd>   redraw with the exit-status colour.
    #   \e[u    SCORC: jump back to the next-prompt row.
    # Known limitations:
    #   - Commands that themselves do DECSC/DECRC (vim, less, man, any
    #     alt-screen app) overwrite our save; the rewrite skips silently
    #     and the user sees the neutral chevron — incorrect colour but
    #     not actively misleading.
    #   - Commands whose output scrolls the screen past the transient
    #     line: the saved row is no longer the transient row; rewrite
    #     lands on whatever line is at that absolute row now. Glitch.
    #   - Multi-line input or wrapped input: \e[A only steps up one row,
    #     so only the last visible row of the transient gets rewritten.
    if [[ -n "$_chevron_transient_pending" && "${CHEVRON_TRANSIENT:-1}" != "0" ]]; then
        local _chevron_color_code
        if (( exit_status == 0 )); then
            _chevron_color_code=2
        else
            _chevron_color_code=1
        fi
        # Cmd may be multi-line; rewrite only the first line so we don't
        # write past the column we landed on.
        local _chevron_rewrite_cmd="${_chevron_cmd_title%%$'\n'*}"
        local _chevron_osc_a=""
        local _chevron_osc_b=""
        if [[ "${CHEVRON_OSC133:-1}" != "0" ]]; then
            _chevron_osc_a=$'\e]133;A\a'
            _chevron_osc_b=$'\e]133;B\a'
        fi
        printf '\e[s\e8\e[A\r\e[2K%s\e[3%dm❯\e[0m %s%s\e[u' \
            "$_chevron_osc_a" "$_chevron_color_code" \
            "$_chevron_osc_b" "$_chevron_rewrite_cmd" > /dev/tty 2>/dev/null
        unset _chevron_transient_pending
    fi
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
    _chevron_make_prompt "${chevron_output%%$'\n'*}"
    PROMPT="$REPLY"
    # Transient stub for accept-line. Rendered neutrally (no green/red)
    # because at accept-line time we can't know the about-to-run
    # command's exit status. precmd does the color correction in place
    # via the cursor-rewrite block above, once the exit is known.
    _chevron_transient_prompt='❯ '
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
        _chevron_make_prompt "${fresh%%$'\n'*}"
        PROMPT="$REPLY"
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
    # Strip the trailing space from the transient prompt since
    # _chevron_make_prompt re-adds one when wrapping; otherwise we'd get
    # a double space between the chevron and the user's input cursor.
    _chevron_make_prompt "${_chevron_transient_prompt% }"
    PROMPT="$REPLY"
    zle .reset-prompt
    # NOTE: cursor save for the precmd color-correction is done in
    # _chevron_preexec, not here. Saving inside the widget runs BEFORE
    # ZLE actually paints the transient (the redraw is buffered until
    # the widget unwinds), so a save here would land at the pre-collapse
    # cursor position and DECRC would later restore to the wrong line.
    # Chain through any prior accept-line override (zsh-autosuggest, etc.)
    # if one exists; otherwise fall back to the built-in.
    if [[ -n "$_chevron_orig_accept_line" ]]; then
        zle "$_chevron_orig_accept_line"
    else
        zle .accept-line
    fi
}
# Build a PROMPT value, optionally wrapping in OSC 133 A/B markers so
# modern terminals can identify the prompt-vs-command boundary. Returns
# the value via REPLY (zsh convention) to avoid a subshell fork. The
# `%{…%}` brackets tell zsh's prompt-width tracker that the escape bytes
# are zero-width; without them, the cursor column drifts after each
# prompt and line editing breaks.
_chevron_make_prompt() {
    if [[ "${CHEVRON_OSC133:-1}" != "0" ]]; then
        REPLY=$'%{\e]133;A\a%}'"$1"$' %{\e]133;B\a%}'
    else
        REPLY="$1 "
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
    # OSC 133 C: command output is about to start.
    [[ "${CHEVRON_OSC133:-1}" != "0" ]] && printf '\e]133;C\a'
}
_chevron_precmd() {
    local exit_status=$?
    _chevron_in_precmd=1
    # OSC 133 D: previous command finished with $exit_status.
    [[ "${CHEVRON_OSC133:-1}" != "0" ]] && printf '\e]133;D;%d\a' $exit_status
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
    # Wrap PS1 in OSC 133 A/B markers when enabled. The `\[…\]` brackets
    # tell bash's prompt-width tracker that the escape bytes are
    # zero-width; without them, line editing miscounts columns.
    local _chevron_body="${chevron_output%%$'\n'*}"
    if [[ "${CHEVRON_OSC133:-1}" != "0" ]]; then
        PS1=$'\\[\e]133;A\a\\]'"$_chevron_body"$' \\[\e]133;B\a\\]'
    else
        PS1="$_chevron_body "
    fi
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
        # Neutral chevron — the colour gets corrected in fish_postexec
        # once the exit status is actually known. See chevron-4xq.
        # Cursor save for the postexec rewrite happens in fish_preexec
        # below, after the transient is actually painted.
        test "$CHEVRON_OSC133" != "0"; and printf '\e]133;A\a'
        echo -n '❯ '
        test "$CHEVRON_OSC133" != "0"; and printf '\e]133;B\a'
        set -g _chevron_transient_pending 1
        return
    end

    set -l exit_status $status
    set -g _chevron_last_exit $exit_status
    set -l duration_ms $CMD_DURATION
    set -l job_count (count (jobs -p 2>/dev/null))
    set -l chevron_output (CHEVRON_SHELL=fish command chevron prompt 20 $exit_status $duration_ms $job_count)
    set -l lines (string split \n -- $chevron_output)
    test "$CHEVRON_OSC133" != "0"; and printf '\e]133;A\a'
    echo -n "$lines[1] "
    test "$CHEVRON_OSC133" != "0"; and printf '\e]133;B\a'
    if set -q TMUX; and test (count $lines) -gt 1
        set -l priority (tmux show-options -w -v @priority_title 2>/dev/null)
        if test -z "$priority"
            tmux set-option -p @custom_title "" \; set-option -p @dir_title "$lines[2]" \; rename-window "$lines[2]"
        end
    end
end

# Pre-execution: emit OSC 133 C so the terminal knows command output is
# about to start, and save cursor for the postexec transient-color
# rewrite (chevron-4xq). At preexec time the transient has been painted
# (fish_prompt + commandline -f repaint completed) and the cursor sits
# one line below the transient.
function _chevron_preexec --on-event fish_preexec
    test "$CHEVRON_OSC133" != "0"; and printf '\e]133;C\a'
    printf '\e7' > /dev/tty 2>/dev/null
end

# Post-execution duration tag: fish exposes $CMD_DURATION directly, so we
# can hook fish_postexec without the timestamp arithmetic zsh needs.
function _chevron_postexec --on-event fish_postexec
    # Capture $status FIRST — every later command resets it.
    set -l exit_status $status
    # OSC 133 D: previous command finished.
    test "$CHEVRON_OSC133" != "0"; and printf '\e]133;D;%d\a' $exit_status
    # Color-correct the transient prompt (chevron-4xq). The transient
    # branch of fish_prompt drew a neutral chevron and saved cursor with
    # \e7; now that we know $exit_status, jump back and rewrite. See
    # the zsh implementation for full rationale + edge-case notes.
    if set -q _chevron_transient_pending; and test "$CHEVRON_TRANSIENT" != "0"
        set -e _chevron_transient_pending
        set -l color_code 2
        test $exit_status -ne 0; and set color_code 1
        set -l rewrite_cmd (string split -m 1 \n -- $argv[1])[1]
        set -l osc_a ""
        set -l osc_b ""
        if test "$CHEVRON_OSC133" != "0"
            set osc_a \e\]133\;A\a
            set osc_b \e\]133\;B\a
        end
        printf '\e[s\e8\e[A\r\e[2K%s\e[3%dm❯\e[0m %s%s\e[u' \
            "$osc_a" $color_code "$osc_b" "$rewrite_cmd" > /dev/tty 2>/dev/null
    end
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
    fn zsh_transient_initial_render_is_neutral() {
        // The accept-line widget draws a neutral chevron because exit
        // status isn't known yet at collapse time. precmd rewrites with
        // colour once the command finishes (chevron-4xq).
        let out = init_zsh();
        assert!(
            out.contains("_chevron_transient_prompt='❯ '"),
            "transient prompt should be a plain chevron, no %F{{...}} colour"
        );
        assert!(
            !out.contains("_chevron_transient_prompt='%F{green}"),
            "no exit-based colour stashed (that was the off-by-one bug)"
        );
    }

    #[test]
    fn zsh_preexec_saves_cursor_for_precmd_rewrite() {
        // The save MUST be in preexec, not the accept-line widget.
        // Saving inside the widget runs before ZLE actually paints
        // the transient (the redraw is buffered until the widget
        // unwinds), so the save lands at the pre-collapse position
        // and DECRC later restores to the wrong line.
        let out = init_zsh();
        assert!(
            out.contains(r"printf '\e7' > /dev/tty 2>/dev/null"),
            "preexec must DECSC-save cursor so precmd can rewrite later"
        );
        assert!(
            out.contains("_chevron_transient_pending=1"),
            "should set pending flag so precmd knows to rewrite"
        );
        // And it should NOT be inside _chevron_accept_line.
        let widget_start = out.find("_chevron_accept_line() {").unwrap();
        let widget_end = out[widget_start..]
            .find("\n}\n")
            .map(|i| widget_start + i)
            .unwrap();
        let widget_body = &out[widget_start..widget_end];
        assert!(
            !widget_body.contains("printf '\\e7'"),
            "save must NOT be in the accept-line widget — ZLE buffers the redraw"
        );
    }

    #[test]
    fn zsh_precmd_color_corrects_via_cursor_rewrite() {
        let out = init_zsh();
        // SCOSC + DECRC + up-one + CR + erase + colored chevron + SCORC.
        // The \e[A step matters: preexec saved at the line BELOW the
        // transient, so we need to move up onto the transient line
        // before erasing.
        assert!(
            out.contains(r"\e[s\e8\e[A\r\e[2K"),
            "precmd should SCOSC, DECRC, cursor-up, CR, erase line"
        );
        assert!(
            out.contains(r"\e[3%dm❯\e[0m"),
            "should emit colour code + chevron + reset"
        );
        assert!(
            out.contains(r"\e[u"),
            "should SCORC restore to pre-rewrite cursor"
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
    fn zsh_emits_osc133_markers_by_default() {
        let out = init_zsh();
        // C in preexec, D in precmd, A+B wrapping PROMPT.
        assert!(
            out.contains(r"printf '\e]133;C\a'"),
            "preexec should emit OSC 133 C"
        );
        assert!(
            out.contains(r"printf '\e]133;D;%d\a'"),
            "precmd should emit OSC 133 D with exit status"
        );
        assert!(
            out.contains(r"\e]133;A\a") && out.contains(r"\e]133;B\a"),
            "PROMPT helper should reference A and B markers"
        );
    }

    #[test]
    fn zsh_osc133_markers_are_width_bracketed() {
        // zsh's `%{...%}` tells the prompt-width tracker the bytes are
        // zero-width. Without it, cursor column drifts on every prompt.
        let out = init_zsh();
        assert!(
            out.contains(r"%{\e]133;A\a%}") && out.contains(r"%{\e]133;B\a%}"),
            "OSC 133 escapes inside PROMPT must be inside %{{...%}}"
        );
    }

    #[test]
    fn zsh_osc133_respects_opt_out() {
        let out = init_zsh();
        assert!(
            out.contains(r#"[[ "${CHEVRON_OSC133:-1}" != "0" ]]"#),
            "OSC 133 emission should be gated by CHEVRON_OSC133"
        );
    }

    #[test]
    fn bash_emits_osc133_markers_by_default() {
        let out = init_bash();
        assert!(
            out.contains(r"printf '\e]133;C\a'"),
            "preexec should emit OSC 133 C"
        );
        assert!(
            out.contains(r"printf '\e]133;D;%d\a'"),
            "precmd should emit OSC 133 D with exit status"
        );
        // bash uses \[...\] not zsh's %{...%}. The init script's raw
        // string contains `\\[` (two backslashes) because that's the
        // escape needed to put a literal `\[` into bash's ANSI-C quoted
        // string `$'...'`, which then expands to bash's prompt
        // non-printing marker `\[`.
        assert!(
            out.contains(r"\\[\e]133;A\a\\]") && out.contains(r"\\[\e]133;B\a\\]"),
            "bash PS1 must wrap OSC escapes in \\[...\\] (via $'\\\\[...\\\\]' in the source)"
        );
    }

    #[test]
    fn bash_osc133_respects_opt_out() {
        let out = init_bash();
        assert!(
            out.contains(r#"[[ "${CHEVRON_OSC133:-1}" != "0" ]]"#),
            "OSC 133 emission should be gated by CHEVRON_OSC133"
        );
    }

    #[test]
    fn fish_emits_osc133_markers_by_default() {
        let out = init_fish();
        // C from fish_preexec, D from fish_postexec, A+B in fish_prompt.
        assert!(
            out.contains("--on-event fish_preexec"),
            "fish needs a fish_preexec handler to emit OSC 133 C"
        );
        assert!(
            out.contains(r"printf '\e]133;C\a'"),
            "preexec should printf OSC 133 C"
        );
        assert!(
            out.contains(r"printf '\e]133;D;%d\a'"),
            "postexec should printf OSC 133 D with exit status"
        );
        assert!(
            out.contains(r"printf '\e]133;A\a'") && out.contains(r"printf '\e]133;B\a'"),
            "fish_prompt should print A and B around the visible prompt"
        );
    }

    #[test]
    fn fish_osc133_respects_opt_out() {
        let out = init_fish();
        assert!(
            out.contains(r#"test "$CHEVRON_OSC133" != "0""#),
            "fish should gate OSC 133 emission on CHEVRON_OSC133"
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
    }

    #[test]
    fn fish_postexec_color_corrects_transient_via_cursor_rewrite() {
        // chevron-4xq: fish_postexec needs to SCOSC + DECRC + step-up +
        // erase + colored chevron + SCORC. Cursor save lives in
        // fish_preexec (after the transient is painted), so postexec's
        // DECRC restores to the line BELOW the transient and \e[A
        // walks up onto it.
        let out = init_fish();
        assert!(
            out.contains(r"\e[s\e8\e[A\r\e[2K"),
            "postexec should SCOSC, DECRC, up-one, CR, erase line"
        );
        assert!(out.contains(r"\e[3%dm❯\e[0m"), "colour + chevron + reset");
        assert!(out.contains(r"\e[u"), "should SCORC restore");
        assert!(
            out.contains("set -g _chevron_transient_pending 1"),
            "fish_prompt's transient branch should set the pending flag"
        );
        // Save must be in fish_preexec, not fish_prompt's transient branch.
        let preexec_start = out.find("function _chevron_preexec").unwrap();
        let preexec_end = out[preexec_start..]
            .find("\nend\n")
            .map(|i| preexec_start + i)
            .unwrap();
        assert!(
            out[preexec_start..preexec_end].contains(r"printf '\e7'"),
            "fish_preexec must contain the DECSC save"
        );
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
