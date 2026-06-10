#[must_use]
#[allow(clippy::too_many_lines)]
pub fn init_zsh() -> &'static str {
    r#"_chevron_preexec() {
    _chevron_cmd_title="$1"
    # OSC 133 C: command output is about to start. Modern terminals
    # (Ghostty, WezTerm, iTerm2, Kitty, VS Code, Windows Terminal) use
    # this to mark the boundary between prompt and command output for
    # navigation, grouping, and "copy output only". Older terminals
    # silently ignore unknown OSC sequences.
    [[ "${CHEVRON_OSC133:-1}" != "0" ]] && printf '\e]133;C\a'
    # Save the cursor's absolute row (via DSR) for the precmd transient-
    # colour rewrite, plus the geometry it was measured under — a resize
    # while the command runs invalidates every saved coordinate. The
    # query helper owns all tty state and typeahead safety — see
    # _chevron_query_row.
    if [[ "${CHEVRON_TRANSIENT:-1}" != "0" ]]; then
        _chevron_query_row
        _chevron_transient_save_row="$REPLY"
        _chevron_transient_save_geom="$COLUMNS:$LINES"
    fi
    # Timestamp LAST: the DSR exchange and tty bookkeeping above can
    # cost tens of milliseconds and must not count against the user's
    # command (precmd takes its end timestamp first, symmetrically).
    _chevron_cmd_start=$EPOCHREALTIME
}
# Query the current cursor row via DSR (\e[6n). Stores the row in REPLY
# (empty on failure or skip). One `read -d 'R'` call instead of a
# per-byte loop — fewer dispatch overheads, and the 300 ms timeout
# covers tmux/SSH round-trip latency that a tight 50 ms-per-byte budget
# can't.
#
# Typeahead safety: `read -d 'R'` consumes every queued byte ahead of
# the response. If the user has already typed the next command, the
# read would swallow it — the line never executes and reappears parked
# in the next edit buffer — and a typed literal `R` truncates the
# exchange early, leaving the real response queued to poison the NEXT
# cycle's query with a stale row (the rewrite then lands on the wrong
# line: duplicated chevrons). So probe for pending input first and skip
# the query entirely when anything is waiting: an honest neutral
# chevron beats racing the user's keystrokes. zselect ships with zsh;
# if the module is somehow absent the probe fails closed into the old
# always-query behaviour.
#
# Tty state: -echo must flip BEFORE the query goes out — response bytes
# arriving ahead of the read are otherwise kernel-echoed at the cursor
# as `^[[12;34R` (this bit precmd, whose query used to run unguarded).
# -icanon because line-mode buffers the response forever (no newline
# ever comes) and echoctl can echo control bytes regardless of -echo.
# min/time 0/0 makes read see bytes as they arrive. The full stty state
# is saved and restored to avoid stomping anything else the user set.
#
# The DSR response shape is `\e[<row>;<col>R`. The response is parsed
# from the LAST CSI in the buffer: terminals with focus reporting (or
# other unsolicited reports) enabled can interleave `\e[I` etc. ahead
# of the response, and parsing the first CSI would feed garbage like
# `I\e[5` into arithmetic — a visible zsh math error. The row is then
# validated as numeric. Anything before the last CSI is input that
# slipped in between probe and read: plain keystrokes (plus tab/CR/LF)
# are re-injected via `print -z` so they land in the next edit buffer
# instead of vanishing; anything carrying other control bytes — ESC
# from arrow keys or stray reports, ^C from an interrupt racing the
# exchange — is dropped rather than pushed into the buffer as garbage.
_chevron_query_row() {
    REPLY=""
    local fd
    exec {fd}< /dev/tty 2>/dev/null || return
    if zselect -t 0 -r $fd 2>/dev/null; then
        exec {fd}<&-
        return
    fi
    local _chevron_stty=$(stty -g 2>/dev/null)
    stty -echo -icanon min 0 time 0 2>/dev/null
    printf '\e[6n' > /dev/tty 2>/dev/null
    local resp
    if IFS= read -u $fd -d 'R' -s -t 0.3 resp 2>/dev/null; then
        _chevron_dsr_inflight=""
        # Double-reporting emulator stacks answer twice; absorb the
        # trailing duplicate now, while the tty is still raw.
        _chevron_drain_reports $fd
    else
        # The response missed the budget and will land later — possibly
        # while ZLE owns the terminal, where split arrival across
        # KEYTIMEOUT lets its tail self-insert into the command line as
        # literal text (`1R`). Flag it; precmd sweeps it out before the
        # editor runs.
        _chevron_dsr_inflight=1
    fi
    [[ -n "$_chevron_stty" ]] && stty "$_chevron_stty" 2>/dev/null
    exec {fd}<&-
    if [[ "$resp" == *$'\e['* ]]; then
        local pre_esc="${resp%$'\e['*}"
        local payload="${resp##*$'\e['}"
        REPLY="${payload%%;*}"
        [[ "$REPLY" == <-> ]] || REPLY=""
        [[ -n "$pre_esc" && "$pre_esc" != *[^[:print:]$'\t\r\n']* ]] && print -z -- "$pre_esc" 2>/dev/null
    fi
}
# Absorb unsolicited ESC-led reports queued on the tty: duplicate DSR
# responses from double-reporting emulator stacks (called with the
# default 20 ms first window right after a successful read) and late
# responses that missed an earlier read's budget (called from precmd's
# sweep with a longer window). Only ESC-led data is consumed; the first
# plain byte is user typeahead — pushed onto the buffer stack, which
# pops into the next edit buffer BEFORE queued input is read, so typing
# order is preserved — and draining stops there.
_chevron_drain_reports() {
    local fd=$1 _chevron_wait=${2:-2} _chevron_first _chevron_junk
    while zselect -t $_chevron_wait -r $fd 2>/dev/null; do
        _chevron_wait=2
        IFS= read -u $fd -k 1 -s -t 0 _chevron_first 2>/dev/null || break
        if [[ "$_chevron_first" == $'\e' ]]; then
            IFS= read -u $fd -d 'R' -s -t 0.1 _chevron_junk 2>/dev/null
        else
            print -z -- "$_chevron_first" 2>/dev/null
            break
        fi
    done
}
_chevron_precmd() {
    local exit_status=$?
    # End timestamp FIRST, before the sweep/rewrite machinery below
    # spends its own time — the duration tag measures the command only.
    local _chevron_cmd_end=$EPOCHREALTIME
    # OSC 133 D: the just-completed command finished with $exit_status.
    # Emitted first so it closes out the previous command region before
    # we print the duration tag and next prompt's A marker.
    [[ "${CHEVRON_OSC133:-1}" != "0" ]] && printf '\e]133;D;%d\a' $exit_status
    # Color-correct the transient prompt (chevron-4xq). preexec captured
    # the absolute row of the line below the painted transient (via
    # DSR). Now we query R_current, erase the transient's rows, write
    # the exit-coloured chevron + cmd at the transient's top row, then
    # absolute-position back to R_current so zsh's main loop draws the
    # next PROMPT where it belongs.
    #
    # The painted transient is `❯ ` plus the typed command, which WRAPS
    # on narrow terminals: it occupies ceil(width/COLUMNS) rows ENDING
    # at R_saved-1. Erasing only the last of those rows left the earlier
    # rows behind — the duplicated-chevron glitch — while the rewrite
    # re-wrapped into rows the next prompt then half-overwrote.
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
    #   - True multi-line input (PS2 continuations): rewrite skipped,
    #     transient stays neutral (see the guard below).
    #   - Terminals that don't support DSR: query returns empty,
    #     rewrite is skipped, the transient stays neutral grey.
    # Sweep a still-in-flight DSR response (preexec's query timed out on
    # a slow link) before ZLE takes the terminal: arriving there, split
    # across KEYTIMEOUT, its tail self-inserts into the command line as
    # literal text (`1R`). By precmd time the straggler has normally
    # arrived; wait up to 300 ms more for it. Terminals that never
    # respond pay this wait once per command — CHEVRON_TRANSIENT=0 is
    # the escape hatch for those.
    if [[ -n "$_chevron_dsr_inflight" && "${CHEVRON_TRANSIENT:-1}" != "0" ]]; then
        local _chevron_sweep_fd
        if exec {_chevron_sweep_fd}< /dev/tty 2>/dev/null; then
            local _chevron_sweep_stty=$(stty -g 2>/dev/null)
            stty -echo -icanon min 0 time 0 2>/dev/null
            _chevron_drain_reports $_chevron_sweep_fd 30
            [[ -n "$_chevron_sweep_stty" ]] && stty "$_chevron_sweep_stty" 2>/dev/null
            exec {_chevron_sweep_fd}<&-
        fi
        _chevron_dsr_inflight=""
    fi
    # A resize between preexec and precmd invalidates the saved row AND
    # the wrap-span math: reflowing terminals (tmux, kitty, ghostty)
    # rewrap the transient to different rows entirely, and recomputing
    # the span under the new COLUMNS would erase rows the transient
    # never occupied. Skipping is the only honest move.
    #
    # Multi-line input (PS2 continuations) is skipped for the same
    # reason: the painted transient spans the buffer's rows plus PS2
    # prefixes whose widths we can't reconstruct, and measuring only
    # the first line made the rewrite erase the continuation row and
    # paint a SECOND copy of line one there — a duplicated chevron.
    if [[ -n "$_chevron_transient_save_row" && "${CHEVRON_TRANSIENT:-1}" != "0" \
          && "$_chevron_transient_save_geom" == "$COLUMNS:$LINES" \
          && "$_chevron_cmd_title" != *$'\n'* ]]; then
        _chevron_query_row
        if [[ -n "$REPLY" ]]; then
            local _chevron_R_current="$REPLY"
            local _chevron_R_saved="$_chevron_transient_save_row"
            local _chevron_delta=$(( _chevron_R_current - _chevron_R_saved ))
            local _chevron_lines=${LINES:-24}
            local _chevron_available=$(( _chevron_lines - _chevron_R_saved ))
            # Cmd may be multi-line; only its first line is measured and
            # written so we don't run past the rows we erased.
            local _chevron_rewrite_cmd="${_chevron_cmd_title%%$'\n'*}"
            # Rows the wrapped transient occupies. ${(m)#...} counts
            # display cells (not bytes); `❯ ` adds 2.
            local _chevron_width=$(( ${(m)#_chevron_rewrite_cmd} + 2 ))
            local _chevron_cols=${COLUMNS:-80}
            local _chevron_span=$(( (_chevron_width + _chevron_cols - 1) / _chevron_cols ))
            (( _chevron_span < 1 )) && _chevron_span=1
            local _chevron_R_top=$(( _chevron_R_saved - _chevron_span ))
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
            if (( _chevron_R_top > 0 \
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
                local _chevron_osc_a=""
                local _chevron_osc_b=""
                if [[ "${CHEVRON_OSC133:-1}" != "0" ]]; then
                    _chevron_osc_a=$'\e]133;A\a'
                    _chevron_osc_b=$'\e]133;B\a'
                fi
                # Erase every wrapped row top-to-bottom, then write the
                # rewrite at the top; it re-wraps into exactly the rows
                # just cleared.
                local _chevron_erase=""
                repeat $_chevron_span _chevron_erase+=$'\e[2K\e[B'
                printf '\e[%d;1H%s\e[%d;1H%s\e[3%dm❯\e[0m %s%s\e[%d;1H' \
                    "$_chevron_R_top" "$_chevron_erase" "$_chevron_R_top" \
                    "$_chevron_osc_a" "$_chevron_color_code" \
                    "$_chevron_osc_b" "$_chevron_rewrite_cmd" \
                    "$_chevron_R_current" > /dev/tty 2>/dev/null
            fi
        fi
    fi
    unset _chevron_transient_save_row _chevron_transient_save_geom
    local duration_ms=0
    if [[ -n "$_chevron_cmd_start" && -n "$_chevron_cmd_end" ]]; then
        duration_ms=$(( (_chevron_cmd_end - _chevron_cmd_start) * 1000 ))
        duration_ms=${duration_ms%.*}
    fi
    unset _chevron_cmd_start
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
    # Generation stamp for the async fast path: bumped every cycle so a
    # background refresh that finishes after a NEWER prompt is already
    # up discards itself instead of repainting stale content (previous
    # directory, previous exit status) over the live prompt.
    (( _chevron_async_gen++ ))
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
    # Stamp the refresh with the cycle that spawned it (see the
    # generation bump in precmd).
    _chevron_async_spawn_gen="$_chevron_async_gen"
    exec {_chevron_async_fd}< <(CHEVRON_CACHE_FILE="$_chevron_cache_file" chevron prompt 20 "$exit_status" "$duration_ms" "$job_count" 2>/dev/null)
    zle -F "$_chevron_async_fd" _chevron_async_callback
}
_chevron_async_callback() {
    local fd=$1
    local fresh
    fresh=$(cat <&$fd 2>/dev/null)
    zle -F "$fd"
    exec {fd}<&-
    # Stale guard: if another precmd ran since this refresh was spawned,
    # the result describes a previous cycle — drop it; the newer cycle
    # owns the prompt now. (The fd is already closed above either way.)
    [[ "$_chevron_async_spawn_gen" == "$_chevron_async_gen" ]] || return
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
# zselect backs the pending-input probe in _chevron_query_row. Best
# effort: if the module is missing, the probe is skipped and the query
# behaves as before.
zmodload -F zsh/zselect b:zselect 2>/dev/null
# EPOCHREALTIME lives in zsh/datetime and expands EMPTY when the module
# isn't loaded — in a bare .zshrc the duration tag silently never fired
# (every command measured as 0 ms). Parameter-only load: no strftime
# builtin or other namespace pollution. Best effort: on failure the
# duration stays 0, as before.
zmodload -F zsh/datetime p:EPOCHREALTIME 2>/dev/null
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
end

# Post-execution duration tag: fish exposes $CMD_DURATION directly, so we
# can hook fish_postexec without the timestamp arithmetic zsh needs.
function _chevron_postexec --on-event fish_postexec
    # Capture $status FIRST — every later command resets it.
    set -l exit_status $status
    # OSC 133 D: previous command finished.
    test "$CHEVRON_OSC133" != "0"; and printf '\e]133;D;%d\a' $exit_status
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

    /// Body of a `name() { ... }` zsh function in the init script, up to
    /// the first un-indented closing brace.
    fn body_of<'a>(script: &'a str, header: &str) -> &'a str {
        let start = script.find(header).expect("function exists");
        let end = script[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .expect("function closes");
        &script[start..end]
    }

    #[test]
    fn zsh_query_row_skips_when_typeahead_pending() {
        // `read -d 'R'` consumes everything queued ahead of the DSR
        // response: typed-ahead commands were swallowed (parked in the
        // next edit buffer, never executed) and a typed `R` left a stale
        // response behind to mis-row the next cycle's rewrite. The probe
        // must come before the query so we never race user input.
        let out = init_zsh();
        let body = body_of(out, "_chevron_query_row() {");
        let probe = body.find("zselect -t 0 -r").expect("pending-input probe");
        let query = body.find(r"printf '\e[6n'").expect("DSR query");
        assert!(probe < query, "probe must run before the query is emitted");
        assert!(
            out.contains("zmodload -F zsh/zselect b:zselect"),
            "zselect builtin must be loaded at init"
        );
    }

    #[test]
    fn zsh_query_row_owns_tty_state() {
        // -echo must flip BEFORE the query leaves: response bytes that
        // arrive ahead of the read are kernel-echoed at the cursor
        // (`^[[12;34R`) otherwise. precmd's query historically ran
        // without this guard — keeping the stty dance inside the helper
        // covers every caller.
        let out = init_zsh();
        let body = body_of(out, "_chevron_query_row() {");
        let echo_off = body.find("stty -echo -icanon").expect("echo guard");
        let query = body.find(r"printf '\e[6n'").expect("DSR query");
        assert!(echo_off < query, "-echo must be set before the query");
        // `stty -`/`$(stty` rather than bare `stty`: the preexec comment
        // block legitimately mentions Ghostty.
        let pre = body_of(out, "_chevron_preexec() {");
        assert!(
            !pre.contains("stty -") && !pre.contains("$(stty"),
            "preexec must not duplicate the tty dance outside the helper"
        );
    }

    #[test]
    fn zsh_rewrite_skips_after_resize() {
        // A resize between preexec and precmd invalidates the saved row
        // and the wrap-span math (reflowing terminals rewrap the
        // transient; a new COLUMNS makes the erase loop eat rows the
        // transient never occupied).
        let out = init_zsh();
        assert!(
            out.contains(r#"_chevron_transient_save_geom="$COLUMNS:$LINES""#),
            "preexec must stash the geometry the transient was painted under"
        );
        assert!(
            out.contains(r#""$_chevron_transient_save_geom" == "$COLUMNS:$LINES""#),
            "precmd must compare geometry before rewriting"
        );
        assert!(
            out.contains("unset _chevron_transient_save_row _chevron_transient_save_geom"),
            "both saved values must be cleared every cycle"
        );
    }

    #[test]
    fn zsh_query_row_parses_last_csi_and_validates_row() {
        // Focus reports (`\e[I`) and other unsolicited input can land
        // ahead of the DSR response; parsing the FIRST CSI fed garbage
        // into arithmetic. Parse the last CSI, require a numeric row,
        // and never print -z anything containing ESC back into the
        // edit buffer.
        let out = init_zsh();
        let body = body_of(out, "_chevron_query_row() {");
        assert!(
            body.contains(r#"payload="${resp##*$'\e['}""#),
            "payload must come from the last CSI in the buffer"
        );
        assert!(
            body.contains(r#"[[ "$REPLY" == <-> ]] || REPLY="""#),
            "row must be validated as numeric"
        );
        assert!(
            body.contains(r#""$pre_esc" != *[^[:print:]$'\t\r\n']*"#),
            "control bytes (ESC reports, ^C) must never be re-injected into the edit buffer"
        );
    }

    #[test]
    fn zsh_query_row_tracks_inflight_responses() {
        // A read that times out leaves the response in flight; it can
        // land while ZLE owns the terminal, where split arrival across
        // KEYTIMEOUT self-inserts its tail (`1R`) into the command
        // line (caught by CI's slow runners, invisible locally). The
        // timeout branch must flag it for precmd's sweep; a successful
        // read must clear the flag and absorb same-instant duplicates.
        let out = init_zsh();
        let body = body_of(out, "_chevron_query_row() {");
        assert!(
            body.contains("_chevron_dsr_inflight=1"),
            "timeout branch must flag the in-flight response"
        );
        assert!(
            body.contains(r#"_chevron_dsr_inflight="""#),
            "successful read must clear the flag"
        );
        assert!(
            body.contains("_chevron_drain_reports $fd"),
            "successful read must absorb trailing duplicates"
        );
    }

    #[test]
    fn zsh_precmd_sweeps_inflight_response_before_zle() {
        let out = init_zsh();
        let body = body_of(out, "_chevron_precmd() {");
        assert!(
            body.contains(r#"[[ -n "$_chevron_dsr_inflight""#),
            "precmd must check for an in-flight response"
        );
        assert!(
            body.contains("_chevron_drain_reports $_chevron_sweep_fd 30"),
            "sweep must wait up to 300 ms for the straggler"
        );
    }

    #[test]
    fn zsh_drain_consumes_only_escape_led_data() {
        // The drain must never eat user typeahead: only ESC-led report
        // data is consumed; the first plain byte is pushed back onto
        // the buffer stack (order-preserving) and draining stops.
        let out = init_zsh();
        let body = body_of(out, "_chevron_drain_reports() {");
        assert!(body.contains(r"== $'\e'"), "must discriminate on ESC");
        assert!(
            body.contains(r#"print -z -- "$_chevron_first""#),
            "plain bytes must be pushed back, not eaten"
        );
    }

    #[test]
    fn zsh_rewrite_skips_multiline_input() {
        // The painted transient for PS2-continued input spans the
        // buffer's rows plus PS2 prefixes whose widths we can't
        // reconstruct; measuring only line one made the rewrite erase
        // the continuation row and paint a second copy of line one
        // there — a duplicated chevron.
        let out = init_zsh();
        assert!(
            out.contains(r#""$_chevron_cmd_title" != *$'\n'*"#),
            "precmd must skip the rewrite for multi-line input"
        );
    }

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
            out.contains("_chevron_R_top"),
            "should compute the top row of the wrapped transient span"
        );
        assert!(
            out.contains(r"$'\e[2K\e[B'"),
            "should erase row-by-row down the span"
        );
        assert!(
            out.contains(r"\e[3%dm❯\e[0m"),
            "should emit colour code + chevron + reset"
        );
        // The format string MUST contain at least two `\e[%d;1H` — one
        // to land on the transient span, one to land back on the
        // next-prompt row after the rewrite.
        let format_str_count = out.matches(r"\e[%d;1H").count();
        assert!(
            format_str_count >= 2,
            "should absolute-position twice (transient then next-prompt), got {format_str_count}"
        );
    }

    #[test]
    fn zsh_rewrite_erases_full_wrapped_span() {
        // A command wider than the terminal wraps the collapsed
        // transient across several rows ending at R_saved-1. Erasing
        // only that last row left the earlier rows behind — duplicated
        // chevrons — while the rewrite re-wrapped into rows the next
        // prompt half-overwrote.
        let out = init_zsh();
        assert!(
            out.contains(r"${(m)#_chevron_rewrite_cmd}"),
            "span width must be measured in display cells, not bytes"
        );
        assert!(
            out.contains("repeat $_chevron_span"),
            "must erase one row per wrapped row"
        );
        assert!(
            out.contains("_chevron_R_saved - _chevron_span"),
            "rewrite must anchor at the top of the span"
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
    fn zsh_loads_datetime_for_duration_tag() {
        // EPOCHREALTIME expands empty unless zsh/datetime is loaded;
        // without this line the duration tag was inert in any bare
        // .zshrc (caught end-to-end by the PTY harness). The -F p:
        // form loads only the parameter — no strftime builtin.
        let out = init_zsh();
        assert!(
            out.contains("zmodload -F zsh/datetime p:EPOCHREALTIME"),
            "init must load the EPOCHREALTIME parameter"
        );
    }

    #[test]
    fn zsh_duration_measures_command_only() {
        // The DSR exchange, stty forks, and drain in preexec — and the
        // sweep/rewrite in precmd — cost tens of milliseconds. With the
        // start timestamp taken first and the duration computed last,
        // that overhead was billed to the user's command: instant
        // builtins measured ~0.2s under a lowered threshold. Start is
        // stamped at the END of preexec, end at the TOP of precmd.
        let out = init_zsh();
        let pre = body_of(out, "_chevron_preexec() {");
        assert!(
            pre.find("_chevron_query_row").unwrap()
                < pre.find("_chevron_cmd_start=$EPOCHREALTIME").unwrap(),
            "start timestamp must be stamped after the DSR machinery"
        );
        let pc = body_of(out, "_chevron_precmd() {");
        assert!(
            pc.find("_chevron_cmd_end=$EPOCHREALTIME").unwrap()
                < pc.find("_chevron_query_row").unwrap(),
            "end timestamp must be captured before the rewrite machinery"
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
    fn zsh_async_callback_discards_stale_generations() {
        // A refresh spawned in cycle N can finish after cycle N+1 has
        // painted its prompt (typed-ahead `cd`, fast follow-up command):
        // applying it repainted the PREVIOUS directory/status over the
        // live prompt. Each cycle bumps a generation; callbacks from
        // older generations discard their result.
        let out = init_zsh();
        assert!(
            out.contains("(( _chevron_async_gen++ ))"),
            "precmd must bump the generation every cycle"
        );
        assert!(
            out.contains(r#"_chevron_async_spawn_gen="$_chevron_async_gen""#),
            "spawn must stamp the generation it belongs to"
        );
        let cb = body_of(out, "_chevron_async_callback() {");
        assert!(
            cb.contains(r#"[[ "$_chevron_async_spawn_gen" == "$_chevron_async_gen" ]] || return"#),
            "callback must drop results from older generations"
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
}
