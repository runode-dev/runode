# runode 的 fish 集成：用 OSC 133 标出提示符、用户输入和命令输出的边界。
#
#   133;A  提示符开始       133;B  提示符结束、用户输入开始
#   133;C  命令开始执行     133;D  命令执行完（带退出码）
#   133;P;k=r  右侧提示符开始，画完后用 133;B 回到用户输入
#   6973;path=…  runode 私有：shell 的 PATH（百分号编码），变了才发，补全跑命令时用
#   6973;aliases=… 以及 functions、builtins  runode 私有：shell 里的缩写（当作别名）、函数和
#                内建命令，名字之间用 %20 隔开，变了才发，补命令名时用
#
# 133;C 带上用户输入的命令原文（cmdline_url，百分号编码），终端不必再从屏幕上读。
#
# runode 把这个文件所在的数据目录加进 XDG_DATA_DIRS，fish 启动时会自动加载
# vendor_conf.d 里的脚本。先把加进去的那一项去掉，免得在这个 shell 里启动的程序继承它。

if set -q RUNODE_FISH_DATA_DIR
    if set -l index (contains --index -- $RUNODE_FISH_DATA_DIR $XDG_DATA_DIRS)
        set -e XDG_DATA_DIRS[$index]
        if test (count $XDG_DATA_DIRS) -eq 0
            set -e XDG_DATA_DIRS
        end
    end
    set -e RUNODE_FISH_DATA_DIR
end

status is-interactive; or exit 0
set -q __runode_integrated; and exit 0
set -g __runode_integrated 1

# 用户的 fish_prompt、fish_right_prompt 在这个脚本之后才定义，第一次显示提示符前再把它们
# 包起来，在前后加上提示符的开始和结束标记。右侧提示符画在用户输入的同一行末尾：不能用
# 133;A，它会先换到新的一行；画完要回到用户输入，之后敲的字才不会被当成提示符。
function __runode_wrap_prompt --on-event fish_prompt
    functions -e __runode_wrap_prompt
    if functions -q fish_right_prompt
        functions -c fish_right_prompt __runode_original_right_prompt
        function fish_right_prompt
            printf '\e]133;P;k=r\a'
            __runode_original_right_prompt
            printf '\e]133;B\a'
        end
    end
    functions -q fish_prompt; or return
    functions -c fish_prompt __runode_original_prompt
    function fish_prompt
        printf '\e]133;A;cl=line\a'
        __runode_original_prompt
        printf '\e]133;B\a'
    end
end

# 每次显示提示符前，PATH 和上次报告的不一样就报告给 runode。
function __runode_report_path --on-event fish_prompt
    set -l path (string join : -- $PATH)
    if test "$path" != "$__runode_path"
        set -g __runode_path $path
        printf '\e]6973;path=%s\a' (string escape --style=url -- $path)
    end
end

# 每次显示提示符前，把缩写（当作别名）、函数和内建命令报告给 runode，补命令名时用；和上次报告的
# 一样就不发。只报告以字母或数字开头、由字母数字和 `_.:+@,=-` 组成的名字，名字之间写 %20 就是
# 百分号编码。这些都是 fish 的内建命令，在命令替换里不起子进程。
function __runode_report_names --on-event fish_prompt
    for kind in aliases functions builtins
        set -l names
        switch $kind
            case aliases
                set names (abbr --list)
            case functions
                set names (functions --names)
            case builtins
                set names (builtin --names)
        end
        # 列表为空时 string 会改读标准输入，先判断。
        set -l kept
        if set -q names[1]
            set kept (string match -r -- '^[[:alnum:]][[:alnum:]_.:+@,=-]*$' $names)
        end
        set -l joined (string replace -a ' ' %20 -- "$kept")
        set -l var __runode_names_$kind
        if test "$joined" != "$$var"
            set -g $var $joined
            printf '\e]6973;%s=%s\a' $kind $joined
        end
    end
end

# $argv[1] 是用户输入的命令原文，按百分号编码发出，分号、换行和控制字符都不会打断转义序列。
function __runode_preexec --on-event fish_preexec
    if test -n "$argv[1]"
        printf '\e]133;C;cmdline_url=%s\a' (string escape --style=url -- $argv[1])
    else
        printf '\e]133;C\a'
    end
end

function __runode_postexec --on-event fish_postexec
    printf '\e]133;D;%s\a' $status
end
