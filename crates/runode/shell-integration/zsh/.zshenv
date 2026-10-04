# runode 启动 zsh 时把 ZDOTDIR 指到这个目录，zsh 最先读到的就是这个文件。
# 这里先把 ZDOTDIR 还原，之后的 .zprofile、.zshrc 照常从用户自己的目录读取；
# 再加载用户自己的 .zshenv，交互式 shell 最后接上集成脚本。
#
# 这个文件可能在别名生效时被读取，命令都加了引号，免得被别名替换。

if [[ -n "${RUNODE_ZSH_ZDOTDIR+X}" ]]; then
    'builtin' 'export' ZDOTDIR="$RUNODE_ZSH_ZDOTDIR"
    'builtin' 'unset' 'RUNODE_ZSH_ZDOTDIR'
else
    'builtin' 'unset' 'ZDOTDIR'
fi

{
    'builtin' 'typeset' _runode_file="${ZDOTDIR-$HOME}/.zshenv"
    [[ ! -r "$_runode_file" ]] || 'builtin' 'source' '--' "$_runode_file"
} always {
    if [[ -o 'interactive' ]]; then
        # 集成脚本和这个文件放在同一个目录。
        'builtin' 'typeset' _runode_file="${${(%):-%x}:A:h}/runode-integration.zsh"
        [[ ! -r "$_runode_file" ]] || 'builtin' 'source' '--' "$_runode_file"
    fi
    'builtin' 'unset' '_runode_file'
}
