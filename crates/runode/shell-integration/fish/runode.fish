# runode 的 fish 集成：用 OSC 133 标出提示符、用户输入和命令输出的边界。
#
#   133;A  提示符开始       133;B  提示符结束、用户输入开始
#   133;C  命令开始执行     133;D  命令执行完（带退出码）
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

# 用户的 fish_prompt 在这个脚本之后才定义，第一次显示提示符前再把它包起来，
# 在前后加上提示符的开始和结束标记。
function __runode_wrap_prompt --on-event fish_prompt
    functions -e __runode_wrap_prompt
    functions -q fish_prompt; or return
    functions -c fish_prompt __runode_original_prompt
    function fish_prompt
        printf '\e]133;A;cl=line\a'
        __runode_original_prompt
        printf '\e]133;B\a'
    end
end

function __runode_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end

function __runode_postexec --on-event fish_postexec
    printf '\e]133;D;%s\a' $status
end
