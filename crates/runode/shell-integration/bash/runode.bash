# runode 用 `bash --rcfile 本文件` 启动交互式 bash。--rcfile 对登录 shell 不起作用，
# 所以这里照登录 shell 的顺序自己加载用户配置，再接上集成：用 OSC 133 标出提示符、
# 用户输入和命令输出的边界。
#
#   133;A  提示符开始       133;B  提示符结束、用户输入开始
#   133;C  命令开始执行     133;D  命令执行完（带退出码）

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

    _runode_prompt_command() {
        if [ -n "$_runode_prompted" ]; then
            printf '\033]133;D;%s\007' "$_runode_last_status"
        fi
        _runode_prompted=1
        if [ "$PS1" != "$_runode_ps1" ]; then
            _runode_ps1='\[\033]133;A;cl=line\007\]'"$PS1"'\[\033]133;B\007\]'
            PS1=$_runode_ps1
        fi
        if [ "$PS2" != "$_runode_ps2" ]; then
            _runode_ps2='\[\033]133;A;k=s\007\]'"$PS2"'\[\033]133;B\007\]'
            PS2=$_runode_ps2
        fi
    }

    # 先记下退出码，再跑用户原有的 PROMPT_COMMAND，最后加标记。
    PROMPT_COMMAND="_runode_last_status=\$?;${PROMPT_COMMAND:+$PROMPT_COMMAND;}_runode_prompt_command"
    # 4.4 起才有 PS0：命令开始执行前显示一次。更老的 bash 没有命令开始的标记。
    if [ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 4 ]; }; then
        PS0='\[\033]133;C\007\]'"${PS0-}"
    fi
fi
