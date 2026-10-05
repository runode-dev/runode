# runode 用 `bash --rcfile 本文件` 启动交互式 bash。--rcfile 对登录 shell 不起作用，
# 所以这里照登录 shell 的顺序自己加载用户配置，再接上集成：用 OSC 133 标出提示符、
# 用户输入和命令输出的边界。
#
#   133;A  提示符开始       133;B  提示符结束、用户输入开始
#   133;C  命令开始执行     133;D  命令执行完（带退出码）
#   6973;path=…  runode 私有：shell 的 PATH（百分号编码），变了才发，补全跑命令时用
#   6973;aliases=… 以及 functions、builtins、keywords  runode 私有：shell 里的这些名字，
#                名字之间用 %20 隔开，变了才发，补命令名时用
#   6973;alias_values=…  runode 私有：别名展开成什么，一行一个「名字<Tab>值」，整体百分号
#                编码，变了才发（bash 4 起），补命令名时当说明显示
#
# 133;C 尽量带上命令原文（cmdline_url，百分号编码），终端不必再从屏幕上读。

if [ -r /etc/profile ]; then
    . /etc/profile
fi
for _runode_file in ~/.bash_profile ~/.bash_login ~/.profile; do
    if [ -r "$_runode_file" ]; then
        . "$_runode_file"
        break
    fi
done
unset _runode_file

