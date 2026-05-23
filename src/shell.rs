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
    # Save the cursor's absolute row (via DSR) for the precmd transient-
    # colour rewrite. preexec fires AFTER ZLE handed control back to
    # outside mode (echo restored), so we MUST disable echo around the
    # DSR query — otherwise the kernel echoes the response bytes
    # (`^[[N;NR`) to stdout, where they leak into the upcoming
    # command's output. We save and restore the full stty state to
    # avoid stomping anything else the user set.
    if [[ "${CHEVRON_TRANSIENT:-1}" != "0" ]]; then
        local _chevron_stty=$(stty -g 2>/dev/null)
        # Both -echo (suppress kernel echo) AND -icanon (drop line-mode
        # buffering) — line-mode could otherwise echo control bytes via
        # echoctl regardless of the -echo flag. min/time:0/1 makes read
        # see bytes as they arrive instead of waiting for a line.
        stty -echo -icanon min 0 time 0 2>/dev/null
        _chevron_query_row
        [[ -n "$_chevron_stty" ]] && stty "$_chevron_stty" 2>/dev/null
        _chevron_transient_save_row="$REPLY"
    fi
    # Command lifecycle hook (chevron-1yn): publish to chevrond so the
    # query CLI and live segments can consume the history. Disabled by
    # CHEVRON_HISTORY=0; skipped for empty commands and for commands
    # that begin with a space (HISTCONTROL=ignorespace discipline — the
    # convention for transient secrets). cmd-start prints a ULID we
    # stash in _chevron_cmd_id, then precmd uses that to emit cmd-end.
    if [[ "${CHEVRON_HISTORY:-1}" != "0" && -n "$1" && "$1" != " "* ]]; then
        _chevron_cmd_id=$(chevron event cmd-start "$_chevron_session_id" "$PWD" "$1")
    else
        unset _chevron_cmd_id
    fi
}
# Query the current cursor row via DSR (\e[6n). Stores the row in REPLY
# (empty on failure). One `read -d 'R'` call instead of a per-byte loop
# — fewer dispatch overheads, and the 300 ms timeout covers tmux/SSH
# round-trip latency that a tight 50 ms-per-byte budget can't.
#
# The DSR response shape is `\e[<row>;<col>R`. `read -d 'R'` reads up
# to (but not including) the R terminator, so resp ends up containing
# `\e[<row>;<col>`. Anything in resp BEFORE the ESC is user typeahead;
# we strip it out and re-inject via `print -z` so a fast-typing user
# doesn't lose keystrokes to the DSR exchange.
_chevron_query_row() {
    REPLY=""
    printf '\e[6n' > /dev/tty 2>/dev/null
    local resp
    IFS= read -d 'R' -s -t 0.3 resp < /dev/tty 2>/dev/null
    if [[ "$resp" == *$'\e['* ]]; then
        local pre_esc="${resp%%$'\e['*}"
        local payload="${resp#*$'\e['}"
        REPLY="${payload%%;*}"
        [[ -n "$pre_esc" ]] && print -z -- "$pre_esc" 2>/dev/null
    fi
}
_chevron_precmd() {
    local exit_status=$?
    # OSC 133 D: the just-completed command finished with $exit_status.
    # Emitted first so it closes out the previous command region before
    # we print the duration tag and next prompt's A marker.
    [[ "${CHEVRON_OSC133:-1}" != "0" ]] && printf '\e]133;D;%d\a' $exit_status
    # Color-correct the transient prompt (chevron-4xq). preexec captured
    # the absolute row of the line below the painted transient (via
    # DSR). Now we query R_current and absolute-position to (R_saved-1)
    # to land on the transient line itself, erase it, write the
    # exit-coloured chevron + cmd, then absolute-position back to
    # R_current so zsh's main loop draws the next PROMPT where it
    # belongs.
    #
    # Pure DSR + absolute positioning — no DECSC/DECRC and no SCOSC.
    # The earlier attempts using \e7/\e8 were silently dropped on some
    # terminals (notably under tmux), causing the rewrite to land on
    # the command output row instead of the transient row.
    #
    # Known limitations:
    #   - Output that scrolled the saved row off-screen: the rewrite
    #     lands somewhere off-visible. Bounded by the `delta < 200`
    #     sanity check.
    #   - Multi-line input or wrapped input: we only erase one row;
    #     other rows of the multi-line transient remain.
    #   - Terminals that don't support DSR: query returns empty,
    #     rewrite is skipped, the transient stays neutral grey.
    if [[ -n "$_chevron_transient_save_row" && "${CHEVRON_TRANSIENT:-1}" != "0" ]]; then
        _chevron_query_row
        if [[ -n "$REPLY" ]]; then
            local _chevron_R_current="$REPLY"
            local _chevron_R_saved="$_chevron_transient_save_row"
            local _chevron_R_transient=$(( _chevron_R_saved - 1 ))
            local _chevron_delta=$(( _chevron_R_current - _chevron_R_saved ))
            local _chevron_lines=${LINES:-24}
            local _chevron_available=$(( _chevron_lines - _chevron_R_saved ))
            # Scroll detection: any of these means the saved row is
            # no longer the transient line.
            #   - R_saved at/past bottom: any output scrolled.
            #   - R_current at/past bottom: cursor clamped because the
            #     command exceeded screen height. delta looks safe but
            #     is bogus.
            #   - delta > available: output ran past the bottom edge.
            # Leaving the chevron neutral in those cases is honest;
            # streaming commands (ps, ls of huge dirs, cargo build)
            # commonly trigger one of these and the user gets a clean
            # gray chevron without a glitched rewrite.
            if (( _chevron_R_transient > 0 \
                && _chevron_R_saved < _chevron_lines \
                && _chevron_R_current < _chevron_lines \
                && _chevron_delta >= 0 \
                && _chevron_delta <= _chevron_available )); then
                local _chevron_color_code
                if (( exit_status == 0 )); then
                    _chevron_color_code=2
                else
                    _chevron_color_code=1
                fi
                # Cmd may be multi-line; only write its first line so we
                # don't run past the column we landed on.
                local _chevron_rewrite_cmd="${_chevron_cmd_title%%$'\n'*}"
                local _chevron_osc_a=""
                local _chevron_osc_b=""
                if [[ "${CHEVRON_OSC133:-1}" != "0" ]]; then
                    _chevron_osc_a=$'\e]133;A\a'
                    _chevron_osc_b=$'\e]133;B\a'
                fi
                printf '\e[%d;1H\e[2K%s\e[3%dm❯\e[0m %s%s\e[%d;1H' \
                    "$_chevron_R_transient" \
                    "$_chevron_osc_a" "$_chevron_color_code" \
                    "$_chevron_osc_b" "$_chevron_rewrite_cmd" \
                    "$_chevron_R_current" > /dev/tty 2>/dev/null
            fi
        fi
        unset _chevron_transient_save_row
    fi
    local duration_ms=0
    if [[ -n "$_chevron_cmd_start" ]]; then
        duration_ms=$(( ($EPOCHREALTIME - _chevron_cmd_start) * 1000 ))
        duration_ms=${duration_ms%.*}
        unset _chevron_cmd_start
    fi
    # Command lifecycle finish event (chevron-1yn). Mirror of cmd-start
    # in preexec: only emitted when preexec stashed an id (i.e.
    # CHEVRON_HISTORY enabled and the command wasn't ignorespace'd).
    # Synchronous like cmd-start — the chevron binary's UDS publish
    # path is bounded at ~25 ms, comparable to the prompt-render fork
    # already in this function.
    if [[ -n "$_chevron_cmd_id" ]]; then
        chevron event cmd-end "$_chevron_cmd_id" "$exit_status" "$duration_ms"
        unset _chevron_cmd_id
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
    # Phase 3 (chevron-1yn.3): stash exit_status + duration_ms so the
    # live subscriber callback (which fires between prompts, not from
    # precmd) can call _chevron_start_async with realistic args.
    _chevron_last_exit=$exit_status
    _chevron_last_duration=$duration_ms
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
# Session id for the command-lifecycle hooks (chevron-1yn). Minted once
# at init so every command from this shell shares it; sub-shells get
# their own. Stays unset when CHEVRON_HISTORY=0 so the preexec/precmd
# bookkeeping short-circuits without forking.
if [[ "${CHEVRON_HISTORY:-1}" != "0" && -z "$_chevron_session_id" ]]; then
    _chevron_session_id=$(chevron event new-session 2>/dev/null)
fi
# Live subscriber callback (chevron-1yn.3 Phase 3). Fires from zle
# context when `chevron subscribe`'s background pipe becomes readable
# — i.e., when chevrond saw a state change (FS-watched .git/ event,
# command finished elsewhere, etc.). Reads one EVENT line, debounces
# bursts, and kicks the existing async render path so the prompt
# redraws with fresh state.
_chevron_live_callback() {
    local fd=$1
    local line
    # Read available input. On EOF (chevron subscribe died, daemon
    # restarted, etc.) unregister + close the fd; the user can
    # re-enable Phase 3 by opening a new shell.
    if ! IFS= read -r line <&$fd; then
        zle -F "$fd" 2>/dev/null
        exec {fd}<&- 2>/dev/null
        unset _chevron_live_fd
        return
    fi
    # The subscriber filters PINGs out, so we should only see EVENT
    # lines here — but guard defensively in case of future changes.
    [[ "$line" != EVENT* ]] && return
    # Debounce. A single `git commit` fires several FS events that the
    # daemon coalesces per workdir, but bursts from `git rebase` etc.
    # can still produce a handful within ms of each other. One redraw
    # per 100 ms is plenty for human-perceptible liveness.
    local now_ms=$(( EPOCHREALTIME * 1000 ))
    now_ms=${now_ms%.*}
    local last_ms=${_chevron_live_last_ms:-0}
    if (( now_ms - last_ms < 100 )); then
        return
    fi
    _chevron_live_last_ms=$now_ms
    # Spawn a background render using the existing async pipeline.
    # The completion callback (_chevron_async_callback) sets PROMPT
    # and calls zle reset-prompt, redrawing the prompt in place.
    local _exit=${_chevron_last_exit:-0}
    local _dur=${_chevron_last_duration:-0}
    local _jobs=${(%):-%j}
    _chevron_start_async "$_exit" "$_dur" "$_jobs"
}
# Spawn the subscriber helper and register the zle -F handler when
# CHEVRON_LIVE=1. Off by default until shaken out across terminals.
# The subscriber inherits SIGHUP from this shell on exit, so its
# lifecycle is bounded — no need to track a PID for cleanup.
if [[ "${CHEVRON_LIVE:-0}" != "0" ]]; then
    exec {_chevron_live_fd}< <(chevron subscribe 2>/dev/null)
    zle -F "$_chevron_live_fd" _chevron_live_callback
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
    # Command lifecycle hook (chevron-1yn). $BASH_COMMAND is the literal
    # of the next command bash is about to run via the DEBUG trap. Skip
    # the publish when CHEVRON_HISTORY=0, when $BASH_COMMAND is empty,
    # and when it begins with a space (the HISTCONTROL=ignorespace
    # convention) — matching the zsh/fish opt-out shape. Bash fires the
    # DEBUG trap once per simple command in a pipeline; we only stash
    # an id for the first one and rely on cmd-end in precmd to close it.
    if [[ "${CHEVRON_HISTORY:-1}" != "0" \
        && -n "$BASH_COMMAND" \
        && "$BASH_COMMAND" != " "* \
        && -z "$_chevron_cmd_id" ]]; then
        _chevron_cmd_id=$(chevron event cmd-start "$_chevron_session_id" "$PWD" "$BASH_COMMAND")
    fi
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
    # Command lifecycle finish event (chevron-1yn). Bash counterpart of
    # the zsh precmd cmd-end emission — exit_status and duration_ms are
    # already computed above. _chevron_cmd_id is left unset by preexec
    # when CHEVRON_HISTORY=0 or for ignorespace'd commands, so this
    # silently skips in those cases.
    if [[ -n "$_chevron_cmd_id" ]]; then
        chevron event cmd-end "$_chevron_cmd_id" "$exit_status" "$duration_ms"
        unset _chevron_cmd_id
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
# Session id for the command-lifecycle hooks (chevron-1yn). One per
# shell process; stays unset when CHEVRON_HISTORY=0 so the trap/PROMPT_COMMAND
# bookkeeping short-circuits without forking.
if [[ "${CHEVRON_HISTORY:-1}" != "0" && -z "$_chevron_session_id" ]]; then
    _chevron_session_id=$(chevron event new-session 2>/dev/null)
fi
"#
}

