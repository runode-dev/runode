# runode 的 elvish 集成，照 Ghostty 的 elvish 集成（MIT）的路子。用 OSC 133 标出提示符、用户输入和
# 命令输出的边界：
#
#   133;A、133;B  提示符开始、用户输入开始，两个都在 elvish 画提示符之前发
#   133;C  命令开始执行     133;D  命令执行完（带退出码）
#   6973;<口令>;cwd=…  runode 私有：shell 的当前目录（百分号编码），每次显示提示符前都发
#   6973;<口令>;path=…  runode 私有：shell 的 PATH（百分号编码），变了才发，界面找命令行工具时用
#   6973;<口令>;functions=runode-reload  runode 私有：加载时发一次，runode 据此知道集成装好了
#   6973;<口令>;command=…  runode 私有：紧接在 133;C 前面，值是用户输入的命令原文（百分号编码）；
#                runode 只把这样认过的命令当作真的跑过，报告给等它的前端
#   6973;<口令>;end=  runode 私有：紧接在 133;D 前面，说明这次命令结束是真的；runode 只认这样的
#                133;D，命令输出里伪造的、别家集成另发的都不算
#   6973;<口令>;version=2  runode 私有：集成脚本的版本，每条命令开始时和 command 一起发，2 起
#                133;D 前面有 end；升级后接手的 runode 据此知道这个 shell 会发 end
#
# elvish 把提示符函数输出里的控制字符转义成可见的文字，提示符是在后台算好、晚一点才画的，脚本里
# 没有办法在提示符画完之后发 133;B（见 https://github.com/elves/elvish/pull/1917）。所以 133;B 和
# 133;A 一起在提示符之前发，提示符的文字被终端当成了用户输入。
#
# runode 还经 RUNODE_SHELL_FEATURES 告诉集成脚本另外开哪些功能（逗号隔开，和 Ghostty 的
# GHOSTTY_SHELL_FEATURES 一个写法）：
#
#   cursor:blink、cursor:steady  提示符上把光标换成竖线（闪或不闪），跑命令前用 CSI 0 SP q 换回
#                配置的样式
#
# 口令是 runode 启动这个 shell 时随机生成、经环境变量 RUNODE_REPORT_TOKEN 给的。runode 只认带着这个
# 口令的 6973 报告，屏幕上的别的输出伪造不了。没有口令时不发 6973 报告。
#
# elvish 不会自动加载别的脚本，runode 用 `-rc` 让它读这个文件，这里先照 elvish 自己的规矩加载用户的
# rc.elv，定义的变量、函数和模块照样留在交互环境里，再接上集成。runode-reload 用同一个 elvish 程序
# 和同一个 rc 换掉当前的 shell（exec），重新读用户配置。

use os
use re
use str
use runtime

# 口令和功能最先读进变量，再从环境里删掉：加载用户 rc.elv 时启动的程序，以及这个 shell 里运行的
# 所有程序都继承不到它。集成那一块拷进自己的局部变量后，最后把这两个删掉。
var runode-token = $E:RUNODE_REPORT_TOKEN
var runode-features = [(str:split , $E:RUNODE_SHELL_FEATURES)]
unset-env RUNODE_REPORT_TOKEN
unset-env RUNODE_SHELL_FEATURES

{
  # elvish 0.21 先找旧位置 ~/.elvish/rc.elv，没有时用 $XDG_CONFIG_HOME（没设时是 ~/.config）下的
  # elvish/rc.elv。
  var rc = ~/.elvish/rc.elv
  if (not (os:is-regular $rc)) {
    var config = ~/.config
    if (!=s $E:XDG_CONFIG_HOME '') {
      set config = $E:XDG_CONFIG_HOME
    }
    set rc = $config/elvish/rc.elv
  }
  if (os:is-regular $rc) {
    try {
      # rc.elv 编译不过时 on-end 也会被调，拿到的是个空的 ns（repr 是 `<ns 0x0>`，和 $nil 比不出来）：
      # 对它 keys 会让 elvish 直接崩掉，try 接不住。
      eval (slurp < $rc) &on-end={|ns|
        if (!=s (repr $ns) '<ns 0x0>') {
          edit:add-vars (keys $ns | each {|name| put [$name $ns[$name]] } | make-map)
        }
      }
    } catch e {
      show $e
    }
  }
}

{
  var token = $runode-token
  var features = $runode-features
  var rc = (src)[name]

  # 百分号编码：字母、数字和 `-._~/` 原样，别的按 UTF-8 的每个字节写成 %XX，分号、换行和控制字符
  # 都不会打断转义序列。
  fn encode {|text|
    str:join '' [(str:to-utf8-bytes $text | each {|byte|
      var n = (num $byte)
      var char = (str:from-codepoints $n)
      if (and (< $n 128) (re:match '^[A-Za-z0-9._~/-]$' $char)) {
        put $char
      } else {
        put (printf '%%%02X' $n)
      }
    })]
  }

  fn report {|field value|
    if (!=s $token '') {
      print "\e]6973;"$token";"$field"="(encode $value)"\a"
    }
  }

  var cursor = ''
  if (has-value $features cursor:blink) {
    set cursor = 5
  } elif (has-value $features cursor:steady) {
    set cursor = 6
  }

  var reported-path = $nil
  var running = $false

  # 每次显示提示符前报告当前目录，PATH 和上次报告的不一样时也报告，再标出提示符。
  fn before-readline {
    report cwd $pwd
    var path = (str:join : $paths)
    if (not-eq $path $reported-path) {
      set reported-path = $path
      report path $path
    }
    if (!=s $cursor '') {
      print "\e["$cursor" q"
    }
    print "\e]133;A;cl=line\a\e]133;B\a"
  }

  # 空着回车不是命令，不标。
  fn after-readline {|line|
    if (==s (str:trim-space $line) '') {
      return
    }
    if (!=s $cursor '') {
      print "\e[0 q"
    }
    report version 2
    report command $line
    print "\e]133;C\a"
    set running = $true
  }

  # 出错时取 elvish 报告的退出码；内建命令失败没有退出码，算 1。
  fn after-command {|info|
    if (not $running) {
      return
    }
    set running = $false
    var status = 0
    var error = $info[error]
    if (not-eq $error $nil) {
      set status = 1
      if (and (has-key $error reason) (has-key $error[reason] exit-status)) {
        set status = $error[reason][exit-status]
      }
    }
    report end ''
    print "\e]133;D;"$status"\a"
  }

  set edit:before-readline = (conj $edit:before-readline $before-readline~)
  set edit:after-readline = (conj $edit:after-readline $after-readline~)
  set edit:after-command = (conj $edit:after-command $after-command~)

  # 换成一个新的 elvish，重新读用户配置，接上集成：口令和功能经环境变量交回去，runode 照样认得新
  # shell 的报告。换之前报告这条命令执行完了：exec 之后不会再有它的 133;D。
  edit:add-var runode-reload~ {
    if (!=s $token '') {
      set-env RUNODE_REPORT_TOKEN $token
    }
    set-env RUNODE_SHELL_FEATURES (str:join , $features)
    report end ''
    print "\e]133;D;0\a"
    exec $runtime:elvish-path -rc $rc
  }

  report functions runode-reload
}
del runode-token runode-features
