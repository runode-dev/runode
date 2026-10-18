# runode 的 nushell 集成，照 Ghostty 的 nushell 集成（MIT）的路子：nushell 自己就用 OSC 133 标出
# 提示符、右侧提示符、用户输入和命令输出的边界（$env.config.shell_integration.osc133），这里不再标，
# 只补上 runode 私有的报告：
#
#   6973;<口令>;cwd=…  shell 的当前目录（百分号编码），每次显示提示符前都发
#   6973;<口令>;path=…  shell 的 PATH（百分号编码），变了才发，界面找命令行工具时用
#   6973;<口令>;functions=runode-reload  加载时发一次，runode 据此知道集成装好了、能用 runode-reload
#   6973;<口令>;command=…  命令开始执行时、紧接在 nushell 的 133;C 前面发，值是命令原文（百分号
#                编码）；runode 只把这样认过的命令当作真的跑过，报告给等它的前端
#   6973;<口令>;end=  紧接在 133;D 前面，说明这次命令结束是真的；runode 只认这样的 133;D，命令输出里
#                伪造的、别家集成另发的都不算。nushell 自己的 133;D 紧跟在命令后面发，中间插不进
#                钩子，所以显示提示符前另发一遍前面带 end 的 133;D，nushell 自己的那个 runode 不认
#   6973;<口令>;version=2  集成脚本的版本，每条命令开始时和 command 一起发，2 起 133;D 前面有 end；
#                升级后接手的 runode 据此知道这个 shell 会发 end
#
# runode 还经 RUNODE_SHELL_FEATURES 告诉集成脚本另外开哪些功能（逗号隔开，和 Ghostty 的
# GHOSTTY_SHELL_FEATURES 一个写法）：
#
#   cursor:blink、cursor:steady  提示符上把光标换成竖线（闪或不闪），跑命令前用 CSI 0 SP q 换回
#                配置的样式；用户自己配了 $env.config.cursor_shape 时不管
#
# 口令是 runode 启动这个 shell 时随机生成、经 RUNODE_REPORT_TOKEN_FILE 指着的文件给的。runode 只认带着
# 这个口令的 6973 报告，屏幕上的别的输出伪造不了。
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

# 口令、功能和数据目录读进 $env.__runode 后从环境里删掉，口令文件读完也删掉。它的值是记录，nushell
# 不把它交给子进程，这个 shell 里运行的程序拿不到口令；runode-reload 再写一个文件交给新的 nushell。
# 口令不能直接放进环境变量：进程启动时的环境（ps -E）同一用户的程序都读得到，hide-env 也改不掉。
# 早先版本的 runode-reload 直接给 RUNODE_REPORT_TOKEN，升级后在旧 shell 里换 shell 时照样认。
$env.__runode = {
    token: (if ($env.RUNODE_REPORT_TOKEN_FILE? | default "") != "" {
        let file = $env.RUNODE_REPORT_TOKEN_FILE
        # 用外部命令：这个脚本在用户的 config.nu 之后加载，用户的 alias、def（比如 `alias open = ^open`）
        # 会盖住同名的内建命令。
        let token = try { ^cat $file | str trim } catch { "" }
        ^rm -f -- $file
        $token
    } else {
        $env.RUNODE_REPORT_TOKEN? | default ""
    })
    features: ($env.RUNODE_SHELL_FEATURES? | default "" | split row "," | where $it != "")
    data_dir: ($env.RUNODE_NU_DATA_DIR? | default "")
    path: null
    # 上一个提示符之后跑过命令，下一次显示提示符前要报告它执行完了。
    ran: false
    # 那条命令的退出码，见 pre_prompt。
    status: 0
}
hide-env -i RUNODE_REPORT_TOKEN RUNODE_REPORT_TOKEN_FILE RUNODE_SHELL_FEATURES RUNODE_NU_DATA_DIR

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

# 每次显示提示符前报告当前目录；PATH 和上次报告的不一样时也报告。退出码在最前面就取下来：这个脚本在
# 用户的 config.nu 之后加载，后面这个钩子跑时，排在前面的用户钩子可能已经改了 LAST_EXIT_CODE。
$env.config.hooks.pre_prompt = ($env.config.hooks.pre_prompt? | default [] | prepend {||
    $env.__runode.status = $env.LAST_EXIT_CODE
} | append {||
    if ($env.__runode?.ran? | default false) {
        $env.__runode.ran = false
        print -n $"\e]6973;($env.__runode.token);end=\a\e]133;D;($env.__runode.status)\a"
    }
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
        print -n $"\e]6973;($token);version=2\a\e]6973;($token);command=($command | url encode --all)\a"
        $env.__runode.ran = true
    }
})

# 换成一个新的 nushell，重新读用户配置，接上集成：数据目录照 runode 启动时那样加回 XDG_DATA_DIRS，
# 口令写进新的临时文件（mktemp 建的只有自己能读写），和功能一起经环境变量交回去，runode 照样认得新
# shell 的报告。换之前报告这条命令执行完了：exec 之后不会再有它的 133;D。
def --env runode-reload [] {
    let state = $env.__runode? | default {}
    if ($state.data_dir? | default "") != "" {
        # XDG_DATA_DIRS 没设时 nushell 用这两个默认目录，加了我们的之后要显式带上。
        let dirs = $env.XDG_DATA_DIRS? | default "/usr/local/share:/usr/share" | split row (char esep)
        $env.XDG_DATA_DIRS = [$state.data_dir] ++ $dirs | str join (char esep)
        $env.RUNODE_NU_DATA_DIR = $state.data_dir
    }
    if ($state.token? | default "") != "" {
        try {
            let file = mktemp
            $"($state.token)\n" | save -f $file
            $env.RUNODE_REPORT_TOKEN_FILE = $file
        }
    }
    $env.RUNODE_SHELL_FEATURES = $state.features? | default [] | str join ","
    if ($state.token? | default "") != "" {
        print -n $"\e]6973;($state.token);end=\a"
    }
    print -n "\e]133;D;0\a"
    exec $nu.current-exe -l
}
