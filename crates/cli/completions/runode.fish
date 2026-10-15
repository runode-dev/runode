# fish completion for runode and rn. runode works out the candidates itself
# (RUNODE_GET_COMPLETIONS); this only hands them to fish.
# Install: runode completions fish > ~/.config/fish/conf.d/runode.fish

function __runode_complete
    set -l words (commandline --current-process --tokenize --cut-at-cursor)
    # The current token is not in $words; runode counts the program name as 0,
    # so the index of the current token is the number of words before it.
    set -l current (commandline --current-token --cut-at-cursor)
    set -l lines (env RUNODE_GET_COMPLETIONS=(count $words) $words $current 2>/dev/null)
    # The first line says what else to complete: normal, files or dirs.
    switch "$lines[1]"
        case files
            __fish_complete_path $current
        case dirs
            __fish_complete_directories $current
    end
    if test (count $lines) -gt 1
        printf '%s\n' $lines[2..-1]
    end
end

complete -c runode -c rn -f -a '(__runode_complete)'
