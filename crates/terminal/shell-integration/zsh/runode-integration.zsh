# runode 的 zsh 集成：用 OSC 133 标出提示符、用户输入和命令输出的边界。
#
#   133;A  提示符开始       133;B  提示符结束、用户输入开始
#          带 redraw=1：改尺寸时 zsh 会整个重画多行提示符，终端先把旧的清掉，不然旧提示符折行后
#          留在上面（libghostty-vt 默认不清）
#   133;C  命令开始执行     133;D  命令执行完（带退出码）
#   133;P;k=r  右侧提示符开始，画完后用 133;B 回到用户输入
#   6973;<口令>;cwd=…  runode 私有：shell 的当前目录（百分号编码），每次显示提示符前都发；
#                提示符出来后插件管理器可能临时切进插件目录，runode 那时再读目录就不准了
#   6973;<口令>;path=…  runode 私有：shell 的 PATH（百分号编码），变了才发，补全跑命令时用
#   6973;<口令>;aliases=… 以及 functions、builtins、keywords  runode 私有：shell 里的这些
#                名字，名字之间用 %20 隔开，变了才发，补命令名时用
#   6973;<口令>;alias_values=…  runode 私有：别名展开成什么，一行一个「名字<Tab>值」，整体
#                百分号编码，变了才发，补命令名时当说明显示
#   6973;<口令>;command=…  runode 私有：紧接在 133;C 前面，说明这次命令开始是真的，值是用户
#                输入的命令原文（百分号编码），拿不到时为空，终端从屏幕上读；runode 只把这样
#                认过的命令记进历史
#
# 标记直接写进 PS1、PS2、RPROMPT，提示符因为改窗口大小等原因重画时会跟着重发。
#
# runode 还经 RUNODE_SHELL_FEATURES 告诉集成脚本另外开哪些功能（逗号隔开，和 Ghostty 的
# GHOSTTY_SHELL_FEATURES 一个写法），.zshenv 读进 _runode_features：
#
#   cursor:blink、cursor:steady  提示符上把光标换成竖线（闪或不闪），vi 命令模式和可视模式里是
#                方块，跑命令前用 CSI 0 SP q 换回配置的样式
#
# 口令是 runode 启动这个 shell 时随机生成、经环境变量 RUNODE_REPORT_TOKEN 给的，集成目录里的
# .zshenv 最先把它读进不导出的 _runode_report_token 并从环境里删掉。runode 只认带着这个口令的
# 6973 报告，屏幕上的别的输出伪造不了。没有口令时不发 6973 报告：比如 exec zsh 或者在里面再开
# 一层 zsh，新的 shell 拿不到口令，runode 就沿用之前报告的内容。
#
# runode-reload 用同一个 zsh 程序换掉当前的 shell（exec），重新读用户配置：改了 .zshrc 以后，
# 在早先启动的终端里用它，新的 shell 照样接上集成、带着同一个口令和同样的功能，目录不变。
# 直接 exec zsh 会丢掉集成。

[[ -o interactive ]] || 'builtin' 'return' 0
(( ${+_runode_integrated} )) && 'builtin' 'return' 0
'builtin' 'typeset' -g _runode_integrated=1

# 这个脚本所在的集成目录，runode-reload 把 ZDOTDIR 指回这里。
'builtin' 'typeset' -g _runode_dir=${${(%):-%x}:A:h}

# 上一个提示符之后执行过命令，下一次显示提示符前要报告它执行完了。
'builtin' 'typeset' -g _runode_ran=
# 上一次加好标记的 PS1、PS2、RPROMPT：提示符框架把它们换掉之后要重新加。
'builtin' 'typeset' -g _runode_ps1= _runode_ps2= _runode_rps1=
# 上一次报告给 runode 的 PATH。
'builtin' 'typeset' -g _runode_path=
# cursor 功能开着时提示符上竖线的样式：5 闪，6 不闪；为空时不管光标。方块是它减 4。
'builtin' 'typeset' -g _runode_cursor=
case ,${_runode_features-}, in
    (*,cursor:blink,*) _runode_cursor=5 ;;
    (*,cursor:*) _runode_cursor=6 ;;