#[must_use]
pub fn init_fish() -> &'static str {
    r#"function fish_prompt
    # Transient branch: short prompt fired when the user hits Enter.
    # _chevron_transient_enter sets the flag and forces a repaint
    # before executing the command. We unset the flag here so the next
    # full prompt (after the command runs) renders normally.
    #
    # By design, the fish transient chevron stays neutral (no
    # green/red exit-status colour). The about-to-run command's exit
    # status is unknowable at collapse time, and fish lacks a
    # timeout-capable `read` builtin to safely query DSR for the
    # post-exec cursor-rewrite that zsh uses. We prefer an honest
    # neutral chevron over a coloured guess that might lie.
    if set -q _chevron_transient_show
        set -e _chevron_transient_show
        test "$CHEVRON_OSC133" != "0"; and printf '\e]133;A\a'
        echo -n '❯ '
        test "$CHEVRON_OSC133" != "0"; and printf '\e]133;B\a'
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
# about to start.
function _chevron_preexec --on-event fish_preexec
    test "$CHEVRON_OSC133" != "0"; and printf '\e]133;C\a'
    # Command lifecycle hook (chevron-1yn). fish_preexec passes the
    # literal command line as $argv[1]. Skip when CHEVRON_HISTORY=0,
    # empty cmd, or leading-space (ignorespace convention).
    set -e _chevron_cmd_id
    if test "$CHEVRON_HISTORY" != "0"
        and test -n "$argv[1]"
        and not string match -q ' *' -- "$argv[1]"
        set -g _chevron_cmd_id (chevron event cmd-start "$_chevron_session_id" "$PWD" "$argv[1]")
    end