if [ -z "${_runode_integrated-}" ]; then
    _runode_integrated=1
    # 已经显示过一次提示符：之后每次显示提示符都意味着上一条命令执行完了。
    _runode_prompted=
    # 上一次加好标记的 PS1、PS2：配置把它们换掉之后要重新加。
    _runode_ps1=
    _runode_ps2=
    # 上一次报告给 runode 的 PATH。
    _runode_path=
    # 上一次报告给 runode 的各种名字，按种类各存一个变量。
    _runode_names_aliases= _runode_names_functions= _runode_names_builtins= _runode_names_keywords=
    _runode_alias_values=
    # bash 5.3 起 compgen -V 能把结果直接放进数组，每次显示提示符都能重新收集而不起子进程。更老
    # 的 bash 只能用命令替换（要起子进程），只在第一次显示提示符时收集一次；别名例外，bash 4 起
    # 可以从 BASH_ALIASES 直接读。
    _runode_compgen_v=
    if [ "${BASH_VERSINFO[0]}" -gt 5 ] || { [ "${BASH_VERSINFO[0]}" -eq 5 ] && [ "${BASH_VERSINFO[1]}" -ge 3 ]; }; then
        _runode_compgen_v=1
    fi
    _runode_names_collected=
    # 接下来输入的命令进历史时会得到的编号，命令开始时据此确认历史里最新的一条就是它。
    _runode_histnext=
    _runode_bang='\!'
    # 4.4 起才有 PS0：命令开始执行前显示一次。更老的 bash 没有命令开始的标记。
    _runode_ps0=
    if [ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 4 ]; }; then
        _runode_ps0=1
    fi

    _runode_prompt_command() {
        if [ -n "$_runode_prompted" ]; then
            printf '\033]133;D;%s\007' "$_runode_last_status"
        fi
        _runode_prompted=1
        # ${var@P} 也是 4.4 才有，和 PS0 一起判断。
        if [ -n "$_runode_ps0" ]; then
            _runode_histnext=${_runode_bang@P}
        fi
        if [ "$PS1" != "$_runode_ps1" ]; then
            _runode_ps1='\[\033]133;A;cl=line\007\]'"$PS1"'\[\033]133;B\007\]'
            PS1=$_runode_ps1
        fi
        if [ "$PS2" != "$_runode_ps2" ]; then
            _runode_ps2='\[\033]133;A;k=s\007\]'"$PS2"'\[\033]133;B\007\]'
            PS2=$_runode_ps2
        fi
        if [ "$PATH" != "$_runode_path" ]; then
            _runode_path=$PATH
            _runode_urlencode "$PATH"
            printf '\033]6973;path=%s\007' "$_runode_encoded"
        fi
        _runode_report_names
    }

    # 把 $2 起的名字里以字母或数字开头、由字母数字和 `_.:+@,=-` 组成的那些用 %20 连起来（这样
    # 就已经是百分号编码），报告为 $1 这一种；和上次报告的一样就不发。
    _runode_report() {
        local kind=$1 name joined= var
        shift
        for name in "$@"; do
            [[ $name =~ ^[[:alnum:]][[:alnum:]_.:+@,=-]*$ ]] || continue
            joined+=${joined:+%20}$name
        done
        var=_runode_names_$kind
        if [ "$joined" != "${!var}" ]; then
            printf -v "$var" '%s' "$joined"
            printf '\033]6973;%s=%s\007' "$kind" "$joined"
        fi
    }

    # 别名展开成什么（bash 4 起从 BASH_ALIASES 读，不起子进程），和上次的一样就不发。值里有换行
    # 或 Tab 的不报告。
    _runode_report_alias_values() {
        local name value lines=
        for name in "${!BASH_ALIASES[@]}"; do
            value=${BASH_ALIASES[$name]}
            [[ $name =~ ^[[:alnum:]][[:alnum:]_.:+@,=-]*$ ]] || continue
            [[ $value == *[$'\n\t']* ]] && continue
            lines+=$name$'\t'$value$'\n'
        done
        [ "$lines" = "$_runode_alias_values" ] && return
        _runode_alias_values=$lines
        _runode_urlencode "$lines"
        printf '\033]6973;alias_values=%s\007' "$_runode_encoded"
    }

    # 把别名、函数、内建命令和关键字报告给 runode，补命令名时用。
    _runode_report_names() {
        local -a list
        if [ -n "$_runode_compgen_v" ]; then
            compgen -V list -a
            _runode_report aliases "${list[@]}"
            compgen -V list -A function
            _runode_report functions "${list[@]}"
            compgen -V list -b
            _runode_report builtins "${list[@]}"
            compgen -V list -k
            _runode_report keywords "${list[@]}"
            _runode_report_alias_values
            return
        fi
        if [ "${BASH_VERSINFO[0]}" -ge 4 ]; then
            _runode_report aliases "${!BASH_ALIASES[@]}"
            _runode_report_alias_values
        fi
        [ -n "$_runode_names_collected" ] && return
        _runode_names_collected=1
        local IFS=$'\n' GLOBIGNORE='*'
        if [ "${BASH_VERSINFO[0]}" -lt 4 ]; then
            _runode_report aliases $(compgen -a)
        fi
        _runode_report functions $(compgen -A function)
        _runode_report builtins $(compgen -b)
        _runode_report keywords $(compgen -k)
    }

    # 先记下退出码，再跑用户原有的 PROMPT_COMMAND，最后加标记。
    PROMPT_COMMAND="_runode_last_status=\$?;${PROMPT_COMMAND:+$PROMPT_COMMAND;}_runode_prompt_command"
    # 把 $1 按字节做百分号编码，结果放在 _runode_encoded 里：分号、换行、ESC、BEL 和
    # 非 ASCII 字节都不会打断转义序列。
    _runode_urlencode() {
        local LC_ALL=C s=$1 c i n
        _runode_encoded=
        for (( i = 0; i < ${#s}; i++ )); do
            c=${s:i:1}
            case $c in
                [A-Za-z0-9._~/-]) _runode_encoded+=$c ;;
                *)
                    printf -v n '%d' "'$c"
                    printf -v c '%%%02X' $(( n & 255 ))
                    _runode_encoded+=$c
                    ;;
            esac
        done
    }

    # 在 PS0 里运行：历史里最新一条的编号正是这条命令该有的编号时，它就是刚输入的命令，
    # 带上原文报告命令开始。命令没进历史（以空格开头、被 HISTIGNORE 忽略、关了历史等）时
    # 最新一条是别的命令，只报告命令开始，终端从屏幕上读。
    _runode_command_start() {
        local entry num
        entry=$(HISTTIMEFORMAT= builtin history 1)
        entry=${entry#"${entry%%[![:space:]]*}"}
        num=${entry%%[!0-9]*}
        entry=${entry#"$num"}
        entry=${entry#\*}
        entry=${entry#"  "}
        if [ -n "$num" ] && [ "$num" = "$_runode_histnext" ] && [ -n "$entry" ]; then
            _runode_urlencode "$entry"
            printf '\033]133;C;cmdline_url=%s\007' "$_runode_encoded"
        else
            printf '\033]133;C\007'
        fi
    }

    # 关了 promptvars 时 PS0 里的命令替换不展开，只发不带原文的标记。
    if [ -n "$_runode_ps0" ]; then
        if shopt -q promptvars; then
            PS0='$(_runode_command_start)'"${PS0-}"
        else
            PS0='\[\033]133;C\007\]'"${PS0-}"
        fi
    fi
fi