esac
# 已经把换光标挂到 zle 的钩子上了，见 _runode_hook_zle。
'builtin' 'typeset' -g _runode_zle_hooked=
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
        _runode_ps1=$'%{\e]133;A;cl=line;redraw=1\a%}'"$PS1"$'%{\e]133;B\a%}'
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
    if [[ -n ${_runode_report_token-} ]]; then
        'builtin' 'local' REPLY
        _runode_urlencode "$PWD"
        'builtin' 'print' -rn -- $'\e]6973;'"$_runode_report_token;cwd=$REPLY"$'\a'
        if [[ $PATH != "$_runode_path" ]]; then
            _runode_path=$PATH
            _runode_urlencode "$PATH"
            'builtin' 'print' -rn -- $'\e]6973;'"$_runode_report_token;path=$REPLY"$'\a'
        fi
    fi
    _runode_report_names
    # 用户配置自己定义的 zle-line-init、zle-keymap-select 要先定义好，所以等 .zshrc 加载完、第一次
    # 显示提示符时才挂。
    if [[ -n $_runode_cursor && -z $_runode_zle_hooked ]]; then
        _runode_zle_hooked=1
        _runode_hook_zle line-init
        _runode_hook_zle keymap-select
    fi
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
    [[ -n ${_runode_report_token-} ]] || 'builtin' 'return' 0
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
            'builtin' 'print' -rn -- $'\e]6973;'"$_runode_report_token;$kind=$joined"$'\a'
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
    'builtin' 'print' -rn -- $'\e]6973;'"$_runode_report_token;alias_values=$REPLY"$'\a'
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

# 按 zle 当前的键位换光标：vi 命令模式和可视模式里是方块，别的（emacs、vi 插入模式）是竖线。
_runode_zle_cursor() {
    case ${KEYMAP-} in
        (vicmd|visual) 'builtin' 'print' -rn -- $'\e['$(( _runode_cursor - 4 ))' q' ;;
        (*) 'builtin' 'print' -rn -- $'\e['$_runode_cursor' q' ;;
    esac
}

# 挂在 zle-line-init、zle-keymap-select 上的 widget：先换光标，再调用户原来的那个（_runode_hook_zle
# 改名留下的），它自己也换光标时以它为准。
_runode_zle_widget() {
    _runode_zle_cursor
    'builtin' 'local' orig=._runode_orig_$WIDGET
    (( ${+widgets[$orig]} )) || 'builtin' 'return' 0
    'builtin' 'zle' $orig -N${_runode_zle_flags[$WIDGET]-} -- "$@"
}

# 把换光标挂到 zle 的 $1 钩子（line-init、keymap-select）上，照 Ghostty 的办法：这个钩子已经由
# add-zle-hook-widget 管着时交给它，排在最后，不然自己包一层会触发 add-zle-hook-widget 的问题；
# 用户自己定义了这个 widget 时改名留着，由 _runode_zle_widget 调它，名字以点开头，
# zsh-syntax-highlighting 不会再包它；都没有时直接定义。
_runode_hook_zle() {
    'builtin' 'zmodload' -i zsh/zleparameter 2>/dev/null
    'builtin' 'local' widget=zle-$1
    if [[ ${widgets[$widget]-} == user:azhw:* ]] && (( ${+functions[add-zle-hook-widget]} )); then
        add-zle-hook-widget $1 _runode_zle_cursor
        'builtin' 'return'
    fi
    if (( ${+widgets[$widget]} )); then
        'builtin' 'zle' -A $widget ._runode_orig_$widget
        # 照 Ghostty：用户定义的不带 -w 调，它看到的 $WIDGET 还是钩子的名字；别的带 -w。
        [[ ${widgets[$widget]} == user:* ]] && _runode_zle_flags[$widget]= || _runode_zle_flags[$widget]=w
    fi
    'builtin' 'zle' -N $widget _runode_zle_widget
}
'builtin' 'typeset' -gA _runode_zle_flags

