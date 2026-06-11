use std::fmt::Write;

use crate::config::ShellConfig;

/// Returns the zsh init script using `ShellConfig::default()`. Convenience
/// for callers (tests, the integration suite) that don't care about config.
#[must_use]
pub fn init_zsh() -> String {
    init_zsh_with(&ShellConfig::default())
}

/// Returns the zsh init script with a config-derived preamble that sets
/// defaults for the `CHEVRON_*` env vars. Existing env exports still win
/// (env > config > default) because the preamble uses `${VAR:=value}`,
/// which assigns only when the variable is unset.
#[must_use]
pub fn init_zsh_with(cfg: &ShellConfig) -> String {
    format!("{}{}", shell_preamble_posix(cfg), BODY_ZSH)
}

/// Returns the top-of-`.zshrc` snippet that paints the previously cached
/// prompt before the rest of `.zshrc` finishes loading (chevron-nf8). The
/// user pastes this verbatim at the very top of their `~/.zshrc`. Activates
/// only when chevron's main init has not yet loaded, when stdin/stdout are
/// a tty, and when the cache file looks zsh-shaped.
#[must_use]
pub fn init_zsh_instant_prompt() -> &'static str {
    INSTANT_PROMPT_ZSH
}

/// Sentinel comment line — used by `chevron doctor` to detect whether the
/// user has pasted the instant-prompt snippet into their .zshrc.
pub const INSTANT_PROMPT_MARKER: &str = "chevron-instant-prompt-v1";

const INSTANT_PROMPT_ZSH: &str = r#"# chevron instant prompt — paste this BLOCK at the TOP of ~/.zshrc.
# Marker: chevron-instant-prompt-v1
# Paints the previously-rendered prompt from cache in <50ms while .zshrc
# loads. Any output .zshrc produces is buffered and replayed when the real
# prompt takes over. To disable, comment out this block.
if [[ -o interactive && -t 1 ]] \
    && [[ -z "$_chevron_instant_active" ]] \
    && ! typeset -f _chevron_make_prompt >/dev/null 2>&1; then
    _chevron_cache_file="${XDG_RUNTIME_DIR:-/tmp}/chevron-${UID:-$(id -u)}/last-prompt"
    if [[ -r "$_chevron_cache_file" ]]; then
        # Pure zsh — no fork. `$(<file)` is the builtin equivalent of cat.
        local _chevron_cfull="$(<"$_chevron_cache_file")"
        local _chevron_cprompt="${_chevron_cfull#*$'\n'}"   # drop pwd line
        _chevron_cprompt="${_chevron_cprompt%%$'\n'*}"       # drop tmux title
        # Shell-shape heuristic: only paint if the cache looks like a
        # zsh-bracketed prompt body. Skip bash-style `\[` brackets which
        # would print as literal characters under `print -P`.
        if [[ -n "$_chevron_cprompt" \
            && "$_chevron_cprompt" == *'%{'* \
            && "$_chevron_cprompt" != *'\['* ]]; then
            _chevron_instant_buf="${TMPDIR:-/tmp}/chevron-instant-$$"
            if : > "$_chevron_instant_buf" 2>/dev/null; then
                _chevron_instant_active=1
                # Paint the cached prompt. `-P` interprets %{...%} as
                # zero-width markers so cursor tracking stays correct.
                print -nP -- "$_chevron_cprompt"
                # Save original fds, then capture all stdout/stderr from
                # .zshrc into the buffer. Takeover (or zshexit) restores.
                exec {_chevron_instant_orig_stdout}>&1 \
                     {_chevron_instant_orig_stderr}>&2
                exec >>"$_chevron_instant_buf" 2>&1
                # Defensive: if .zshrc errors before precmd takes over,
                # this hook restores fds and flushes the buffer so the
                # user isn't stuck with a broken terminal.
                _chevron_instant_zshexit() {
                    [[ -z "$_chevron_instant_active" ]] && return
                    # No 2>/dev/null here: exec persists every redirection
                    # on it, so it would re-point stderr at /dev/null.
                    exec >&"$_chevron_instant_orig_stdout" \
                         2>&"$_chevron_instant_orig_stderr"
                    [[ -s "$_chevron_instant_buf" ]] \
                        && cat -- "$_chevron_instant_buf" >&2 2>/dev/null
                    rm -f -- "$_chevron_instant_buf" 2>/dev/null
                }
                zshexit_functions+=(_chevron_instant_zshexit)
            fi
        fi
        unset _chevron_cfull _chevron_cprompt
    fi
fi
"#;

