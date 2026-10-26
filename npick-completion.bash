# shellcheck shell=bash
# Completion picker for bash: the Linux counterpart of profile.example.ps1's
# Ctrl+Space binding. Ctrl+Space lists what Tab would complete for the word at
# the cursor in an nterm picker; Enter replaces that word.
#
# Source it from ~/.bashrc. It binds Ctrl+Space only in nterm panes,
# where npick can reach the terminal. Uses bash-completion when it is loaded.

# Split the current command, up to the cursor, into words the way readline does
# for completion: whitespace separates words, COMP_WORDBREAKS characters such as
# = and : become words of their own, and ; | & ( start a new command. Sets
# COMP_WORDS, COMP_CWORD, COMP_LINE, COMP_POINT and __npick_start, the offset of
# the current word in the text.
__npick_split() {
    local text=$1 c word="" quote="" i escape=0 have=0 start=0 cmd_start=0
    local breaks=${COMP_WORDBREAKS:-$'"\'><=;|&(:'}
    breaks=${breaks//[\"\';|&()$' \t\n']/}
    COMP_WORDS=()
    for ((i = 0; i < ${#text}; i++)); do
        c=${text:i:1}
        if ((escape)); then
            word+=$c escape=0
            continue
        fi
        if [[ $quote ]]; then
            word+=$c
            if [[ $c == "$quote" ]]; then
                quote=""
            elif [[ $c == \\ && $quote == '"' ]]; then
                escape=1
            fi
            continue
        fi
        case $c in
            ' ' | $'\t')
                ((have)) && COMP_WORDS+=("$word")
                word="" have=0
                continue
                ;;
            ';' | '|' | '&' | '(' | ')' | $'\n')
                COMP_WORDS=() word="" have=0 cmd_start=$((i + 1))
                continue
                ;;
        esac
        if [[ $breaks == *"$c"* ]]; then
            ((have)) && COMP_WORDS+=("$word")
            COMP_WORDS+=("$c")
            word="" have=0
            continue
        fi
        ((have)) || start=$i
        have=1 word+=$c
        [[ $c == \\ ]] && escape=1
        [[ $c == [\"\'] ]] && quote=$c
    done
    ((have)) || start=${#text}
    COMP_CWORD=${#COMP_WORDS[@]}
    COMP_WORDS+=("$word")
    COMP_LINE=${text:cmd_start}
    COMP_POINT=${#COMP_LINE}
    __npick_start=$start
}

# The current word without its quoting, for compgen.
__npick_dequote() {
    local word=$1
    if [[ $word == \'* ]]; then
        word=${word#\'}
        word=${word%\'}
    elif [[ $word == \"* ]]; then
        word=${word#\"}
        word=${word%\"}
    fi
    printf '%s' "${word//\\/}"
}

# Completion functions call compopt, which only works during real completion.
# While they run, this stand-in records the options they ask for instead.
__npick_compopt() {
    local arg
    while (($#)); do
        arg=$1
        shift
        case $arg in
            -o) __npick_opts+=" $1 " && shift ;;
            +o) __npick_opts=${__npick_opts// $1 / } && shift ;;
        esac
    done
    return 0
}

# Run the completion registered for COMP_WORDS[0]. Fills __npick_reply and
# __npick_opts (the -o options, space separated).
__npick_run_spec() {
    local cmd=${COMP_WORDS[0]} cur=${COMP_WORDS[COMP_CWORD]}
    local prev=${COMP_WORDS[COMP_CWORD - 1]} spec attempt status i
    local dcur
    dcur=$(__npick_dequote "$cur")
    for attempt in 1 2; do
        spec=$(complete -p -- "$cmd" 2>/dev/null)
        if [[ -z $spec ]] && declare -F _comp_load >/dev/null; then
            _comp_load -- "$cmd" 2>/dev/null
            spec=$(complete -p -- "$cmd" 2>/dev/null)
        fi
        if [[ -z $spec ]]; then
            __npick_opts=" filenames "
            mapfile -t __npick_reply < <(compgen -f -- "$dcur")
            return
        fi
        # `complete -p` prints a shell-quoted `complete ... cmd` command.
        local -a parts=() args=()
        eval "parts=(${spec#complete })"
        unset 'parts[${#parts[@]}-1]'
        local func="" command=""
        __npick_opts=" "
        for ((i = 0; i < ${#parts[@]}; i++)); do
            case ${parts[i]} in
                -F) func=${parts[++i]} ;;
                -C) command=${parts[++i]} ;;
                -o) __npick_opts+="${parts[++i]} " ;;
                -[AGWXPS]) args+=("${parts[i]}" "${parts[++i]}") ;;
                -[abcdefgjksuv]) args+=("${parts[i]}") ;;
            esac
        done
        __npick_reply=()
        ((${#args[@]})) && mapfile -t __npick_reply < <(compgen "${args[@]}" -- "$dcur" 2>/dev/null)
        if [[ $command ]]; then
            # -C commands read COMP_LINE and COMP_POINT from their environment.
            mapfile -t -O "${#__npick_reply[@]}" __npick_reply < <(
                export COMP_LINE COMP_POINT
                eval "$command" '"$cmd" "$cur" "$prev"' 2>/dev/null
            )
        fi
        [[ $func ]] || break
        local -a saved=("${__npick_reply[@]}")
        COMPREPLY=()
        compopt() { __npick_compopt "$@"; }
        "$func" "$cmd" "$cur" "$prev" 2>/dev/null
        status=$?
        unset -f compopt
        __npick_reply=("${saved[@]}" "${COMPREPLY[@]}")
        # 124 means the function loaded a new completion; run that instead.
        ((status == 124)) || break
    done
    if ((${#__npick_reply[@]} == 0)); then
        if [[ $__npick_opts == *" dirnames "* ]]; then
            mapfile -t __npick_reply < <(compgen -d -- "$dcur")
        elif [[ $__npick_opts == *" default "* || $__npick_opts == *" bashdefault "* ]]; then
            __npick_opts+="filenames "
            mapfile -t __npick_reply < <(compgen -f -- "$dcur")
        fi
    fi
    if [[ $__npick_opts == *" plusdirs "* ]]; then
        mapfile -t -O "${#__npick_reply[@]}" __npick_reply < <(compgen -d -- "$dcur")
    fi
}

# List the completions for `text`, the line up to the cursor, as
# "display<TAB>insert" lines in __npick_items. Insert text is quoted for the
# shell and ends with a space unless the completion is a directory or asks for
# no space. Sets __npick_start.
__npick_complete_items() {
    local COMP_WORDS COMP_CWORD COMP_LINE COMP_POINT COMP_TYPE=9 COMP_KEY=9
    local __npick_reply=() __npick_opts=" " candidate insert display path
    __npick_split "$1"
    local cur=${COMP_WORDS[COMP_CWORD]}
    if ((COMP_CWORD == 0)) && [[ $cur != */* ]]; then
        mapfile -t __npick_reply < <(compgen -c -- "$cur")
    elif ((COMP_CWORD == 0)); then
        __npick_opts=" filenames "
        mapfile -t __npick_reply < <(compgen -f -- "$(__npick_dequote "$cur")")
    else
        __npick_run_spec
    fi
    # bash-completion quotes the word for compgen and relies on readline to
    # unquote it during real Tab completion, which a key binding cannot turn
    # on. So words with quotes, backslashes or ~ find nothing. Fall back to
    # bash's own completion for those, and for commands without completions.
    if ((${#__npick_reply[@]} == 0)) && [[ $cur != -* ]]; then
        local dcur
        dcur=$(__npick_dequote "$cur")
        if [[ $dcur == \$* ]]; then
            __npick_opts=" "
            mapfile -t __npick_reply < <(compgen -v -P '$' -- "${dcur#\$}")
        elif [[ ${COMP_WORDS[0]} == @(cd|pushd|rmdir) ]]; then
            __npick_opts=" filenames "
            mapfile -t __npick_reply < <(compgen -d -- "$dcur")
        else
            __npick_opts=" filenames "
            mapfile -t __npick_reply < <(compgen -f -- "$dcur")
        fi
    fi
    __npick_items=()
    if [[ $__npick_opts != *" nosort "* ]]; then
        mapfile -t __npick_reply < <(printf '%s\n' "${__npick_reply[@]}" | LC_ALL=C sort -u)
    fi
    for candidate in "${__npick_reply[@]}"; do
        [[ $candidate ]] || continue
        insert=$candidate display=$candidate
        if [[ $__npick_opts == *" filenames "* ]]; then
            path=$candidate
            [[ $path == "~/"* ]] && path=$HOME/${path#"~/"}
            if [[ $__npick_opts != *" noquote "* ]]; then
                if [[ $candidate == "~/"* ]]; then
                    insert="~/$(printf '%q' "${candidate#"~/"}")"
                else
                    insert=$(printf '%q' "$candidate")
                fi
            fi
            if [[ -d $path ]]; then
                insert+=/ display+=/
                __npick_items+=("$display"$'\t'"$insert")
                continue
            fi
        fi
        [[ $__npick_opts == *" nospace "* ]] || insert+=" "
        __npick_items+=("$display"$'\t'"$insert")
    done
}

# bash erases the prompt line before running a `bind -x` command and redraws
# it only when the command returns, so it would stay blank while the picker is
# open. Draw it again, with the cursor where it was, so the line stays visible
# and the cursor-positioned picker opens at the word being completed.
__npick_redraw_line() {
    local prompt=${PS1@P}
    prompt=${prompt##*$'\n'}
    prompt=${prompt//[$'\001\002']/}
    local rest=${READLINE_LINE:READLINE_POINT}
    printf '\r%s%s' "$prompt" "$READLINE_LINE" >/dev/tty
    ((${#rest})) && printf '\e[%dD' "${#rest}" >/dev/tty
    return 0
}

# Ctrl+Space: pick a completion for the word at the cursor and insert it.
__npick_complete() {
    local before=${READLINE_LINE:0:READLINE_POINT} __npick_items=() __npick_start choice
    __npick_complete_items "$before"
    case ${#__npick_items[@]} in
        0) return ;;
        1) choice=${__npick_items[0]#*$'\t'} ;;
        *)
            __npick_redraw_line
            choice=$(printf '%s\n' "${__npick_items[@]}" |
                npick --display 1 --value 2 --position Cursor)
            local status=$?
            # Clear our copy so bash's redraw starts from an empty line.
            printf '\r\e[K' >/dev/tty
            ((status == 0)) || return
            ;;
    esac
    # Don't double the space when the cursor is already before one.
    [[ $choice == *' ' && ${READLINE_LINE:READLINE_POINT:1} == ' ' ]] && choice=${choice% }
    READLINE_LINE=${READLINE_LINE:0:__npick_start}$choice${READLINE_LINE:READLINE_POINT}
    READLINE_POINT=$((__npick_start + ${#choice}))
}

if [[ $- == *i* && -n $GHOSTTY_PICKER_ENDPOINT ]] && command -v npick >/dev/null; then
    bind -x '"\C-@": __npick_complete'
fi
