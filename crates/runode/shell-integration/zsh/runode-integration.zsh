# runode 的 zsh 集成：用 OSC 133 标出提示符、用户输入和命令输出的边界。
#
#   133;A  提示符开始       133;B  提示符结束、用户输入开始
#   133;C  命令开始执行     133;D  命令执行完（带退出码）
#   133;P;k=r  右侧提示符开始，画完后用 133;B 回到用户输入
#   6973;path=…  runode 私有：shell 的 PATH（百分号编码），变了才发，补全跑命令时用
#   6973;aliases=… 以及 functions、builtins、keywords  runode 私有：shell 里的这些名字，
#                名字之间用 %20 隔开，变了才发，补命令名时用
#   6973;alias_values=…  runode 私有：别名展开成什么，一行一个「名字<Tab>值」，整体百分号
#                编码，变了才发，补命令名时当说明显示
#
# 标记直接写进 PS1、PS2、RPROMPT，提示符因为改窗口大小等原因重画时会跟着重发。
# 133;C 带上用户输入的命令原文（cmdline_url，百分号编码），终端不必再从屏幕上读。

[[ -o interactive ]] || 'builtin' 'return' 0
(( ${+_runode_integrated} )) && 'builtin' 'return' 0
'builtin' 'typeset' -g _runode_integrated=1

# 上一个提示符之后执行过命令，下一次显示提示符前要报告它执行完了。
'builtin' 'typeset' -g _runode_ran=
# 上一次加好标记的 PS1、PS2、RPROMPT：提示符框架把它们换掉之后要重新加。
'builtin' 'typeset' -g _runode_ps1= _runode_ps2= _runode_rps1=
# 上一次报告给 runode 的 PATH。
'builtin' 'typeset' -g _runode_path=
# 上一次报告给 runode 的各种名字，按种类。别名、函数这些在 zsh/parameter 模块提供的数组里。
'builtin' 'typeset' -gA _runode_names _runode_names_raw
'builtin' 'zmodload' -i zsh/parameter 2>/dev/null

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
    # 右侧提示符画在用户输入的同一行末尾。不能用 133;A，它会先换到新的一行；画完要回到
    # 用户输入，之后敲的字才不会被当成提示符。
    if [[ -n $RPROMPT && $RPROMPT != "$_runode_rps1" ]]; then
        _runode_rps1=$'%{\e]133;P;k=r\a%}'"$RPROMPT"$'%{\e]133;B\a%}'
        RPROMPT=$_runode_rps1
    fi
    if [[ $PATH != "$_runode_path" ]]; then
        _runode_path=$PATH
        'builtin' 'local' REPLY
        _runode_urlencode "$PATH"
        'builtin' 'print' -rn -- $'\e]6973;path='"$REPLY"$'\a'
    fi
    _runode_report_names
    # 有的插件会在运行时往钩子列表里追加函数，每次都把自己挪回两头。
    if [[ ${precmd_functions[1]} != _runode_save_status || ${precmd_functions[-1]} != _runode_precmd ]]; then
        precmd_functions=(_runode_save_status ${precmd_functions:#_runode_(save_status|precmd)} _runode_precmd)
    fi
}

# 把别名、函数、内建命令和关键字报告给 runode，补命令名时用；和上次报告的一样就不发。只报告
# 以字母或数字开头、由字母数字和 `_.:+@,=-` 组成的名字：用不着逐字编码，名字之间写 %20 就是
# 百分号编码，也顺带滤掉补全系统和 runode 自己以 `_` 开头的内部函数。全是 zsh 内部的展开，
# 不起子进程。
_runode_report_names() {
    'builtin' 'emulate' -L zsh -o extended_glob
    'builtin' 'local' kind raw joined
    'builtin' 'local' -a names
    for kind in aliases functions builtins keywords; do
        case $kind in
            (aliases) names=(${(k)aliases}) ;;
            (functions) names=(${(k)functions}) ;;
            (builtins) names=(${(k)builtins}) ;;
            (keywords) names=($reswords) ;;
        esac
        # 先按原样比一次：没变时（绝大多数提示符）省掉过滤和排序。
        raw=${(j: :)names}
        [[ $raw == "${_runode_names_raw[$kind]-}" ]] && continue
        _runode_names_raw[$kind]=$raw
        names=(${(o)${(M)names:#[[:alnum:]][[:alnum:]_.:+@,=-]#}})
        joined=${(j:%20:)names}
        if [[ $joined != "${_runode_names[$kind]-}" ]]; then
            _runode_names[$kind]=$joined
            'builtin' 'print' -rn -- $'\e]6973;'"$kind=$joined"$'\a'
        fi
    done
    # 别名的值可能含任何字，逐字编码比较慢，只在别名有变化时做。值里有换行或 Tab 的不报告。
    raw=${(j: :)${(kv)aliases}}
    [[ $raw == "${_runode_names_raw[alias_values]-}" ]] && return
    _runode_names_raw[alias_values]=$raw
    'builtin' 'local' name lines= REPLY
    for name in ${(ok)aliases}; do
        [[ $name == [[:alnum:]][[:alnum:]_.:+@,=-]# && ${aliases[$name]} != *[$'\n\t']* ]] || continue
        lines+=$name$'\t'${aliases[$name]}$'\n'
    done
    _runode_urlencode "$lines"
    'builtin' 'print' -rn -- $'\e]6973;alias_values='"$REPLY"$'\a'
}

# 把 $1 按字节做百分号编码，结果放在 REPLY 里：分号、换行、ESC、BEL 和非 ASCII 字节都不会
# 打断转义序列。
_runode_urlencode() {
    'builtin' 'emulate' -L zsh
    'builtin' 'local' LC_ALL=C s=$1 c
    'builtin' 'local' -i i
    REPLY=
    for (( i = 1; i <= $#s; i++ )); do
        c=${s[i]}
        if [[ $c == [A-Za-z0-9._~/-] ]]; then
            REPLY+=$c
        else
            REPLY+=%${(l:2::0:)$(( [##16] #c & 255 ))}
        fi
    done
}

# $1 是用户输入的命令原文；拿不到时只报告命令开始，终端从屏幕上读。
_runode_preexec() {
    if [[ -n $1 ]]; then
        'builtin' 'local' REPLY
        _runode_urlencode "$1"
        'builtin' 'print' -rn -- $'\e]133;C;cmdline_url='"$REPLY"$'\a'
    else
        'builtin' 'print' -rn -- $'\e]133;C\a'
    fi
    _runode_ran=1
}

'builtin' 'typeset' -ga precmd_functions preexec_functions
precmd_functions=(_runode_save_status $precmd_functions _runode_precmd)
preexec_functions+=(_runode_preexec)
