# shellcheck shell=bash
# What a bash shell in an nterm pane tells nterm, and the keys it gives it:
# the counterpart of the NTerm PowerShell module, for a machine whose shells a
# session daemon runs (Linux). Source it from ~/.bashrc:
#
#   [[ -n $NTERM_SHARE && -r $NTERM_SHARE/bash/nterm.bash ]] && . "$NTERM_SHARE/bash/nterm.bash"
#
# The daemon names its share folder to its shells as NTERM_SHARE. Outside a
# pane of nterm it does nothing.
[[ $- == *i* && ( -n ${NTERM_DAEMON-} || -n ${NTERM_PICKER_ENDPOINT-} ) ]] || return 0

# The folder the shell is in (OSC 7), before each prompt: pickers start there,
# and so does a new pane beside this one. The path is sent as it is.
__nterm_report_pwd() {
    printf '\e]7;kitty-shell-cwd://%s%s\a' "${HOSTNAME:-localhost}" "$PWD"
}
case ";${PROMPT_COMMAND-};" in
    *";__nterm_report_pwd;"*) ;;
    *) PROMPT_COMMAND="__nterm_report_pwd${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac

# Ctrl+Space: what Tab would complete for the word at the cursor, in a picker.
__nterm_here=${BASH_SOURCE[0]%/*}
if [[ -r $__nterm_here/npick-completion.bash ]]; then
    # shellcheck source=../npick-completion.bash
    . "$__nterm_here/npick-completion.bash"
fi
unset __nterm_here
