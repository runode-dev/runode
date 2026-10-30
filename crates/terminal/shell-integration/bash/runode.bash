# runode 用 `bash --rcfile 本文件` 启动交互式 bash。--rcfile 对登录 shell 不起作用，
# 所以这里照登录 shell 的顺序自己加载用户配置，再接上集成：用 OSC 133 标出提示符、
# 用户输入和命令输出的边界。
#
#   133;A  提示符开始       133;B  提示符结束、用户输入开始
#          带 redraw=last：改尺寸时 readline 只重画提示符的最后一行，终端只清那一行
#   133;C  命令开始执行     133;D  命令执行完（带退出码）
#   133;I  提示符结束、用户输入开始，输入到这一行的换行为止：bash 4.4 以下没有 133;C，用它
#          代替 133;B
#   6973;<口令>;cwd=…  runode 私有：shell 的当前目录（百分号编码），每次显示提示符前都发
#   6973;<口令>;path=…  runode 私有：shell 的 PATH（百分号编码），变了才发，界面找命令行工具时用
#   6973;<口令>;functions=…  runode 私有：shell 里定义的函数名，名字之间用 %20 隔开，变了才发，
#                看集成脚本装好了没有
#   6973;<口令>;command=…  runode 私有：紧接在 133;C 前面，说明这次命令开始是真的，值尽量是
#                命令原文（百分号编码），拿不到时为空，终端从屏幕上读；runode 只把这样认过的
#                命令当作真的跑过，报告给等它的前端。bash 4.4 起才有，更老的 bash 没有命令开始的标记
#   6973;<口令>;end=  runode 私有：紧接在 133;D 前面，说明这次命令结束是真的；runode 只认这样的
#                133;D，命令输出里伪造的、别家集成另发的都不算
#   6973;<口令>;version=2  runode 私有：集成脚本的版本，每条命令开始时和 command 一起发，2 起
#                133;D 前面有 end；升级后接手的 runode 据此知道这个 shell 会发 end
#
# 口令是 runode 启动这个 shell 时随机生成、经 RUNODE_REPORT_TOKEN_FILE 指着的文件给的。runode 只认
# 带着这个口令的 6973 报告，屏幕上的别的输出伪造不了。没有口令时不发 6973 报告：比如 exec bash
# 或者在里面再开一层 bash，新的 shell 拿不到口令，runode 就沿用之前报告的内容。
#
# runode 还经 RUNODE_SHELL_FEATURES 告诉集成脚本另外开哪些功能（逗号隔开，和 Ghostty 的
# GHOSTTY_SHELL_FEATURES 一个写法）：
#
#   cursor:blink、cursor:steady  提示符上把光标换成竖线（闪或不闪），跑命令前用 CSI 0 SP q 换回
#                配置的样式。要在命令开始时换回，所以 bash 4.4 起有 PS0 才做
#
# runode-reload 用同一个 bash 程序换掉当前的 shell（exec），重新读用户配置：改了 .bashrc 等文件
# 以后，在早先启动的终端里用它，新的 shell 照样接上集成、带着同一个口令和同样的功能，目录不变。
# 直接 exec bash 会丢掉集成。
#
# 用到口令的函数都把标准错误接到 /dev/null：开着 set -x 时 bash 把展开后的命令行（带着口令）打到
# 标准错误，也就是屏幕上。这些函数里只有集成自己的命令，不吞用户的输出。设了 BASH_XTRACEFD 时跟踪
# 不走标准错误，所以 4.4 起（有 `local -`）进函数先关掉跟踪，返回时还原；BASH_XTRACEFD 4.1 起才有，
# 4.1 到 4.3 挡不住。

# 加载用户配置之前就把口令读进不导出的变量，删掉文件和环境变量：口令不能直接放进环境变量，进程
# 启动时的环境（ps -E）同一用户的程序都读得到，unset 也改不掉。早先版本的 runode-reload 直接给
# RUNODE_REPORT_TOKEN，升级后在旧 shell 里换 shell 时照样认。这个文件又被加载一次时环境里已经没有
# 口令，保留已经读到的。
if [ -n "${RUNODE_REPORT_TOKEN_FILE-}" ]; then
    # 先删掉同名变量：它要是从环境里继承来的，直接赋值还会带着导出属性。
    unset _runode_report_token
    _runode_report_token=
    read -r _runode_report_token 2>/dev/null <"$RUNODE_REPORT_TOKEN_FILE"
    command rm -f -- "$RUNODE_REPORT_TOKEN_FILE"
elif [ -n "${RUNODE_REPORT_TOKEN-}" ]; then
    unset _runode_report_token
    _runode_report_token=$RUNODE_REPORT_TOKEN
