#compdef runode rn
# zsh completion for runode and rn. runode works out the candidates itself
# (RUNODE_GET_COMPLETIONS); this only hands them to zsh.
# Install: echo 'eval "$(runode completions zsh)"' >> ~/.zshrc (after compinit),
# or save it as _runode in a directory on $fpath.

_runode() {
    local -a lines suggestions
    local kind line
    # The index of the current word, counting the program name as 0.
    lines=("${(@f)$(RUNODE_GET_COMPLETIONS=$((CURRENT - 1)) "${(Q)words[@]}" 2>/dev/null)}")
    kind=$lines[1]
    for line in "${(@)lines[2,-1]}"; do
        [[ -z $line ]] && continue
        # _describe separates the word from its description with a colon, so
        # colons inside the word (agent:claude) are escaped.
        suggestions+=("${${line%%$'\t'*}//:/\\:}:${line#*$'\t'}")
    done
    (( $#suggestions )) && _describe -t runode 'runode' suggestions
    case $kind in
        files) _files ;;
        dirs) _files -/ ;;
    esac
}

if [[ $zsh_eval_context[-1] == loadautofunc ]]; then
    _runode "$@"
else
    compdef _runode runode rn
fi
