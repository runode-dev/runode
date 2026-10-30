# runode 的 PowerShell 集成，PowerShell 7（pwsh）和系统自带的 Windows PowerShell 5.1 都用它。runode 用
# `-NoExit -Command` 让 PowerShell 读完用户的 profile 以后读出本文件交给 Invoke-Expression：执行策略只管
# 脚本文件，管不着 Invoke-Expression，默认不让跑脚本的 Windows 上也加载得了。
#
# 包一层用户的 prompt 函数，用 OSC 133 标出提示符、用户输入和命令输出的边界：
#
#   133;A  提示符开始       133;B  提示符结束、用户输入开始
#   133;C  命令开始执行     133;D  命令执行完（带退出码）
#   6973;<口令>;cwd=…  runode 私有：shell 的当前目录（百分号编码），每次显示提示符前都发；当前位置不在
#                文件系统上（比如 HKLM:、Env:）时不发
#   6973;<口令>;path=…  runode 私有：shell 的 PATH（百分号编码），变了才发，界面找命令行工具时用
#   6973;<口令>;functions=…  runode 私有：shell 里定义的函数名，名字之间用 %20 隔开，变了才发，
#                看集成脚本装好了没有
#   6973;<口令>;command=…  runode 私有：紧接在 133;C 前面，说明这次命令开始是真的，值是用户输入的
#                命令原文（百分号编码），太长时为空，终端从屏幕上读；runode 只把这样认过的命令当作
#                真的跑过，报告给等它的前端
#   6973;<口令>;end=  runode 私有：紧接在 133;D 前面，说明这次命令结束是真的；runode 只认这样的
#                133;D，命令输出里伪造的、别家集成另发的都不算
#   6973;<口令>;version=2  runode 私有：集成脚本的版本，每条命令开始时和 command 一起发，2 起
#                133;D 前面有 end
#
# 命令开始要接在 PSReadLine 读一行的函数（PSConsoleHostReadLine）上，没装 PSReadLine 时没有命令开始的
# 标记，也不换光标。退出码：命令成功时是 0；失败时，命令里出了 PowerShell 自己的错误（$Error 多了一条）
# 是 1，否则是外部程序的退出码（$LASTEXITCODE），没有时也是 1。
#
# runode 还经 RUNODE_SHELL_FEATURES 告诉集成脚本另外开哪些功能（逗号隔开，和 Ghostty 的
# GHOSTTY_SHELL_FEATURES 一个写法）：
#
#   cursor:blink、cursor:steady  提示符上把光标换成竖线（闪或不闪），跑命令前用 CSI 0 SP q 换回
#                配置的样式
#
# 口令是 runode 启动这个 shell 时随机生成、经 RUNODE_REPORT_TOKEN_FILE 指着的文件给的。runode 只认
# 带着这个口令的 6973 报告，屏幕上的别的输出伪造不了。口令读进全局变量，不进环境变量，这个 shell
# 里运行的程序拿不到。没有 runode-reload：PowerShell 不能拿新的进程换掉自己。
#
# 集成的函数都先关掉严格模式：用户的 profile 开了 Set-StrictMode 时，读还没赋值的变量会出错。

