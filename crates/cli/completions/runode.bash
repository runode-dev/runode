# bash completion for runode and rn. runode works out the candidates itself
# (RUNODE_GET_COMPLETIONS); this only hands them to bash. Works without the
# bash-completion package and with the bash 3.2 that macOS ships.
# Install: echo 'eval "$(runode completions bash)"' >> ~/.bashrc

_runode() {
    local -a words=()
    local cword=0 i word cur
    # Readline breaks words at colons (COMP_WORDBREAKS), so agent:claude arrives
    # as three words; glue them back together and keep track of the current one.
    for ((i = 0; i < ${#COMP_WORDS[@]}; i++)); do
        word=${COMP_WORDS[i]}
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
                        while IFS= read -r line; do COMPREPLY+=("$line"); done < <(compgen -f -- "$cur")
                    fi
                    ;;
                dirs)
                    if type compopt >/dev/null 2>&1; then compopt -o dirnames; else
                        while IFS= read -r line; do COMPREPLY+=("$line"); done < <(compgen -d -- "$cur")
                    fi
                    ;;
            esac
        else
            COMPREPLY+=("${line%%$'\t'*}")
        fi
    done < <(RUNODE_GET_COMPLETIONS=$cword "${words[@]}" 2>/dev/null)
    # Readline will only replace the part after the last colon.
    if [[ $COMP_WORDBREAKS == *:* && $cur == *:* ]]; then
        local prefix=${cur%:*}:
        for ((i = 0; i < ${#COMPREPLY[@]}; i++)); do
            COMPREPLY[i]=${COMPREPLY[i]#"$prefix"}
        done
    fi
}

complete -F _runode runode rn
