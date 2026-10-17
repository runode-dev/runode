# bash completion for runode and rn. runode works out the candidates itself
# (RUNODE_GET_COMPLETIONS); this only hands them to bash. Works without the
# bash-completion package and with the bash 3.2 that macOS ships.
# Install: echo 'eval "$(runode completions bash)"' >> ~/.bashrc

# Readline does not quote -F candidates, and bash 3.2's printf %q mangles
# multibyte characters, so put a backslash before each character the shell
# would read specially. The result goes to REPLY to save a subshell per name.
_runode_escape() {
    local s=$1 c i
    REPLY=
    for ((i = 0; i < ${#s}; i++)); do
        c=${s:i:1}
        case $c in
            [[:space:]\'\"\$\&\(\)\;\<\>\|\\\`\*\?\[\!\{\}\#\~]) REPLY+=\\$c ;;
            *) REPLY+=$c ;;
        esac
    done
}

# Same for a path from compgen, but a leading ~ stays bare so the shell still
# expands it.
_runode_escape_file() {
    if [[ $1 == \~* ]]; then
        _runode_escape "${1:1}"
        REPLY=\~$REPLY
    else
        _runode_escape "$1"
    fi
}

# The other way round: drop the backslashes typed or inserted before characters.
_runode_unescape() {
    local s=$1 c i
    REPLY=
    for ((i = 0; i < ${#s}; i++)); do
        c=${s:i:1}
        if [[ $c == \\ ]] && ((i + 1 < ${#s})); then
            i=$((i + 1))
            c=${s:i:1}
        fi
        REPLY+=$c
    done
}

_runode() {
    local -a words=()
    local cword=0 i word cur
    # Readline breaks words at colons (COMP_WORDBREAKS), so agent:claude arrives
    # as three words; glue them back together and keep track of the current one.
    for ((i = 0; i < ${#COMP_WORDS[@]}; i++)); do
        # COMP_WORDS keeps the backslashes typed or inserted by _runode_escape;
        # runode matches against the unquoted text, as the candidates below are.
        _runode_unescape "${COMP_WORDS[i]}"
        word=$REPLY
        if [[ $COMP_WORDBREAKS == *:* && $i -gt 0 && ( $word == :* || ${COMP_WORDS[i - 1]} == *: ) ]]; then
            words[${#words[@]} - 1]+=$word
        else
            words+=("$word")
        fi
        if [[ $i -eq $COMP_CWORD ]]; then
            cword=$(( ${#words[@]} - 1 ))
        fi
    done
    cur=${words[cword]}
    COMPREPLY=()
    local kind='' line
    while IFS= read -r line; do
        if [[ -z $kind ]]; then
            kind=$line
            case $kind in
                files)
                    if type compopt >/dev/null 2>&1; then compopt -o default; else
                        while IFS= read -r line; do _runode_escape_file "$line"; COMPREPLY+=("$REPLY"); done < <(compgen -f -- "$cur")
                    fi
                    ;;
                dirs)
                    if type compopt >/dev/null 2>&1; then compopt -o dirnames; else
                        while IFS= read -r line; do _runode_escape_file "$line"; COMPREPLY+=("$REPLY"); done < <(compgen -d -- "$cur")
                    fi
                    ;;
            esac
        else
            _runode_escape "${line%%$'\t'*}"
            COMPREPLY+=("$REPLY")
        fi
    done < <(RUNODE_GET_COMPLETIONS=$cword "${words[@]}" 2>/dev/null)
    # Readline will only replace the part after the last colon.
    if [[ $COMP_WORDBREAKS == *:* && $cur == *:* ]]; then
        local prefix=${cur%:*}:
        _runode_escape "$prefix"
        prefix=$REPLY
        for ((i = 0; i < ${#COMPREPLY[@]}; i++)); do
            COMPREPLY[i]=${COMPREPLY[i]#"$prefix"}
        done
    fi
}

complete -F _runode runode rn
