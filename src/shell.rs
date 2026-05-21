#[must_use]
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
    local job_count=${(%):-%j}
    local chevron_output
    chevron_output="$(chevron prompt 20 $exit_status $duration_ms $job_count)"
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
    set -l exit_status $status
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
}
