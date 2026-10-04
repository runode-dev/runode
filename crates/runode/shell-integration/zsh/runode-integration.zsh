# runode 的 zsh 集成：用 OSC 133 标出提示符、用户输入和命令输出的边界。
#
#   133;A  提示符开始       133;B  提示符结束、用户输入开始
#   133;C  命令开始执行     133;D  命令执行完（带退出码）
#
# 标记直接写进 PS1、PS2，提示符因为改窗口大小等原因重画时会跟着重发。

[[ -o interactive ]] || 'builtin' 'return' 0
(( ${+_runode_integrated} )) && 'builtin' 'return' 0
'builtin' 'typeset' -g _runode_integrated=1

# 上一个提示符之后执行过命令，下一次显示提示符前要报告它执行完了。
'builtin' 'typeset' -g _runode_ran=
# 上一次加好标记的 PS1、PS2：提示符框架把它们换掉之后要重新加。
'builtin' 'typeset' -g _runode_ps1= _runode_ps2=

# 排在 precmd 钩子的最前面，在别的钩子改掉 $? 之前记下退出码。
_runode_save_status() {
    'builtin' 'typeset' -g _runode_last_status=$?
}

# 排在 precmd 钩子的最后面，别的钩子改完 PS1 之后再加标记。
_runode_precmd() {
    if [[ -n $_runode_ran ]]; then
        'builtin' 'print' -rn -- $'\e]133;D;'"$_runode_last_status"$'\a'
        _runode_ran=
    fi
    if [[ $PS1 != "$_runode_ps1" ]]; then
        _runode_ps1=$'%{\e]133;A;cl=line\a%}'"$PS1"$'%{\e]133;B\a%}'
        PS1=$_runode_ps1
    fi
    if [[ $PS2 != "$_runode_ps2" ]]; then
        _runode_ps2=$'%{\e]133;A;k=s\a%}'"$PS2"$'%{\e]133;B\a%}'
        PS2=$_runode_ps2
    fi
    # 有的插件会在运行时往钩子列表里追加函数，每次都把自己挪回两头。
    if [[ ${precmd_functions[1]} != _runode_save_status || ${precmd_functions[-1]} != _runode_precmd ]]; then
        precmd_functions=(_runode_save_status ${precmd_functions:#_runode_(save_status|precmd)} _runode_precmd)
    fi
}

_runode_preexec() {
    'builtin' 'print' -rn -- $'\e]133;C\a'
    _runode_ran=1
}

'builtin' 'typeset' -ga precmd_functions preexec_functions
precmd_functions=(_runode_save_status $precmd_functions _runode_precmd)
preexec_functions+=(_runode_preexec)