fi
unset RUNODE_REPORT_TOKEN RUNODE_REPORT_TOKEN_FILE
# 另外开哪些功能也一样读进不导出的变量。
if [ -n "${RUNODE_SHELL_FEATURES+X}" ]; then
    unset _runode_features
    _runode_features=$RUNODE_SHELL_FEATURES
fi
unset RUNODE_SHELL_FEATURES

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
    # 这个文件的路径，runode-reload 再用它启动 bash。
    _runode_rcfile=${BASH_SOURCE[0]}
    # 已经显示过一次提示符：之后每次显示提示符都意味着上一条命令执行完了。
    _runode_prompted=
    # 上一次加好标记的 PS1、PS2：配置把它们换掉之后要重新加。
    _runode_ps1=
    _runode_ps2=
    # 上一次报告给 runode 的 PATH。
    _runode_path=
    # Git Bash、MSYS2 和 Cygwin 里的路径写成 /c/Users/…，PATH 用冒号隔开，Windows 上的 runode 用不了：报告
    # 前用 cygpath 换成 Windows 的写法。cygpath 是外部程序，只在目录或 PATH 变了时调；换过的目录记在
    # _runode_cwd（原样）和 _runode_cwd_native（换好的）里。
    _runode_windows=
    case ${OSTYPE-} in msys* | cygwin*) _runode_windows=1 ;; esac
    _runode_cwd=
    _runode_cwd_native=
    # 上一次报告给 runode 的函数名。
    _runode_names=
    # bash 5.3 起 compgen -V 能把结果直接放进数组，每次显示提示符都能重新收集而不起子进程。更老
    # 的 bash 只能用命令替换（要起子进程），只在第一次显示提示符时收集一次。
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
    # 提示符之后用户输入开始的标记。终端在输入状态下遇到换行，会把下一行也当成输入（续行），
    # 直到 133;C 说输出开始；没有 133;C 的老 bash 里命令的输出就全成了输入，按提示符切出来的
    # 命令输出（`runode read --command`）读成空的，命令在跑时也像是还在提示符上编辑。所以老
    # bash 用 133;I：输入到换行为止，之后是输出。bash 的输入不会不带续行提示符地跨行（续行
    # 都有 PS2，PS2 自己标成续行），只有 4.4 起括号粘贴进来的多行命令会，这时有 133;C，还用
    # 133;B 把多行都算输入。
    _runode_input_mark=I
    if [ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 4 ]; }; then
        _runode_ps0=1
        _runode_input_mark=B
    fi
    # cursor 功能开着时写进 PS1 的竖线（5 闪，6 不闪），为空时不管光标。
    _runode_cursor=
    if [ -n "$_runode_ps0" ]; then
        case ,${_runode_features-}, in
            *,cursor:blink,*) _runode_cursor='\[\033[5 q\]' ;;
            *,cursor:*) _runode_cursor='\[\033[6 q\]' ;;
        esac
    fi

    _runode_prompt_command() {
        [ -z "$_runode_ps0" ] || { local -; set +x; }
        if [ -n "$_runode_prompted" ]; then
            _runode_command_end "$_runode_last_status"
        fi
        _runode_prompted=1
        # PROMPT_COMMAND 里的 HISTCMD 就是接下来输入的命令的编号；这时展开的 \! 常常还是上一条命令的。用户
        # unset 过 HISTCMD 时它只是个普通变量，才退回 \!。${var@P} 也是 4.4 才有，和 PS0 一起判断。
        if [ -n "$_runode_ps0" ]; then
            case ${HISTCMD-} in
                '' | *[!0-9]*) _runode_histnext=${_runode_bang@P} ;;
                *) _runode_histnext=$HISTCMD ;;
            esac
        fi
        if [ "$PS1" != "$_runode_ps1" ]; then
            _runode_ps1='\[\033]133;A;cl=line;redraw=last\007\]'"$PS1$_runode_cursor"'\[\033]133;'$_runode_input_mark'\007\]'
            PS1=$_runode_ps1
        fi
        if [ "$PS2" != "$_runode_ps2" ]; then
            _runode_ps2='\[\033]133;A;k=s\007\]'"$PS2"'\[\033]133;'$_runode_input_mark'\007\]'
            PS2=$_runode_ps2
        fi
        if [ -n "${_runode_report_token-}" ]; then
            if [ -z "$_runode_windows" ]; then
                _runode_cwd_native=$PWD
            elif [ "$PWD" != "$_runode_cwd" ]; then
                _runode_cwd=$PWD
                _runode_cwd_native=$(command cygpath -w -- "$PWD") || _runode_cwd_native=$PWD
            fi
            _runode_urlencode "$_runode_cwd_native"
            printf '\033]6973;%s;cwd=%s\007' "$_runode_report_token" "$_runode_encoded"
        fi
        if [ -n "${_runode_report_token-}" ] && [ "$PATH" != "$_runode_path" ]; then
            _runode_path=$PATH
            if [ -n "$_runode_windows" ]; then
                local native
                native=$(command cygpath -w -p -- "$PATH") || native=$PATH
                _runode_urlencode "$native"
            else
                _runode_urlencode "$PATH"
            fi
            printf '\033]6973;%s;path=%s\007' "$_runode_report_token" "$_runode_encoded"
        fi
        _runode_report_names
    } 2>/dev/null

    # 把函数名报告给 runode，它据此看集成脚本装好了没有：只报以字母或数字开头、由字母数字和
    # `_.:+@,=-` 组成的名字，用 %20 连起来（这样就已经是百分号编码）；和上次报告的一样就不发。
    _runode_report_names() {
        [ -n "${_runode_report_token-}" ] || return 0
        local -a list
        local name joined=
        if [ -n "$_runode_compgen_v" ]; then
            compgen -V list -A function
        else
            [ -n "$_runode_names_collected" ] && return
            _runode_names_collected=1
            # 逐行读进数组，不靠按 IFS 拆分加 GLOBIGNORE 挡通配：局部的 GLOBIGNORE 出了函数被还原成
            # 未设置时，bash 会顺手关掉用户自己开的 dotglob。
            local names
            names=$(compgen -A function)
            while IFS= read -r name; do list+=("$name"); done <<<"$names"
        fi
        for name in "${list[@]}"; do
            [[ $name =~ ^[[:alnum:]][[:alnum:]_.:+@,=-]*$ ]] || continue
            joined+=${joined:+%20}$name
        done
        [ "$joined" = "$_runode_names" ] && return
        _runode_names=$joined
        printf '\033]6973;%s;functions=%s\007' "$_runode_report_token" "$joined"
    }

    # 报告命令执行完了，退出码是 $1；有口令时前面先发带口令的 end 报告，runode 才认这个 133;D。
    _runode_command_end() {
        [ -z "$_runode_ps0" ] || { local -; set +x; }
        if [ -n "${_runode_report_token-}" ]; then
            printf '\033]6973;%s;end=\007\033]133;D;%s\007' "$_runode_report_token" "$1"
        else
            printf '\033]133;D;%s\007' "$1"
        fi
    } 2>/dev/null

    # 把 $? 换回记下的退出码。调用时接上 `&& :`：用户开了 `set -e` 时，返回非零的函数单独一条会让
    # shell 退出，放在 `&&` 前面不会，退出码照样留在 $? 里。
    _runode_restore_status() {
        return "$_runode_last_status"
    }

    # 先记下退出码，再跑用户原有的 PROMPT_COMMAND，最后加标记：标记要在最后加，前面的命令改了
    # PS1 也照样标上。bash 5.1 起 PROMPT_COMMAND 可以是数组，逐个元素跑，每个元素开始时 $? 都是
    # 上一条命令的退出码；是数组时在前后各加一个元素，后面的元素也在标记之前跑。按属性认数组：
    # 稀疏的数组（`([10]=…)`）没有下标 0，`${PROMPT_COMMAND+X}` 认不出来。字符串形式的
    # 记下退出码这一步把 $? 变成 0，跑用户的命令前换回去；用换行连接：用户的值以 `;`、`&` 结尾
    # 或只有空白时，拼上 `;` 会成语法错误。
    if [ "${BASH_VERSINFO[0]}" -gt 5 ] || { [ "${BASH_VERSINFO[0]}" -eq 5 ] && [ "${BASH_VERSINFO[1]}" -ge 1 ]; } &&
        [[ ${PROMPT_COMMAND[@]@a} == *a* ]]; then
        PROMPT_COMMAND=('_runode_last_status=$?' "${PROMPT_COMMAND[@]}" _runode_prompt_command)
    else
        PROMPT_COMMAND="_runode_last_status=\$?"$'\n'"${PROMPT_COMMAND:+_runode_restore_status && :$'\n'$PROMPT_COMMAND$'\n'}_runode_prompt_command"
    fi
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

    # 在 PS0 里运行：先发带口令的 command 报告，再报告命令开始。历史里最新一条的编号正是这条
    # 命令该有的编号时，它就是刚输入的命令，报告里带上原文。命令没进历史（以空格开头、被
    # HISTIGNORE 忽略、关了历史等）时最新一条是别的命令，报告的原文为空，终端从屏幕上读。
    # 没有口令时只报告命令开始，runode 不把它报告给等命令的前端。
    _runode_command_start() {
        [ -z "$_runode_ps0" ] || { local -; set +x; }
        if [ -n "${_runode_report_token-}" ]; then
            local entry num
            entry=$(HISTTIMEFORMAT= builtin history 1)
            entry=${entry#"${entry%%[![:space:]]*}"}
            num=${entry%%[!0-9]*}
            entry=${entry#"$num"}
            entry=${entry#\*}
            entry=${entry#"  "}
            _runode_encoded=
            # 逐字编码的耗时随长度平方涨，粘贴的长命令不编码，原文留空。
            if [ -n "$num" ] && [ "$num" = "$_runode_histnext" ] && [ -n "$entry" ] && [ "${#entry}" -le 4096 ]; then
                _runode_urlencode "$entry"
            fi
            printf '\033]6973;%s;version=2\007\033]6973;%s;command=%s\007' "$_runode_report_token" \
                "$_runode_report_token" "$_runode_encoded"
        fi
        # 提示符上换过光标的，在命令开始前换回配置的样式。
        [ -z "$_runode_cursor" ] || printf '\033[0 q'
        printf '\033]133;C\007'
    } 2>/dev/null

    # 吞掉漏到命令行上的 SGR 鼠标报告（ESC [ < 按键;列;行 M 或 m）。开着鼠标上报的程序退出前
    # 已经不读输入、还没关上报的那一小段里，鼠标一动报告就留在终端里，等提示符出来被 readline
    # 读到：ESC [ < 没有绑定被丢掉，剩下的「51;72;30M」就成了用户敲的字。readline 的
    # skip-csi-sequence 读完一个 CSI 序列的参数和结尾字节，bash 4.2 起才有；更老的 bash 在
    # bind -x 的函数里读不到剩下的字节，只能不管。
    if [ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 2 ]; }; then
        bind '"\e[<": skip-csi-sequence' 2>/dev/null
    fi

    # runode 换 shell 时按的专用按键：把整行换成 runode-reload 执行。runode 看到 runode-reload-key 这个
    # 函数就只按它，不再挪光标、退格清输入：vi 命令模式里退格不删字，回车会把用户原来那行执行掉。
    # bind -x 的函数改不了行就执行，所以按键绑成宏：先按另一个序列让函数换掉整行，再按回车。
    # READLINE_LINE 要 bash 4，bind -x 在 vi 命令键位里也好用的版本保守地取 4.4。
    if [ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 4 ]; }; then
        runode-reload-key() {
            READLINE_LINE=' runode-reload'
            READLINE_POINT=${#READLINE_LINE}
        }
        for _runode_keymap in emacs vi-insert vi-command; do
            bind -m "$_runode_keymap" -x '"\e[6974~": runode-reload-key' 2>/dev/null
            bind -m "$_runode_keymap" '"\e[6973~": "\e[6974~\C-m"' 2>/dev/null
        done
        unset _runode_keymap
    fi

    # 换成一个新的 bash，重新读用户配置，接上集成：口令写进新的临时文件（mktemp 建的只有自己能读写），
    # 和功能一起经环境变量交回去，runode 照样认得新 shell 的报告。换之前把这个 shell 的历史写进文件，
    # 再报告这条命令执行完了：exec 之后不会再有它的 133;D。
    runode-reload() {
        [ -z "$_runode_ps0" ] || { local -; set +x; }
        local bash=${BASH:-bash}
        local -a vars=()
        local file
        if { [ -n "${_runode_report_token-}" ] && file=$(command mktemp) &&
            printf '%s\n' "$_runode_report_token" >|"$file"; } 2>/dev/null; then
            vars+=("RUNODE_REPORT_TOKEN_FILE=$file")
        fi
        [ -z "${_runode_features+X}" ] || vars+=("RUNODE_SHELL_FEATURES=$_runode_features")
        builtin history -a 2>/dev/null
        _runode_command_end 0
        _runode_prompted=
        exec env ${vars[@]+"${vars[@]}"} "$bash" --rcfile "$_runode_rcfile"
    }

    # 关了 promptvars 时 PS0 里的命令替换不展开，只发不带报告的标记，runode 不记这些命令。
    if [ -n "$_runode_ps0" ]; then
        if shopt -q promptvars; then
            PS0='$(_runode_command_start)'"${PS0-}"
        else
            PS0='\[\033]133;C\007\]'"${PS0-}"
            [ -z "$_runode_cursor" ] || PS0='\[\033[0 q\]'$PS0
        fi
    fi
fi
