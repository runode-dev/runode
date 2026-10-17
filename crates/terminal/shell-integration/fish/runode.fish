# runode 的 fish 集成：用 OSC 133 标出提示符、用户输入和命令输出的边界。
#
#   133;A  提示符开始       133;B  提示符结束、用户输入开始
#          带 redraw=1：改尺寸时 fish 会整个重画提示符，终端先把旧的清掉（libghostty-vt 默认不清）
#   133;C  命令开始执行     133;D  命令执行完（带退出码）
#   133;P;k=r  右侧提示符开始，画完后用 133;B 回到用户输入
#   6973;<口令>;cwd=…  runode 私有：shell 的当前目录（百分号编码），每次显示提示符前都发
#   6973;<口令>;path=…  runode 私有：shell 的 PATH（百分号编码），变了才发，界面找命令行工具时用
#   6973;<口令>;functions=…  runode 私有：shell 里定义的函数名，名字之间用 %20 隔开，变了才发，
#                看集成脚本装好了没有
#   6973;<口令>;command=…  runode 私有：紧接在 133;C 前面，说明这次命令开始是真的，值是用户
#                输入的命令原文（百分号编码），拿不到时为空，终端从屏幕上读；runode 只把这样
#                认过的命令当作真的跑过，报告给等它的前端
#   6973;<口令>;end=  runode 私有：紧接在 133;D 前面，说明这次命令结束是真的；runode 只认这样的
#                133;D，命令输出里伪造的、别家集成另发的都不算
#   6973;<口令>;version=2  runode 私有：集成脚本的版本，每条命令开始时和 command 一起发，2 起
#                133;D 前面有 end；升级后接手的 runode 据此知道这个 shell 会发 end
#
# runode 还经 RUNODE_SHELL_FEATURES 告诉集成脚本另外开哪些功能（逗号隔开，和 Ghostty 的
# GHOSTTY_SHELL_FEATURES 一个写法）：
#
#   cursor:blink、cursor:steady  提示符上把光标换成竖线（闪或不闪），跑命令前用 CSI 0 SP q 换回
#                配置的样式；fish 自己的 vi 模式管着光标（fish_vi_cursor）时不管
#
# 口令是 runode 启动这个 shell 时随机生成、经环境变量 RUNODE_REPORT_TOKEN 给的。runode 只认
# 带着这个口令的 6973 报告，屏幕上的别的输出伪造不了。没有口令时不发 6973 报告：比如 exec fish
# 或者在里面再开一层 fish，新的 shell 拿不到口令，runode 就沿用之前报告的内容。
#
# runode-reload 用同一个 fish 程序换掉当前的 shell（exec），重新读用户配置：改了 config.fish
# 以后，在早先启动的终端里用它，新的 shell 照样接上集成、带着同一个口令和同样的功能，目录不变。
# 直接 exec fish 会丢掉集成。
#
# runode 把这个文件所在的数据目录加进 XDG_DATA_DIRS，fish 启动时会自动加载
# vendor_conf.d 里的脚本。先把加进去的那一项去掉，免得在这个 shell 里启动的程序继承它。

# 口令最先读进不导出的变量，再从环境里删掉，这个 shell 里运行的程序都继承不到它。这个文件又被
# 加载一次时环境里已经没有口令，保留已经读到的。
if set -q RUNODE_REPORT_TOKEN
    if test -n "$RUNODE_REPORT_TOKEN"
        set -gu __runode_report_token $RUNODE_REPORT_TOKEN
    end
    set -e RUNODE_REPORT_TOKEN
end
# 另外开哪些功能也一样读进不导出的变量。
if set -q RUNODE_SHELL_FEATURES
    set -gu __runode_features (string split , -- "$RUNODE_SHELL_FEATURES")
    set -e RUNODE_SHELL_FEATURES
end