#[allow(clippy::too_many_lines)]
const BODY_ZSH: &str = r#"_chevron_preexec() {
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
    # Gated on the collapse having actually painted: alternate accept
    # widgets (accept-and-hold, accept-line-and-down-history, custom
    # widgets that call .accept-line directly) bypass the override, so
    # the FULL prompt is still on screen — a rewrite sized for `❯ cmd`
    # would erase the wrong span and paint a collapsed line over it.
    if [[ "${CHEVRON_TRANSIENT:-1}" != "0" && -n "$_chevron_transient_collapsed" ]]; then
        _chevron_query_row
        _chevron_transient_save_row="$REPLY"
        _chevron_transient_save_geom="$COLUMNS:$LINES"
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
    # Timestamp LAST: the DSR exchange, tty bookkeeping, and the
    # lifecycle publish above can each cost tens of milliseconds and
    # must not count against the user's command (precmd takes its end
    # timestamp first, symmetrically).
    _chevron_cmd_start=$EPOCHREALTIME
}
# Query the current cursor row via DSR (\e[6n). Stores the row in REPLY
# (empty on failure or skip). One `read -d 'R'` call instead of a
# per-byte loop — fewer dispatch overheads, and the 300 ms timeout
# covers tmux/SSH round-trip latency that a tight 50 ms-per-byte budget
# can't. An optional argument gives a linger window (zselect
# hundredths): on timeout or truncation the helper then drains the
# straggling response INSIDE its own raw window instead of flagging it
# for a later sweep — for callers with no sweep point behind them
# (precmd's rewrite query; the next stop is ZLE).
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
# The probe must run with the tty already RAW: select on a canonical
# tty reports readability only at line boundaries, so a partial line —
# paste leftovers past the newline an interactive `read -rs` consumed,
# half a typed command — is invisible to a cooked-mode probe. The query
# then races input the probe swore wasn't there: `read -d 'R'`
# truncates at the first typed `R` (pasted API keys are full of them)
# and the real response is left stranded to kernel-echo as literal
# `^[[68;1R` once echo returns. Raw first makes partials visible.
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
# A buffer with NO CSI at all means a typed `R` ended the read before
# the response existed: the whole buffer is typeahead. Re-inject it
# (restoring the `R` the delimiter ate) and treat the response as still
# in flight — silently eating those bytes loses user input.
_chevron_query_row() {
    REPLY=""
    local fd
    # The braces are load-bearing: a bare `exec` makes EVERY redirection
    # on it permanent, so a trailing 2>/dev/null — meant only to silence
    # a failed open — would point the whole shell's stderr at /dev/null
    # for the rest of the session. Command error output vanishes, and
    # ncurses tset/reset (which reach the terminal via fd 2) abort
    # silently: "reset stopped working". The brace group scopes the
    # suppression to the open; the fd allocated by exec persists anyway.
    { exec {fd}< /dev/tty } 2>/dev/null || return
    local _chevron_stty=$(stty -g 2>/dev/null)
    local resp
    # try/always: a ^C during the exchange — the zselect, the read, or
    # a linger drain; windows up to 600 ms on a slow terminal — unwinds
    # the whole hook chain mid-function. Without the always-block the
    # stty restore and fd close are skipped: the terminal is left raw
    # with echo off (typing invisible, interactive reads broken), and
    # the NEXT exchange saves the wedged state via `stty -g` and
    # faithfully re-restores it, making the wedge permanent. The
    # always-list runs on interrupt as well as on every normal exit
    # path, including the early return.
    {
        stty -echo -icanon min 0 time 0 2>/dev/null
        if zselect -t 0 -r $fd 2>/dev/null; then
            return
        fi
        printf '\e[6n' > /dev/tty 2>/dev/null
        if IFS= read -u $fd -d 'R' -s -t 0.3 resp 2>/dev/null \
            && [[ "$resp" == *$'\e['* ]]; then
            _chevron_dsr_inflight=""
            # Double-reporting emulator stacks answer twice; absorb the
            # trailing duplicate now, while the tty is still raw.
            _chevron_drain_reports $fd
        else
            # Timed out, or truncated by a typed `R` before the response
            # arrived — either way the answer lands later, possibly while
            # ZLE owns the terminal, where split arrival across KEYTIMEOUT
            # lets its tail self-insert into the command line as literal
            # text (`1R`). Flag it; precmd sweeps it out before the editor
            # runs.
            _chevron_dsr_inflight=1
        fi
        # Linger mode: this caller has no sweep point behind it, so absorb
        # the straggler now, while -echo still holds. Restoring echo first
        # would let the response kernel-echo at the cursor as `^[[68;1R`.
        if [[ -n "$1" && -n "$_chevron_dsr_inflight" ]]; then
            _chevron_drain_reports $fd "$1"
            _chevron_dsr_inflight=""
        fi
    } always {
        [[ -n "$_chevron_stty" ]] && stty "$_chevron_stty" 2>/dev/null
        exec {fd}<&-
    }
    if [[ "$resp" == *$'\e['* ]]; then
        local pre_esc="${resp%$'\e['*}"
        local payload="${resp##*$'\e['}"
        REPLY="${payload%%;*}"
        [[ "$REPLY" == <-> ]] || REPLY=""
        [[ -n "$pre_esc" && "$pre_esc" != *[^[:print:]$'\t\r\n']* ]] && print -z -- "$pre_esc" 2>/dev/null
    elif [[ -n "$resp" && "$resp" != *[^[:print:]$'\t\r\n']* ]]; then
        # CSI-less buffer: pure typeahead, truncated at a typed `R`.
        print -z -- "${resp}R" 2>/dev/null
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
    # End timestamp FIRST, before the instant-prompt takeover and the
    # sweep/rewrite machinery below spend their own time — the duration
    # tag measures the command only.
    local _chevron_cmd_end=$EPOCHREALTIME
    # Instant-prompt takeover (chevron-nf8). If the top-of-rc snippet
    # painted a cached prompt and redirected stdout/stderr to a buffer,
    # restore the real fds, close the spurious OSC 133 prompt region we
    # opened, clear the cached prompt line, then replay any output that
    # .zshrc produced. No-op when the snippet wasn't activated.
    if [[ -n "$_chevron_instant_active" ]]; then
        # No error suppression on the restore: redirections apply left to
        # right and exec persists ALL of them, so a trailing 2>/dev/null
        # was the FINAL word on fd 2 — every instant-prompt shell ran with
        # stderr on /dev/null from the first prompt on. A failed restore
        # prints into the buffer file instead, which is harmless.
        exec >&"$_chevron_instant_orig_stdout" 2>&"$_chevron_instant_orig_stderr"
        { exec {_chevron_instant_orig_stdout}>&- {_chevron_instant_orig_stderr}>&- } 2>/dev/null
        [[ "${CHEVRON_OSC133:-1}" != "0" ]] && printf '\e]133;D;0\a'
        print -n -- $'\r\e[2K'
        [[ -s "$_chevron_instant_buf" ]] && cat -- "$_chevron_instant_buf"
        rm -f -- "$_chevron_instant_buf" 2>/dev/null
        unset _chevron_instant_active _chevron_instant_buf \
              _chevron_instant_orig_stdout _chevron_instant_orig_stderr \
              _chevron_cache_file
    fi
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
        # Braced for the same reason as _chevron_query_row's open: a bare
        # exec would make the 2>/dev/null permanent for the whole shell.
        if { exec {_chevron_sweep_fd}< /dev/tty } 2>/dev/null; then
            local _chevron_sweep_stty=$(stty -g 2>/dev/null)
            # Interrupt-safe for the same reason as _chevron_query_row:
            # the 300 ms drain is a wide ^C target, and an unwound hook
            # would otherwise leave the terminal raw with echo off.
            {
                stty -echo -icanon min 0 time 0 2>/dev/null
                _chevron_drain_reports $_chevron_sweep_fd 30
            } always {
                [[ -n "$_chevron_sweep_stty" ]] && stty "$_chevron_sweep_stty" 2>/dev/null
                exec {_chevron_sweep_fd}<&-
            }
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
        # Linger 300 ms (30 hundredths) on timeout: unlike preexec's
        # query — whose straggler the sweep above catches a cycle later
        # — there is NO sweep between this query and ZLE taking the
        # terminal. An unabsorbed response would kernel-echo as literal
        # `^[[68;1R` the instant the helper restores echo.
        _chevron_query_row 30
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
            # Width an exact multiple of COLUMNS: the painted transient
            # ends in the auto-margin pending-wrap state, and emulators
            # DISAGREE on how the following newline advances from there
            # (one row or two — ble.sh's "xenl" trap). The saved row may
            # be off by one, which paints the duplicated-chevron glitch;
            # unknowable from here, so skip honestly.
            if (( _chevron_R_top > 0 \
                && _chevron_width % _chevron_cols != 0 \
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
    unset _chevron_transient_save_row _chevron_transient_save_geom \
          _chevron_transient_collapsed
    local duration_ms=0
    if [[ -n "$_chevron_cmd_start" && -n "$_chevron_cmd_end" ]]; then
        duration_ms=$(( (_chevron_cmd_end - _chevron_cmd_start) * 1000 ))
        duration_ms=${duration_ms%.*}
    fi
    unset _chevron_cmd_start
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
    # The generation only bumps in precmd, so a refresh landing in the
    # accept window — after the collapse painted, before the next
    # precmd — passes the guard above, and its reset-prompt would draw
    # a fresh full prompt over the just-collapsed line (chevron-6tc).
    # The result describes a finished cycle and the next precmd
    # re-renders regardless; the collapse flag brackets exactly that
    # window, so drop the result there.
    [[ -n "$_chevron_transient_collapsed" ]] && return
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
        # PS2 continuation lines: reset-prompt there corrupts the
        # secondary-prompt state (pure guards identically). The fresh
        # PROMPT still applies at the next repaint.
        [[ "$CONTEXT" != "cont" ]] && zle reset-prompt
    fi
}
# Transient prompt: rewrite the just-issued prompt line to a minimal stub
# (single chevron) so scrollback stays clean and copy-paste captures only
# the command, not the prompt chrome. Disable with CHEVRON_TRANSIENT=0.
_chevron_accept_line() {
    # Record that the collapse really painted — preexec's row save (and
    # with it the precmd rewrite) is gated on this, because execution
    # paths that bypass this widget leave the full prompt on screen.
    _chevron_transient_collapsed=1
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
        { exec {fd}<&- } 2>/dev/null
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
"#;

/// Bash init using `ShellConfig::default()`.
#[must_use]
pub fn init_bash() -> String {
    init_bash_with(&ShellConfig::default())
}

/// Bash init with a config-derived preamble.
///
/// Bash port intentionally skips the transient prompt that zsh and fish
/// implement. Bash has no clean equivalent of ZLE's accept-line override,
/// and the two viable workarounds (`bind -x` with cursor codes; DEBUG trap
/// with `$BASH_COMMAND`) both break on multi-line input, wrapped lines,
/// and compound commands. Same reason powerlevel10k stays zsh-only and
/// Starship's bash init doesn't transient-collapse. The post-exec
/// duration tag, however, ports cleanly and is enabled here.
#[must_use]
pub fn init_bash_with(cfg: &ShellConfig) -> String {
    format!("{}{}", shell_preamble_posix(cfg), BODY_BASH)
}

const BODY_BASH: &str = r#"_chevron_preexec() {
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
"#;

/// Fish init using `ShellConfig::default()`.
#[must_use]
pub fn init_fish() -> String {
    init_fish_with(&ShellConfig::default())
}

/// Fish init with a config-derived preamble. Fish uses a different
/// set-if-unset idiom (`set -q VAR; or set -gx VAR value`).
#[must_use]
pub fn init_fish_with(cfg: &ShellConfig) -> String {
    format!("{}{}", shell_preamble_fish(cfg), BODY_FISH)
}

const BODY_FISH: &str = r#"function fish_prompt
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
    # Belt-and-suspenders flag clear: fish_prompt normally consumes it
    # at the repaint, but an execute that skipped the repaint must not
    # leave it armed for the next prompt.
    set -e _chevron_transient_show
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
#
# Collapse only when the line will actually execute (gating ported from
# starship and oh-my-posh):
#   - with the completion pager open, Enter selects an entry — never
#     collapse;
#   - an incomplete buffer (open quote, unterminated block) makes
#     `execute` insert a newline instead of running — collapsing would
#     strand a bare chevron over a buffer still being edited;
#   - a syntactically valid or empty line really executes — collapse.
function _chevron_transient_enter
    if commandline --paging-mode
        commandline -f execute
        return
    end
    if commandline --is-valid; or test -z (commandline --current-buffer | string collect)
        set -g _chevron_transient_show 1
        commandline -f repaint
    end
    commandline -f execute
end
# fish_prompt consumes the flag at the repaint; this covers paths where
# that repaint never happens (line cancelled with ^C) so a stale flag
# cannot collapse the NEXT prompt.
function _chevron_transient_cancel --on-event fish_cancel
    set -e _chevron_transient_show
end
if test "$CHEVRON_TRANSIENT" != "0"
    bind \r _chevron_transient_enter
    bind \n _chevron_transient_enter
    # vi-mode users hit Enter from insert (and occasionally visual)
    # mode; the default-mode binding above does not cover those maps.
    bind -M insert \r _chevron_transient_enter
    bind -M insert \n _chevron_transient_enter
    bind -M visual \r _chevron_transient_enter
end
# Session id for the command-lifecycle hooks (chevron-1yn). Once per
# fish process; stays unset when CHEVRON_HISTORY=0 so the hooks
# short-circuit without forking.
if test "$CHEVRON_HISTORY" != "0"; and not set -q _chevron_session_id
    set -g _chevron_session_id (chevron event new-session 2>/dev/null)
end
"#;

// ── preamble generators ────────────────────────────────────────────────────

/// Generates a sh/bash/zsh preamble that exports defaults for the
/// `CHEVRON_*` env vars. `${VAR-default}` returns the existing value if
/// set (even if empty) and the default otherwise, so an `export` set
/// before sourcing this script still wins (env > config > default).
///
/// We avoid the seemingly-cleaner `: "${VAR:=value}"` idiom because the
/// `:` builtin is commonly aliased in user dotfiles (e.g., `alias :='cd
/// ..'` for quick directory-up navigation). Alias expansion in zsh runs
/// before builtin dispatch, so the `:` lines would expand to `cd ..` and
/// every preamble line would emit a `string not in pwd: ..` error
/// (chevron-8dt). Direct assignment bypasses the alias entirely.
fn shell_preamble_posix(cfg: &ShellConfig) -> String {
    let mut out = String::with_capacity(384);
    let _ = writeln!(
        out,
        "# Generated by `chevron init` — env vars override these defaults."
    );
    write_posix_default(&mut out, "CHEVRON_OSC133", bool_to_shell(cfg.osc133));
    write_posix_default(&mut out, "CHEVRON_TRANSIENT", bool_to_shell(cfg.transient));
    write_posix_default(
        &mut out,
        "CHEVRON_TRANSIENT_DURATION_MS",
        &cfg.transient_duration_ms.to_string(),
    );
    write_posix_default(&mut out, "CHEVRON_ASYNC", bool_to_shell(cfg.async_render));
    write_posix_default(&mut out, "CHEVRON_HISTORY", bool_to_shell(cfg.history));
    write_posix_default(&mut out, "CHEVRON_LIVE", bool_to_shell(cfg.live));
    out.push('\n');
    out
}

fn write_posix_default(out: &mut String, var: &str, value: &str) {
    let _ = writeln!(out, "export {var}=\"${{{var}-{value}}}\"");
}

/// Generates the fish equivalent of the posix preamble. fish has no `:=`
/// expansion, so we use `set -q VAR; or set -gx VAR value`.
fn shell_preamble_fish(cfg: &ShellConfig) -> String {
    let mut out = String::with_capacity(512);
    let _ = writeln!(
        out,
        "# Generated by `chevron init` — env vars override these defaults."
    );
    write_fish_default(&mut out, "CHEVRON_OSC133", bool_to_shell(cfg.osc133));
    write_fish_default(&mut out, "CHEVRON_TRANSIENT", bool_to_shell(cfg.transient));
    write_fish_default(
        &mut out,
        "CHEVRON_TRANSIENT_DURATION_MS",
        &cfg.transient_duration_ms.to_string(),
    );
    write_fish_default(&mut out, "CHEVRON_ASYNC", bool_to_shell(cfg.async_render));
    write_fish_default(&mut out, "CHEVRON_HISTORY", bool_to_shell(cfg.history));
    write_fish_default(&mut out, "CHEVRON_LIVE", bool_to_shell(cfg.live));
    out.push('\n');
    out
}

fn write_fish_default(out: &mut String, var: &str, value: &str) {
    let _ = writeln!(out, "set -q {var}; or set -gx {var} {value}");
}

const fn bool_to_shell(v: bool) -> &'static str {
    if v { "1" } else { "0" }
}

#[cfg(test)]
mod tests {
    use super::{
        init_bash, init_bash_with, init_fish, init_fish_with, init_zsh, init_zsh_with,
        shell_preamble_fish, shell_preamble_posix,
    };
    use crate::config::ShellConfig;

    // ── config-driven preamble ────────────────────────────────────────────

    #[test]
    fn posix_preamble_defaults_match_today() {
        let out = shell_preamble_posix(&ShellConfig::default());
        assert!(out.contains("export CHEVRON_OSC133=\"${CHEVRON_OSC133-1}\""));
        assert!(out.contains("export CHEVRON_TRANSIENT=\"${CHEVRON_TRANSIENT-1}\""));
        assert!(out.contains(
            "export CHEVRON_TRANSIENT_DURATION_MS=\"${CHEVRON_TRANSIENT_DURATION_MS-2000}\""
        ));
        assert!(out.contains("export CHEVRON_ASYNC=\"${CHEVRON_ASYNC-0}\""));
        assert!(out.contains("export CHEVRON_HISTORY=\"${CHEVRON_HISTORY-1}\""));
        assert!(out.contains("export CHEVRON_LIVE=\"${CHEVRON_LIVE-0}\""));
    }

    #[test]
    fn posix_preamble_reflects_config_overrides() {
        let cfg = ShellConfig {
            osc133: false,
            transient: false,
            transient_duration_ms: 500,
            async_render: true,
            history: false,
            live: true,
        };
        let out = shell_preamble_posix(&cfg);
        assert!(out.contains("export CHEVRON_OSC133=\"${CHEVRON_OSC133-0}\""));
        assert!(out.contains("export CHEVRON_TRANSIENT=\"${CHEVRON_TRANSIENT-0}\""));
        assert!(out.contains(
            "export CHEVRON_TRANSIENT_DURATION_MS=\"${CHEVRON_TRANSIENT_DURATION_MS-500}\""
        ));
        assert!(out.contains("export CHEVRON_ASYNC=\"${CHEVRON_ASYNC-1}\""));
        assert!(out.contains("export CHEVRON_HISTORY=\"${CHEVRON_HISTORY-0}\""));
        assert!(out.contains("export CHEVRON_LIVE=\"${CHEVRON_LIVE-1}\""));
    }

    #[test]
    fn posix_preamble_does_not_invoke_colon_builtin() {
        // Regression: `:` is commonly aliased to `cd ..` in dotfiles.
        // Lines starting with `:` would alias-expand and break (chevron-8dt).
        let out = shell_preamble_posix(&ShellConfig::default());
        for line in out.lines() {
            let trimmed = line.trim_start();
            assert!(
                !trimmed.starts_with(": "),
                "preamble must not start lines with `: ` (alias collision risk): {line:?}"
            );
        }
    }

    #[test]
    fn fish_preamble_uses_set_q_or_set_gx() {
        let out = shell_preamble_fish(&ShellConfig::default());
        assert!(out.contains("set -q CHEVRON_OSC133; or set -gx CHEVRON_OSC133 1"));
        assert!(out.contains("set -q CHEVRON_TRANSIENT; or set -gx CHEVRON_TRANSIENT 1"));
        assert!(out.contains(
            "set -q CHEVRON_TRANSIENT_DURATION_MS; or set -gx CHEVRON_TRANSIENT_DURATION_MS 2000"
        ));
    }

    #[test]
    fn init_zsh_with_prepends_preamble_before_body() {
        let out = init_zsh_with(&ShellConfig::default());
        let preamble_idx = out.find("export CHEVRON_OSC133").expect("preamble missing");
        let body_idx = out.find("_chevron_preexec").expect("body missing");
        assert!(preamble_idx < body_idx, "preamble must come before body");
    }

    #[test]
    fn init_bash_with_uses_posix_preamble() {
        let out = init_bash_with(&ShellConfig::default());
        assert!(out.contains("export CHEVRON_OSC133=\"${CHEVRON_OSC133-1}\""));
        assert!(out.contains("PROMPT_COMMAND=_chevron_precmd"));
    }

    #[test]
    fn init_fish_with_uses_fish_preamble() {
        let out = init_fish_with(&ShellConfig::default());
        assert!(out.contains("set -q CHEVRON_OSC133"));
        assert!(out.contains("function fish_prompt"));
    }

    #[test]
    fn init_zsh_zero_arg_matches_default_with() {
        assert_eq!(init_zsh(), init_zsh_with(&ShellConfig::default()));
    }

    #[test]
    fn init_bash_zero_arg_matches_default_with() {
        assert_eq!(init_bash(), init_bash_with(&ShellConfig::default()));
    }

    #[test]
    fn init_fish_zero_arg_matches_default_with() {
        assert_eq!(init_fish(), init_fish_with(&ShellConfig::default()));
    }

    // ── instant prompt (chevron-nf8) ──────────────────────────────────────

    #[test]
    fn instant_prompt_snippet_has_detection_marker() {
        let snippet = super::init_zsh_instant_prompt();
        assert!(
            snippet.contains(super::INSTANT_PROMPT_MARKER),
            "snippet must contain the marker so doctor can detect it"
        );
    }

    #[test]
    fn instant_prompt_snippet_reads_cache_file_without_forking() {
        let snippet = super::init_zsh_instant_prompt();
        // $(<file) is the zsh-builtin file-read — no fork.
        assert!(
            snippet.contains("$(<\"$_chevron_cache_file\")"),
            "snippet must use $(<file) builtin, not sed/cat (perf)"
        );
        assert!(
            !snippet.contains("sed -n"),
            "snippet must not fork sed (perf)"
        );
    }

    #[test]
    fn instant_prompt_snippet_gates_on_interactive_tty() {
        let snippet = super::init_zsh_instant_prompt();
        assert!(snippet.contains("-o interactive"));
        assert!(snippet.contains("-t 1"));
    }

    #[test]
    fn instant_prompt_snippet_skips_when_chevron_already_loaded() {
        // The `! typeset -f _chevron_make_prompt` guard prevents re-firing
        // when the user `source ~/.zshrc`s after the shell is up.
        let snippet = super::init_zsh_instant_prompt();
        assert!(snippet.contains("typeset -f _chevron_make_prompt"));
    }

    #[test]
    fn instant_prompt_snippet_only_paints_zsh_shaped_cache() {
        // Multi-shell guard: only paint when zsh-style %{ brackets are
        // present and bash-style square-bracket markers are absent.
        let snippet = super::init_zsh_instant_prompt();
        assert!(snippet.contains("*'%{'*"));
        assert!(snippet.contains("*'\\['*"));
    }

    #[test]
    fn instant_prompt_snippet_registers_zshexit_cleanup() {
        // Critical: if .zshrc errors before precmd fires, this hook must
        // restore the fds so the user's terminal isn't broken.
        let snippet = super::init_zsh_instant_prompt();
        assert!(snippet.contains("zshexit_functions+=(_chevron_instant_zshexit)"));
        assert!(snippet.contains("_chevron_instant_zshexit()"));
    }

    #[test]
    fn instant_prompt_snippet_uses_print_p_for_paint() {
        // `-P` is essential so %{…%} markers are recognised as zero-width.
        let snippet = super::init_zsh_instant_prompt();
        assert!(snippet.contains("print -nP -- \"$_chevron_cprompt\""));
    }

    #[test]
    fn zsh_precmd_takeover_block_present_in_body() {
        // Takeover block must be inside _chevron_precmd, before the OSC 133 D
        // emission for the just-finished command.
        let out = init_zsh();
        assert!(
            out.contains("Instant-prompt takeover"),
            "takeover block must be present"
        );
        assert!(
            out.contains("if [[ -n \"$_chevron_instant_active\" ]]"),
            "takeover must gate on the activation flag"
        );
        // exit_status must be captured BEFORE takeover (cat in takeover
        // would clobber $? otherwise).
        let body = out;
        let exit_idx = body
            .find("local exit_status=$?")
            .expect("exit_status capture");
        let takeover_idx = body
            .find("if [[ -n \"$_chevron_instant_active\" ]]")
            .expect("takeover gate");
        assert!(
            exit_idx < takeover_idx,
            "exit_status must be captured before the takeover clobbers $?"
        );
    }

    #[test]
    fn zsh_precmd_takeover_emits_osc133_d_under_gate() {
        let out = init_zsh();
        assert!(
            out.contains("printf '\\e]133;D;0\\a'"),
            "takeover must emit OSC 133 D to close the instant prompt region"
        );
    }

    #[test]
    fn zsh_precmd_takeover_clears_cached_paint() {
        let out = init_zsh();
        // \r\e[2K returns to col 1 and clears the cached prompt line.
        assert!(
            out.contains("print -n -- $'\\r\\e[2K'"),
            "takeover must clear the cached prompt line"
        );
    }

    #[test]
    fn zsh_precmd_takeover_unsets_state_vars() {
        let out = init_zsh();
        assert!(out.contains("unset _chevron_instant_active"));
        assert!(out.contains("_chevron_instant_buf"));
        assert!(out.contains("_chevron_instant_orig_stdout"));
        assert!(out.contains("_chevron_instant_orig_stderr"));
    }

    // ── original body assertions (preamble doesn't affect these) ──────────

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
        let body = body_of(&out, "_chevron_query_row() {");
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
        let body = body_of(&out, "_chevron_query_row() {");
        let echo_off = body.find("stty -echo -icanon").expect("echo guard");
        let query = body.find(r"printf '\e[6n'").expect("DSR query");
        assert!(echo_off < query, "-echo must be set before the query");
        // `stty -`/`$(stty` rather than bare `stty`: the preexec comment
        // block legitimately mentions Ghostty.
        let pre = body_of(&out, "_chevron_preexec() {");
        assert!(
            !pre.contains("stty -") && !pre.contains("$(stty"),
            "preexec must not duplicate the tty dance outside the helper"
        );
    }

    #[test]
    fn zsh_rewrite_only_after_real_collapse() {
        // Alternate accept widgets (accept-and-hold, custom widgets
        // calling .accept-line) bypass the override: the full prompt is
        // still on screen, and a rewrite sized for the collapsed line
        // would erase the wrong span over it. The row save (and with it
        // the rewrite) must be gated on the collapse having painted.
        let out = init_zsh();
        let widget = body_of(&out, "_chevron_accept_line() {");
        assert!(
            widget.contains("_chevron_transient_collapsed=1"),
            "accept-line override must record that the collapse painted"
        );
        let pre = body_of(&out, "_chevron_preexec() {");
        assert!(
            pre.contains(r#"-n "$_chevron_transient_collapsed""#),
            "preexec row save must be gated on the collapse flag"
        );
        assert!(
            out.contains("_chevron_transient_collapsed\n")
                || out.contains("_chevron_transient_collapsed "),
            "the flag must be cleared every cycle"
        );
    }

    #[test]
    fn zsh_async_reset_prompt_skips_ps2_continuations() {
        // reset-prompt during a PS2 continuation corrupts the
        // secondary-prompt state; the async callback must skip it there
        // (the fresh PROMPT still applies at the next repaint).
        let out = init_zsh();
        assert!(
            out.contains(r#"[[ "$CONTEXT" != "cont" ]] && zle reset-prompt"#),
            "async callback must not reset-prompt during PS2"
        );
    }

    #[test]
    fn zsh_async_callback_drops_results_in_accept_window() {
        // The generation stamp only advances in precmd, so a refresh
        // landing between accept-line and the next precmd passes the
        // stale guard — and repainting there draws a full prompt over
        // the just-collapsed line (chevron-6tc). The collapse flag
        // brackets that window; the callback must drop results inside
        // it, before any PROMPT mutation.
        let out = init_zsh();
        let cb = body_of(&out, "_chevron_async_callback() {");
        let gen_guard = cb
            .find("_chevron_async_spawn_gen")
            .expect("generation stale guard");
        let collapse_guard = cb
            .find(r#"-n "$_chevron_transient_collapsed" ]] && return"#)
            .expect("accept-window guard");
        let prompt_set = cb.find("PROMPT=").expect("PROMPT assignment");
        assert!(
            gen_guard < collapse_guard && collapse_guard < prompt_set,
            "accept-window guard must sit between the stale guard and the repaint"
        );
    }

    #[test]
    fn fish_transient_gates_on_validity_and_pager() {
        // Ported from starship/oh-my-posh: Enter with the completion
        // pager open selects an entry (never collapse); an incomplete
        // buffer inserts a newline instead of executing (collapsing
        // would strand a bare chevron over a live multiline edit); a
        // cancelled line must drop the armed flag.
        let out = init_fish();
        assert!(out.contains("commandline --paging-mode"));
        assert!(out.contains("commandline --is-valid"));
        assert!(out.contains("--on-event fish_cancel"));
        assert!(
            out.contains(r"bind -M insert \r _chevron_transient_enter"),
            "vi insert mode needs its own Enter binding"
        );
    }

    #[test]
    fn zsh_tty_dances_are_interrupt_safe() {
        // ^C during the exchange unwinds the hook chain mid-function;
        // only an always-block guarantees the stty restore and fd close
        // still run. Without it the terminal is left raw with echo off,
        // and the next exchange's `stty -g` save makes the wedge
        // permanent.
        let out = init_zsh();
        let query = body_of(&out, "_chevron_query_row() {");
        assert!(
            query.contains("} always {"),
            "query helper must restore the tty via an always-block"
        );
        let precmd = body_of(&out, "_chevron_precmd() {");
        assert!(
            precmd.contains("} always {"),
            "precmd sweep must restore the tty via an always-block"
        );
    }

    #[test]
    fn zsh_query_row_probes_raw_and_lingers_for_precmd() {
        let out = init_zsh();
        let body = body_of(&out, "_chevron_query_row() {");
        // Raw mode must precede the pending-input probe: select on a
        // canonical tty reports readability only at line boundaries, so
        // a cooked probe misses partial-line typeahead (paste leftovers
        // after an interactive `read -rs`) and the query then races
        // input the probe missed.
        let raw = body.find("stty -echo -icanon").expect("raw flip");
        let probe = body.find("zselect -t 0 -r").expect("probe");
        assert!(raw < probe, "raw mode must precede the probe");
        // A CSI-less read buffer is typeahead truncated at a typed `R`;
        // it must be re-injected (with the eaten delimiter restored),
        // never silently dropped.
        assert!(
            body.contains(r#"print -z -- "${resp}R""#),
            "truncated typeahead must be re-injected"
        );
        // precmd's rewrite query must linger on timeout: no sweep runs
        // between it and ZLE, so an unabsorbed straggler kernel-echoes.
        assert!(
            out.contains("_chevron_query_row 30"),
            "precmd's query must pass a linger window"
        );
    }

    #[test]
    fn zsh_exec_never_persists_stderr_suppression() {
        // A bare `exec` makes every redirection on it permanent: `exec
        // {fd}< /dev/tty 2>/dev/null` pointed the whole shell's stderr
        // at /dev/null from the first prompt cycle — error messages
        // vanished and ncurses reset/tset (which reach the terminal via
        // fd 2) aborted silently as "reset stopped working". Error
        // suppression on an exec must be scoped via a brace group.
        let out = init_zsh();
        assert!(
            out.contains("{ exec {fd}< /dev/tty } 2>/dev/null || return"),
            "query helper's tty open must scope its 2>/dev/null"
        );
        assert!(
            out.contains("if { exec {_chevron_sweep_fd}< /dev/tty } 2>/dev/null; then"),
            "precmd sweep's tty open must scope its 2>/dev/null"
        );
        // The instant-prompt fd restores must not end with a stderr
        // redirect: redirections apply left to right, so a trailing
        // 2>/dev/null is the final word on fd 2 and exec persists it.
        assert!(
            !out.contains(r#""$_chevron_instant_orig_stderr" 2>/dev/null"#),
            "instant takeover restore must not re-point fd 2 at /dev/null"
        );
        let snippet = super::init_zsh_instant_prompt();
        assert!(
            !snippet.contains(r#""$_chevron_instant_orig_stderr" 2>/dev/null"#),
            "zshexit restore must not re-point fd 2 at /dev/null"
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
        let body = body_of(&out, "_chevron_query_row() {");
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
        let body = body_of(&out, "_chevron_query_row() {");
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
        let body = body_of(&out, "_chevron_precmd() {");
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
        let body = body_of(&out, "_chevron_drain_reports() {");
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
        let pre = body_of(&out, "_chevron_preexec() {");
        assert!(
            pre.find("_chevron_query_row").unwrap()
                < pre.find("_chevron_cmd_start=$EPOCHREALTIME").unwrap(),
            "start timestamp must be stamped after the DSR machinery"
        );
        let pc = body_of(&out, "_chevron_precmd() {");
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
        let cb = body_of(&out, "_chevron_async_callback() {");
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