# $1 是用户输入的命令原文，放在带口令的 command 报告里；拿不到时报告的原文为空，终端从屏幕上
# 读。没有口令时只报告命令开始，runode 不把它记进历史。
_runode_preexec() {
    if [[ -n ${_runode_report_token-} ]]; then
        'builtin' 'local' REPLY=
        [[ -z $1 ]] || _runode_urlencode "$1"
        'builtin' 'print' -rn -- $'\e]6973;'"$_runode_report_token;command=$REPLY"$'\a'
    fi
    # 提示符上换过光标的，在命令开始前换回配置的样式。
    [[ -z $_runode_cursor ]] || 'builtin' 'print' -rn -- $'\e[0 q'
    'builtin' 'print' -rn -- $'\e]133;C\a'
    _runode_ran=1
}

# 吞掉漏到命令行上的 SGR 鼠标报告（ESC [ < 按键;列;行 M 或 m）。开着鼠标上报的程序退出前
# 已经不读输入、还没关上报的那一小段里，鼠标一动报告就留在终端里，等提示符出来被 zsh 读到：
# ESC [ < 没有绑定被丢掉，剩下的「51;72;30M」就成了用户敲的字。绑定匹配到 ESC [ < 以后这里
# 读到 M 或 m 为止；最多读 32 个字，每个字最多等 0.1 秒，用户真的敲了 ESC [ < 也卡不住。
_runode_drop_mouse_report() {
    'builtin' 'local' c
    'builtin' 'local' -i n
    for (( n = 0; n < 32; n++ )); do
        'builtin' 'read' -rs -k1 -t 0.1 c || 'builtin' 'break'
        [[ $c == [Mm] ]] && 'builtin' 'break'
    done
}
'builtin' 'zle' -N _runode_drop_mouse_report
'builtin' 'bindkey' -M emacs '\e[<' _runode_drop_mouse_report
'builtin' 'bindkey' -M viins '\e[<' _runode_drop_mouse_report
'builtin' 'bindkey' -M vicmd '\e[<' _runode_drop_mouse_report

# 换成一个新的 zsh，重新读用户配置，接上集成：ZDOTDIR 指回集成目录，现在的 ZDOTDIR 经
# RUNODE_ZSH_ZDOTDIR 交给集成目录里的 .zshenv 还原；口令和功能经环境变量交回去，runode 照样认得
# 新 shell 的报告。换之前把这个 shell 的历史写进文件，再报告这条命令执行完了：exec 之后不会再有
# 它的 133;D。
runode-reload() {
    'builtin' 'emulate' -L zsh
    # 启动这个 shell 的程序；登录 shell 的 argv[0] 可能带着开头的 -，或者只是个名字。
    'builtin' 'local' zsh=${ZSH_ARGZERO#-}
    [[ $zsh == */* ]] || zsh=${commands[$zsh]-}
    [[ -x $zsh ]] || zsh=${commands[zsh]:-/bin/zsh}
    'builtin' 'local' -a vars
    vars=(ZDOTDIR=$_runode_dir RUNODE_SHELL_FEATURES=${_runode_features-})
    (( ${+ZDOTDIR} )) && vars+=(RUNODE_ZSH_ZDOTDIR=$ZDOTDIR)
    [[ -z ${_runode_report_token-} ]] || vars+=(RUNODE_REPORT_TOKEN=$_runode_report_token)
    'builtin' 'fc' -AI 2>/dev/null
    'builtin' 'print' -rn -- $'\e]133;D;0\a'
    _runode_ran=
    'builtin' 'exec' 'env' $vars $zsh -l
}

'builtin' 'typeset' -ga precmd_functions preexec_functions
precmd_functions=(_runode_save_status $precmd_functions _runode_precmd)
preexec_functions+=(_runode_preexec)