if set -q RUNODE_FISH_DATA_DIR
    # runode-reload 再把它加进去。
    set -gu __runode_data_dir $RUNODE_FISH_DATA_DIR
    # fish 不按冒号拆 XDG_DATA_DIRS，它是一整个字符串，拆开再找。
    set -l dirs (string split : -- "$XDG_DATA_DIRS")
    if set -l index (contains --index -- $RUNODE_FISH_DATA_DIR $dirs)
        set -e dirs[$index]
        if set -q dirs[1]
            set -gx XDG_DATA_DIRS (string join : -- $dirs)
        else
            set -e XDG_DATA_DIRS
        end
    end
    set -e RUNODE_FISH_DATA_DIR
end

status is-interactive; or exit 0
set -q __runode_integrated; and exit 0
set -g __runode_integrated 1

# 用户的 fish_prompt、fish_right_prompt 在这个脚本之后才定义，每次显示提示符前再看一眼，还没包
# 起来就包上，在前后加上提示符的开始和结束标记：用户之后还会换提示符（fish_config prompt choose、
# tide configure、funced），换上来的新函数也要包。右侧提示符画在用户输入的同一行末尾：不能用
# 133;A，它会先换到新的一行；画完要回到用户输入，之后敲的字才不会被当成提示符。
#
# 每条命令开始前换回用户自己的函数，下次显示提示符时再包：命令里 funcsave、fish_config prompt save 存下的，
# Python venv 的 activate.fish 拷走的，funced 改的，都是用户自己的提示符，不是这里的包装。存下包装的话，以后
# 开的每个 fish 都会去调一个不存在的函数。还原前看一眼提示符还是不是包装：别的 fish_prompt 事件处理器
# 可能在这之后又换了一个，那就不动它。
#
# 原来的提示符最先跑、输出先收起来，和标记一起打印：先打印标记的话 $status、$pipestatus 就成了
# printf 的，提示符显示不出上一条命令失败。fish 本来就是收起提示符的输出再画，收一道不改变什么。参数照传：
# 开了 fish_transient_prompt 时 fish 带 --final-rendering 重画提示符。
function __runode_wrap_prompt --on-event fish_prompt
    if functions -q fish_right_prompt; and not functions -q __runode_original_right_prompt
        functions -c fish_right_prompt __runode_original_right_prompt
        function fish_right_prompt
            set -l prompt (__runode_original_right_prompt $argv | string collect --no-trim-newlines)
            printf '\e]133;P;k=r\a%s\e]133;B\a' "$prompt"
        end
    end
    if functions -q fish_prompt; and not functions -q __runode_original_prompt
        functions -c fish_prompt __runode_original_prompt
        function fish_prompt
            set -l prompt (__runode_original_prompt $argv | string collect --no-trim-newlines)
            printf '\e]133;A;cl=line;redraw=1\a%s\e]133;B\a' "$prompt"
        end
    end
end

function __runode_unwrap_prompt --on-event fish_preexec
    for name in fish_prompt fish_right_prompt
        set -l original __runode_original_(string replace fish_ '' $name)
        functions -q $original; or continue
        if functions -q $name; and functions $name | string match -q "*$original*"
            functions -e $name
            functions -c $original $name
        end
        functions -e $original
    end
end

# cursor 功能：提示符上换成竖线（5 闪，6 不闪），命令开始前换回配置的样式。
set -l cursor
if contains cursor:blink $__runode_features
    set cursor 5
else if contains cursor:steady $__runode_features
    set cursor 6
end
if test -n "$cursor"
    function __runode_cursor_bar --on-event fish_prompt -V cursor
        functions -q fish_vi_cursor_handle; or printf '\e[%s q' $cursor
    end
    function __runode_cursor_reset --on-event fish_preexec
        functions -q fish_vi_cursor_handle; or printf '\e[0 q'
    end
end