end

# Post-execution duration tag: fish exposes $CMD_DURATION directly, so we
# can hook fish_postexec without the timestamp arithmetic zsh needs.
function _chevron_postexec --on-event fish_postexec
    # Capture $status FIRST — every later command resets it.
    set -l exit_status $status
    # OSC 133 D: previous command finished.
    test "$CHEVRON_OSC133" != "0"; and printf '\e]133;D;%d\a' $exit_status
    # Command lifecycle finish event (chevron-1yn). Mirrors fish_preexec:
    # only emit if cmd-start stashed an id (history enabled, non-empty,
    # non-ignorespace command). $CMD_DURATION is in ms already.
    if set -q _chevron_cmd_id
        chevron event cmd-end "$_chevron_cmd_id" "$exit_status" "$CMD_DURATION"
        set -e _chevron_cmd_id
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
# Session id for the command-lifecycle hooks (chevron-1yn). Once per
# fish process; stays unset when CHEVRON_HISTORY=0 so the hooks
# short-circuit without forking.
if test "$CHEVRON_HISTORY" != "0"; and not set -q _chevron_session_id
    set -g _chevron_session_id (chevron event new-session 2>/dev/null)
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
    fn zsh_preexec_captures_cursor_row_via_dsr() {
        // preexec captures the cursor row (one line below the painted
        // transient) so precmd can absolute-position back. Must be in
        // preexec, NOT in the accept-line widget — saving inside the
        // widget runs before ZLE has actually painted the transient.
        let out = init_zsh();
        let preexec_start = out.find("_chevron_preexec() {").unwrap();
        let preexec_end = out[preexec_start..]
            .find("\n}\n")
            .map(|i| preexec_start + i)
            .unwrap();
        let preexec_body = &out[preexec_start..preexec_end];
        assert!(
            preexec_body.contains("_chevron_query_row"),
            "preexec must call the DSR helper to capture cursor row"
        );
        assert!(
            preexec_body.contains("_chevron_transient_save_row"),
            "preexec must store the saved row for precmd to read"
        );
        // No DECSC inside the accept-line widget.
        let widget_start = out.find("_chevron_accept_line() {").unwrap();
        let widget_end = out[widget_start..]
            .find("\n}\n")
            .map(|i| widget_start + i)
            .unwrap();
        assert!(
            !out[widget_start..widget_end].contains(r"printf '\e7'"),
            "save must NOT be in the accept-line widget"
        );
    }

    #[test]
    fn zsh_precmd_color_corrects_via_absolute_positioning() {
        let out = init_zsh();
        // Pure DSR + absolute positioning. No DECSC/DECRC anywhere
        // in the rewrite block — some terminals (notably under tmux)
        // silently drop \e8.
        assert!(
            out.contains("_chevron_R_transient"),
            "should compute the transient row from saved row - 1"
        );
        assert!(
            out.contains(r"\e[%d;1H\e[2K"),
            "should absolute-move to transient row and erase"
        );
        assert!(
            out.contains(r"\e[3%dm❯\e[0m"),
            "should emit colour code + chevron + reset"
        );
        // The format string MUST contain two `\e[%d;1H` — one to land
        // on the transient line, one to land back on the next-prompt
        // row after the rewrite.
        let format_str_count = out.matches(r"\e[%d;1H").count();
        assert!(
            format_str_count >= 2,
            "should absolute-position twice (transient then next-prompt), got {format_str_count}"
        );
    }

    #[test]
    fn zsh_rewrite_does_not_rely_on_decrc_or_scosc() {
        // Both DECRC (\e8) and SCOSC (\e[s) have known portability
        // problems: DECRC silently dropped under tmux on some setups,
        // SCOSC aliased with DECSC's save slot on many terminals.
        // Search the actual printf format strings, not the explanatory
        // comments above the rewrite block.
        let out = init_zsh();
        let rewrite_start = out
            .find("Color-correct the transient prompt")
            .expect("rewrite block exists");
        // Skip the prose-comment lines; locate the printf invocation.
        let printf_start = out[rewrite_start..]
            .find("printf '")
            .map(|i| rewrite_start + i)
            .expect("rewrite printf exists");
        let printf_end = out[printf_start..]
            .find("' \\\n")
            .map_or(out.len(), |i| printf_start + i);
        let printf_fmt = &out[printf_start..printf_end];
        assert!(
            !printf_fmt.contains(r"\e8"),
            "rewrite printf must not use DECRC: {printf_fmt}"
        );
        assert!(
            !printf_fmt.contains(r"\e[s"),
            "rewrite printf must not use SCOSC: {printf_fmt}"
        );
        assert!(
            !printf_fmt.contains(r"\e[u"),
            "rewrite printf must not use SCORC: {printf_fmt}"
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
    fn fish_transient_stays_neutral_by_design() {
        // fish's transient chevron is intentionally never colour-
        // corrected post-hoc. The about-to-run command's exit status
        // is unknowable at collapse time, and fish lacks a timeout-
        // capable `read` builtin for the DSR-based cursor rewrite zsh
        // uses. Rather than risk a hung prompt or a misleading guess,
        // fish stays neutral. (See the comment block in the fish
        // transient branch.)
        let out = init_fish();
        assert!(
            !out.contains("_chevron_transient_pending"),
            "fish no longer tracks a pending flag — there's no rewrite to wait for"
        );
        assert!(
            !out.contains(r"\e[%d;1H\e[2K"),
            "fish must not do any cursor-rewrite — guard against accidental copies from zsh"
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

    // ── chevron-1yn Phase 1: command-lifecycle publish ──────────────────

    #[test]
    fn zsh_publishes_cmd_lifecycle_to_chevrond() {
        let out = init_zsh();
        assert!(
            out.contains("_chevron_cmd_id=$(chevron event cmd-start"),
            "preexec must capture a cmd id from chevron event cmd-start"
        );
        assert!(
            out.contains(r#"chevron event cmd-end "$_chevron_cmd_id""#),
            "precmd must publish cmd-end with the captured id"
        );
        assert!(
            out.contains("_chevron_session_id=$(chevron event new-session"),
            "init must mint a session id once per shell"
        );
    }

    #[test]
    fn zsh_lifecycle_respects_history_opt_out() {
        let out = init_zsh();
        // preexec guard: CHEVRON_HISTORY=0 short-circuits.
        assert!(
            out.contains(r#""${CHEVRON_HISTORY:-1}" != "0""#),
            "expected CHEVRON_HISTORY opt-out gate"
        );
        // Ignorespace pattern: leading-space commands are excluded.
        assert!(
            out.contains(r#""$1" != " "*"#),
            "leading-space (ignorespace) commands should be excluded"
        );
    }

    #[test]
    fn bash_publishes_cmd_lifecycle_to_chevrond() {
        let out = init_bash();
        assert!(
            out.contains("_chevron_cmd_id=$(chevron event cmd-start"),
            "DEBUG-trap preexec must capture a cmd id"
        );
        assert!(
            out.contains(r#"chevron event cmd-end "$_chevron_cmd_id""#),
            "PROMPT_COMMAND must publish cmd-end"
        );
        assert!(
            out.contains("_chevron_session_id=$(chevron event new-session"),
            "init must mint a session id"
        );
    }

    #[test]
    fn bash_lifecycle_uses_bash_command_for_cmd_text() {
        // The DEBUG trap fires before the command runs; bash exposes the
        // literal line as $BASH_COMMAND. Confirm the preexec branch uses
        // that rather than something stale or invented.
        let out = init_bash();
        assert!(
            out.contains(
                r#"chevron event cmd-start "$_chevron_session_id" "$PWD" "$BASH_COMMAND""#
            ),
            "preexec should pass $BASH_COMMAND as the cmd argument"
        );
    }

    #[test]
    fn bash_lifecycle_skips_when_already_capturing() {
        // Bash fires DEBUG per simple command in a pipeline; we should
        // only stash an id on the first one (so cmd-end in precmd has a
        // single id to close).
        let out = init_bash();
        assert!(
            out.contains(r#"-z "$_chevron_cmd_id""#),
            "preexec should guard against re-publishing within a single PROMPT_COMMAND cycle"
        );
    }

    #[test]
    fn fish_publishes_cmd_lifecycle_to_chevrond() {
        let out = init_fish();
        assert!(
            out.contains("set -g _chevron_cmd_id (chevron event cmd-start"),
            "fish_preexec must capture a cmd id"
        );
        assert!(
            out.contains(r#"chevron event cmd-end "$_chevron_cmd_id""#),
            "fish_postexec must publish cmd-end"
        );
        assert!(
            out.contains("set -g _chevron_session_id (chevron event new-session"),
            "init must mint a session id"
        );
    }

    #[test]
    fn fish_lifecycle_respects_history_opt_out_and_ignorespace() {
        let out = init_fish();
        assert!(
            out.contains(r#"test "$CHEVRON_HISTORY" != "0""#),
            "fish lifecycle hook must check CHEVRON_HISTORY"
        );
        assert!(
            out.contains(r#"string match -q ' *' -- "$argv[1]""#),
            "fish lifecycle hook must skip leading-space (ignorespace) commands"
        );
    }

    // ── chevron-1yn.3 Phase 3: live prompt subscriber ───────────────────

    #[test]
    fn zsh_live_subscriber_off_by_default() {
        let out = init_zsh();
        // The spawn block is gated on CHEVRON_LIVE != 0; default value
        // is 0, so out-of-the-box behavior is identical to pre-Phase-3.
        assert!(
            out.contains("${CHEVRON_LIVE:-0}"),
            "live subscriber should be gated on CHEVRON_LIVE env var"
        );
    }

    #[test]
    fn zsh_live_subscriber_spawns_chevron_subscribe() {
        let out = init_zsh();
        assert!(
            out.contains("exec {_chevron_live_fd}< <(chevron subscribe"),
            "live mode should spawn `chevron subscribe` via process substitution"
        );
        assert!(
            out.contains(r#"zle -F "$_chevron_live_fd" _chevron_live_callback"#),
            "live mode should register a zle -F callback on the subscriber's stdout fd"
        );
    }

    #[test]
    fn zsh_live_callback_uses_async_pipeline() {
        // The live callback reuses the existing async render
        // pipeline so we don't duplicate prompt-render logic. It
        // should call _chevron_start_async with stashed exit + duration.
        let out = init_zsh();
        let cb_start = out.find("_chevron_live_callback() {").unwrap();
        let cb_end = out[cb_start..].find("\n}\n").map(|i| cb_start + i).unwrap();
        let body = &out[cb_start..cb_end];
        assert!(
            body.contains("_chevron_start_async"),
            "live callback should kick the async render path"
        );
        assert!(
            body.contains("_chevron_last_exit"),
            "live callback should use the precmd-stashed last exit"
        );
    }

    #[test]
    fn zsh_live_callback_debounces_event_bursts() {
        // A burst of FS events (git rebase, fetch + merge) shouldn't
        // trigger N renders. The callback should compare timestamps
        // and bail if within the debounce window.
        let out = init_zsh();
        let cb_start = out.find("_chevron_live_callback() {").unwrap();
        let cb_end = out[cb_start..].find("\n}\n").map(|i| cb_start + i).unwrap();
        let body = &out[cb_start..cb_end];
        assert!(
            body.contains("_chevron_live_last_ms"),
            "callback should compare against a stashed last-event timestamp"
        );
        assert!(
            body.contains("< 100"),
            "callback should debounce within 100ms"
        );
    }

    #[test]
    fn zsh_live_callback_unregisters_on_eof() {
        // When the subscriber pipe closes (daemon restart, helper
        // died), the callback should unregister itself so we don't
        // burn CPU on a busy-empty fd.
        let out = init_zsh();
        let cb_start = out.find("_chevron_live_callback() {").unwrap();
        let cb_end = out[cb_start..].find("\n}\n").map(|i| cb_start + i).unwrap();
        let body = &out[cb_start..cb_end];
        assert!(
            body.contains("zle -F \"$fd\""),
            "callback should call `zle -F fd` (no handler) on EOF to unregister"
        );
    }

    #[test]
    fn zsh_precmd_stashes_last_exit_and_duration() {
        // The live callback needs realistic args for _chevron_start_async.
        // precmd is the only place that knows the last command's exit
        // status and duration, so it must stash them globally for the
        // callback to read between prompts.
        let out = init_zsh();
        assert!(
            out.contains("_chevron_last_exit=$exit_status"),
            "precmd should stash exit_status into a module-scope var"
        );
        assert!(
            out.contains("_chevron_last_duration=$duration_ms"),
            "precmd should stash duration_ms into a module-scope var"
        );
    }
}
