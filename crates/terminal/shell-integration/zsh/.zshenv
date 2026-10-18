# runode 启动 zsh 时把 ZDOTDIR 指到这个目录，zsh 最先读到的就是这个文件。
# 这里先把 ZDOTDIR 还原，之后的 .zprofile、.zshrc 照常从用户自己的目录读取；
# 再加载用户自己的 .zshenv，交互式 shell 最后接上集成脚本。
#
# 这个文件可能在别名生效时被读取，命令都加了引号，免得被别名替换。

# runode 给的报告口令最先读进不导出的变量，集成脚本报告 PATH 等信息时带上它，见集成脚本开头的
# 说明。口令在一个只有自己能读的文件里，环境变量 RUNODE_REPORT_TOKEN_FILE 给出路径，读完马上删掉
# 文件和变量：口令不能直接放进环境变量，进程启动时的环境（ps -E）同一用户的程序都读得到，unset 也
# 改不掉。早先版本的 runode-reload 直接给 RUNODE_REPORT_TOKEN，升级后在旧 shell 里换 shell 时照样认。
if [[ -n "${RUNODE_REPORT_TOKEN_FILE-}" ]]; then
    'builtin' 'typeset' -g +x _runode_report_token=
    { 'builtin' 'read' -r _runode_report_token < "$RUNODE_REPORT_TOKEN_FILE" } 2>/dev/null
    'command' 'rm' -f -- "$RUNODE_REPORT_TOKEN_FILE"
elif [[ -n "${RUNODE_REPORT_TOKEN-}" ]]; then
    'builtin' 'typeset' -g +x _runode_report_token="$RUNODE_REPORT_TOKEN"
fi
'builtin' 'unset' 'RUNODE_REPORT_TOKEN' 'RUNODE_REPORT_TOKEN_FILE'
# 集成脚本另外开哪些功能也一样读进不导出的变量，见集成脚本里的 cursor。
'builtin' 'typeset' -g +x _runode_features="${RUNODE_SHELL_FEATURES-}"
'builtin' 'unset' 'RUNODE_SHELL_FEATURES'

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