if (-not (Test-Path Variable:Global:__runode)) {
    $Global:__runode = @{
        token = ''
        # 提示符上换成竖线的序列，为空时不管光标。
        cursor = ''
        # 已经显示过一次提示符：之后每次显示提示符都意味着上一条命令执行完了。
        prompted = $false
        # 上一次显示提示符时最新的那条错误，据此看命令里有没有出新的错误。
        error = $null
        # 上一次报告给 runode 的 PATH 和函数名。
        path = $null
        names = $null
        # 用户原来的 prompt 和 PSConsoleHostReadLine。
        prompt = $function:prompt
        readline = $null
    }
    if ($env:RUNODE_REPORT_TOKEN_FILE) {
        try { $Global:__runode.token = [IO.File]::ReadAllText($env:RUNODE_REPORT_TOKEN_FILE).Trim() } catch {}
        try { [IO.File]::Delete($env:RUNODE_REPORT_TOKEN_FILE) } catch {}
    }
    $Global:__runode.features = @("$env:RUNODE_SHELL_FEATURES".Split(',') | Where-Object { $_ })
    Remove-Item Env:RUNODE_REPORT_TOKEN_FILE, Env:RUNODE_SHELL_FEATURES -ErrorAction Ignore

    if (Test-Path Function:PSConsoleHostReadLine) {
        $Global:__runode.readline = $function:PSConsoleHostReadLine
        if ($Global:__runode.features -contains 'cursor:blink') {
            $Global:__runode.cursor = "$([char]27)[5 q"
        } elseif ($Global:__runode.features -like 'cursor:*') {
            $Global:__runode.cursor = "$([char]27)[6 q"
        }

        # PSReadLine 读完一行、换了行以后发命令开始。空着回车不算命令，不发。
        function Global:PSConsoleHostReadLine {
            Set-StrictMode -Off
            $line = & $Global:__runode.readline
            if (-not [string]::IsNullOrWhiteSpace($line)) {
                $state = $Global:__runode
                $e = [char]27
                $a = [char]7
                $out = ''
                if ($state.cursor) {
                    $out += "$e[0 q"
                }
                if ($state.token) {
                    $command = ''
                    if ($line.Length -le 4096) {
                        $command = [Uri]::EscapeDataString($line)
                    }
                    $out += "$e]6973;$($state.token);version=2$a$e]6973;$($state.token);command=$command$a"
                }
                [Console]::Write("$out$e]133;C$a")
            }
            $line
        }
    }

    # 把 $text 按 UTF-8 做百分号编码：分号、换行、ESC、BEL 都不会打断转义序列。.NET Framework 的
    # EscapeDataString 不收太长的字符串，这时报告为空。
    function Global:__runode_encode([string]$text) {
        try { [Uri]::EscapeDataString($text) } catch { '' }
    }

    function Global:prompt {
        # 最先取下上一条命令的结果，后面的语句会改掉它们。还没跑过外部程序时没有 LASTEXITCODE，
        # 关掉严格模式以后才读得了。
        $ok = $global:?
        Set-StrictMode -Off
        $code = $Global:LASTEXITCODE
        $state = $Global:__runode
        $e = [char]27
        $a = [char]7
        $out = ''
        if ($state.prompted) {
            $exit = 0
            if (-not $ok) {
                $exit = 1
                $failed = $Error.Count -and -not [object]::ReferenceEquals($Error[0], $state.error)
                if (-not $failed -and $code -is [int] -and $code -ne 0) {
                    $exit = $code
                }
            }
            if ($state.token) {
                $out += "$e]6973;$($state.token);end=$a"
            }
            $out += "$e]133;D;$exit$a"
        }
        $state.prompted = $true
        if ($state.token) {
            $token = $state.token
            if ($PWD.Provider.Name -eq 'FileSystem') {
                $out += "$e]6973;$token;cwd=$(__runode_encode $PWD.ProviderPath)$a"
            }
            if ($env:PATH -cne $state.path) {
                $state.path = $env:PATH
                $out += "$e]6973;$token;path=$(__runode_encode $env:PATH)$a"
            }
            $names = @(Get-ChildItem Function: | ForEach-Object Name |
                Where-Object { $_ -cmatch '^[A-Za-z0-9][A-Za-z0-9_.:+@,=-]*$' }) -join '%20'
            if ($names -cne $state.names) {
                $state.names = $names
                $out += "$e]6973;$token;functions=$names$a"
            }
        }
        # 标记先写出去再跑用户的 prompt：它可能用 Write-Host 直接往屏幕上写，不在返回的字符串里。
        [Console]::Write("$out$e]133;A;cl=line$a")
        # 用户的 prompt 可能看 $? 显示上一条命令成没成，先照上一条命令把 $? 弄回失败。
        if (-not $ok) {
            Write-Error '' -ErrorAction Ignore
        }
        $text = -join @(& $state.prompt)
        $state.error = $null
        if ($Error.Count) {
            $state.error = $Error[0]
        }
        # 用户的 prompt 可能跑了外部程序（比如 git），换回上一条命令的退出码。
        $Global:LASTEXITCODE = $code
        "$text$($state.cursor)$e]133;B$a"
    }
}
