# runode 的 nushell 集成，照 Ghostty 的 nushell 集成（MIT）的路子：nushell 自己就用 OSC 133 标出
# 提示符、右侧提示符、用户输入和命令输出的边界（$env.config.shell_integration.osc133），这里不再标，
# 只补上 runode 私有的报告：
#
#   6973;<口令>;cwd=…  shell 的当前目录（百分号编码），每次显示提示符前都发
#   6973;<口令>;path=…  shell 的 PATH（百分号编码），变了才发，界面找命令行工具时用
#   6973;<口令>;functions=runode-reload  加载时发一次，runode 据此知道集成装好了、能用 runode-reload
#   6973;<口令>;command=…  命令开始执行时、紧接在 nushell 的 133;C 前面发，值是命令原文（百分号
#                编码）；runode 只把这样认过的命令当作真的跑过，报告给等它的前端
#
# runode 还经 RUNODE_SHELL_FEATURES 告诉集成脚本另外开哪些功能（逗号隔开，和 Ghostty 的
# GHOSTTY_SHELL_FEATURES 一个写法）：
#
#   cursor:blink、cursor:steady  提示符上把光标换成竖线（闪或不闪），跑命令前用 CSI 0 SP q 换回
#                配置的样式；用户自己配了 $env.config.cursor_shape 时不管
#
# 口令是 runode 启动这个 shell 时随机生成、经环境变量 RUNODE_REPORT_TOKEN 给的。runode 只认带着这个
# 口令的 6973 报告，屏幕上的别的输出伪造不了。
#
# runode 把这个文件所在的数据目录加进 XDG_DATA_DIRS，nushell 读完用户的 config.nu 以后自动加载
# vendor/autoload 里的脚本。先把加进去的那一项去掉，免得在这个 shell 里启动的程序继承它。

if ($env.RUNODE_NU_DATA_DIR? | default "") != "" and 'XDG_DATA_DIRS' in $env {
    let dirs = $env.XDG_DATA_DIRS | split row (char esep) | where $it != $env.RUNODE_NU_DATA_DIR
    if ($dirs | is-empty) {
        hide-env XDG_DATA_DIRS
    } else {
        $env.XDG_DATA_DIRS = $dirs | str join (char esep)
    }
}

# 口令、功能和数据目录读进 $env.__runode 后从环境里删掉。它的值是记录，nushell 不把它交给子进程，
# 这个 shell 里运行的程序拿不到口令；runode-reload 再经环境变量交给新的 nushell。
$env.__runode = {
    token: ($env.RUNODE_REPORT_TOKEN? | default "")
    features: ($env.RUNODE_SHELL_FEATURES? | default "" | split row "," | where $it != "")
    data_dir: ($env.RUNODE_NU_DATA_DIR? | default "")
    path: null
}
hide-env -i RUNODE_REPORT_TOKEN RUNODE_SHELL_FEATURES RUNODE_NU_DATA_DIR

# runode 的提示符、输入和输出的边界全靠 nushell 自己的 OSC 133。
$env.config.shell_integration.osc133 = true

if $env.__runode.token != "" {
    print -n $"\e]6973;($env.__runode.token);functions=runode-reload\a"
}

# cursor 功能：提示符上换成竖线（5 闪，6 不闪），命令开始前换回配置的样式。用户配了哪种编辑模式的
# 光标形状，就由 nushell 自己管，这里不动。
def __runode_cursor []: nothing -> string {
    let features = $env.__runode?.features? | default []
    let own = $env.config.cursor_shape? | default {} | values | all {|shape| $shape == "inherit" }
    if not $own {
        ""
    } else if "cursor:blink" in $features {
        "5"
    } else if "cursor:steady" in $features {
        "6"
    } else {
        ""
    }
}

# 每次显示提示符前报告当前目录；PATH 和上次报告的不一样时也报告。
$env.config.hooks.pre_prompt = ($env.config.hooks.pre_prompt? | default [] | append {||
    let cursor = __runode_cursor
    if $cursor != "" {
        print -n $"\e[($cursor) q"
    }
    let token = $env.__runode?.token? | default ""
    if $token == "" {
        return
    }
    print -n $"\e]6973;($token);cwd=($env.PWD | url encode --all)\a"
    let path = [$env.PATH] | flatten | str join (char esep)
    if $path != $env.__runode.path {
        $env.__runode.path = $path
        print -n $"\e]6973;($token);path=($path | url encode --all)\a"
    }
})

# 命令开始前报告命令原文，紧接着就是 nushell 的 133;C。空着回车 nushell 也发 133;C，那不是命令，
# 不报告。
$env.config.hooks.pre_execution = ($env.config.hooks.pre_execution? | default [] | append {||
    let command = commandline
    if ($command | str trim) == "" {
        return
    }
    if (__runode_cursor) != "" {
        print -n "\e[0 q"
    }
    let token = $env.__runode?.token? | default ""
    if $token != "" {
        print -n $"\e]6973;($token);command=($command | url encode --all)\a"
    }
})

# 换成一个新的 nushell，重新读用户配置，接上集成：数据目录照 runode 启动时那样加回 XDG_DATA_DIRS，
# 口令和功能经环境变量交回去，runode 照样认得新 shell 的报告。换之前报告这条命令执行完了：exec 之后
# 不会再有它的 133;D。
def --env runode-reload [] {
    let state = $env.__runode? | default {}
    if ($state.data_dir? | default "") != "" {
        # XDG_DATA_DIRS 没设时 nushell 用这两个默认目录，加了我们的之后要显式带上。
        let dirs = $env.XDG_DATA_DIRS? | default "/usr/local/share:/usr/share" | split row (char esep)
        $env.XDG_DATA_DIRS = [$state.data_dir] ++ $dirs | str join (char esep)
        $env.RUNODE_NU_DATA_DIR = $state.data_dir
    }
    if ($state.token? | default "") != "" {
        $env.RUNODE_REPORT_TOKEN = $state.token
    }
    $env.RUNODE_SHELL_FEATURES = $state.features? | default [] | str join ","
    print -n "\e]133;D;0\a"
    exec $nu.current-exe -l
}