# 换成一个新的 fish，重新读用户配置，接上集成：数据目录照 runode 启动时那样加回 XDG_DATA_DIRS，
# 口令和功能经环境变量交回去，runode 照样认得新 shell 的报告。换之前把历史写进文件，再报告这条
# 命令执行完了：exec 之后不会再有它的 133;D。和 runode 启动 fish 时一样关掉 fish 自己的 OSC 133
# （mark-prompt），提示符只由这个脚本标。
function runode-reload --description 'Restart this shell to reload its configuration'
    set -l fish (status fish-path 2>/dev/null)
    test -x "$fish"; or set fish fish
    set -l vars
    if set -q __runode_data_dir
        set -l dirs $XDG_DATA_DIRS
        # XDG_DATA_DIRS 没设时 fish 用这两个默认目录，加了我们的之后要显式带上。
        set -q dirs[1]; or set dirs /usr/local/share /usr/share
        set -a vars RUNODE_FISH_DATA_DIR=$__runode_data_dir XDG_DATA_DIRS=(string join : -- $__runode_data_dir $dirs)
    end
    test -z "$__runode_report_token"; or set -a vars RUNODE_REPORT_TOKEN=$__runode_report_token
    set -q __runode_features; and set -a vars RUNODE_SHELL_FEATURES=(string join , -- $__runode_features)
    history save
    __runode_command_end 0
    exec env $vars $fish --features no-mark-prompt -l
end

# 每次显示提示符前报告当前目录；PATH 和上次报告的不一样时也报告给 runode。
function __runode_report_path --on-event fish_prompt
    test -n "$__runode_report_token"; or return
    printf '\e]6973;%s;cwd=%s\a' $__runode_report_token (string escape --style=url -- $PWD)
    set -l path (string join : -- $PATH)
    if test "$path" != "$__runode_path"
        set -g __runode_path $path
        printf '\e]6973;%s;path=%s\a' $__runode_report_token (string escape --style=url -- $path)
    end
end

# 每次显示提示符前，把函数名报告给 runode，它据此看集成脚本装好了没有；和上次报告的一样就不发。
# 只报告以字母或数字开头、由字母数字和 `_.:+@,=-` 组成的名字，名字之间写 %20 就是百分号编码。
# 这些都是 fish 的内建命令，在命令替换里不起子进程。
function __runode_report_names --on-event fish_prompt
    test -n "$__runode_report_token"; or return
    set -l names (functions --names)
    # 列表为空时 string 会改读标准输入，先判断。
    set -l kept
    if set -q names[1]
        set kept (string match -r -- '^[[:alnum:]][[:alnum:]_.:+@,=-]*$' $names)
    end
    set -l joined (string replace -a ' ' %20 -- "$kept")
    if test "$joined" != "$__runode_names"
        set -g __runode_names $joined
        printf '\e]6973;%s;functions=%s\a' $__runode_report_token $joined
    end
end

# $argv[1] 是用户输入的命令原文，按百分号编码放进带口令的 command 报告，分号、换行和控制字符
# 都不会打断转义序列；拿不到时报告的原文为空，终端从屏幕上读。没有口令时只报告命令开始，
# runode 不把它报告给等命令的前端。
function __runode_preexec --on-event fish_preexec
    if test -n "$__runode_report_token"
        set -l command
        if test -n "$argv[1]"
            set command (string escape --style=url -- $argv[1])
        end
        printf '\e]6973;%s;version=2\a\e]6973;%s;command=%s\a' $__runode_report_token $__runode_report_token "$command"
    end
    printf '\e]133;C\a'
end

function __runode_postexec --on-event fish_postexec
    __runode_command_end $status
end

# 报告命令执行完了，退出码是 $argv[1]；有口令时前面先发带口令的 end 报告，runode 才认这个 133;D。
function __runode_command_end
    if test -n "$__runode_report_token"
        printf '\e]6973;%s;end=\a\e]133;D;%s\a' $__runode_report_token $argv[1]
    else
        printf '\e]133;D;%s\a' $argv[1]
    end
end
