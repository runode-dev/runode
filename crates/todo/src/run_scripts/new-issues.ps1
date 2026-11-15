# runode 启动时会用自带的版本覆盖本文件；要改就复制到别处，再把定时的先跑命令指向副本。
#
# 定时的先跑命令：找带某个标签、还开着、没派过的 issue，挑编号最小的一个，把它的内容和评论写到标准输出，
# runode 据此开任务；没有时标准输出为空。派过的编号记在状态目录的 seen 里，离开列表（关掉、摘了标签）的从
# seen 里清掉。日志一律写标准错误。
# 用法：new-issues.ps1 <标签>
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$OutputEncoding = [Text.UTF8Encoding]::new($false)

$Utf8 = [Text.UTF8Encoding]::new($false)
$Schedule = if ($env:RUNODE_SCHEDULE) { $env:RUNODE_SCHEDULE } else { 'manual' }
$StateDir = Join-Path (Join-Path $PSScriptRoot 'state') $Schedule
$SeenFile = Join-Path $StateDir 'seen'

function Write-Log([string]$Message) { [Console]::Error.WriteLine("new-issues: $Message") }

# 正文、评论和文件清单各自最多留几个字节，一起管着标准输出的大小，要在宿主收得下的提示词里留出定时自己的
# 提示词。截掉的部分由 agent 自己用 gh 再读。
$MaxText = 16000

# 超过 $Max 个 UTF-8 字节时只留开头（不切开字符），再接一行说明。
function Limit-Text([string]$Text, [int]$Max, [string]$Note) {
    $bytes = $Utf8.GetBytes($Text)
    if ($bytes.Length -le $Max) { return $Text }
    $cut = $Max
    while ($cut -gt 0 -and ($bytes[$cut] -band 0xC0) -eq 0x80) { $cut-- }
    return $Utf8.GetString($bytes, 0, $cut) + "`n[cut here; $Note]"
}

# 跑 gh，按行返回标准输出，标准错误照转，失败就退出。PowerShell 5.1 在自己的标准错误被重定向时把原生程序写的
# 标准错误包成错误记录，Stop 下会中止，所以这里临时用 Continue。
function Invoke-Gh([string[]]$GhArgs) {
    $ErrorActionPreference = 'Continue'
    $lines = @(& gh @GhArgs 2>&1 | ForEach-Object {
            if ($_ -is [Management.Automation.ErrorRecord]) { [Console]::Error.WriteLine($_.ToString()) } else { [string]$_ }
        })
    if ($LASTEXITCODE -ne 0) { Write-Log "gh $($GhArgs[0]) $($GhArgs[1]) failed"; exit 1 }
    return , $lines
}

function Read-State([string]$Path) {
    $lines = [Collections.Generic.List[string]]::new()
    if (Test-Path -LiteralPath $Path) {
        foreach ($line in [IO.File]::ReadAllLines($Path, $Utf8)) { if ($line) { $lines.Add($line) } }
    }
    return , $lines
}

function Write-State([string]$Path, [string[]]$Lines) {
    $text = -join @($Lines | ForEach-Object { "$_`n" })
    [IO.File]::WriteAllText("$Path.tmp", $text, $Utf8)
    Move-Item -LiteralPath "$Path.tmp" -Destination $Path -Force
}

$Label = if ($args.Count -gt 0) { [string]$args[0] } else { '' }
if (-not $Label) {
    Write-Log 'usage: new-issues.ps1 <label>'
    exit 2
}

if (-not (Get-Command gh -CommandType Application -ErrorAction SilentlyContinue)) { Write-Log 'cannot find gh'; exit 1 }
New-Item -ItemType Directory -Force -Path $StateDir | Out-Null

$list = Invoke-Gh 'issue', 'list', '--state', 'open', '--label', $Label, '--limit', '100', '--json', 'number', '--jq', '.[].number'

# 记在 seen 里的是派过的，seen 只留还在列表里的；其余的按编号从小到大等着派。
$seen = Read-State $SeenFile
$kept = [Collections.Generic.List[string]]::new()
$waiting = [Collections.Generic.List[long]]::new()
foreach ($line in $list) {
    $item = $line.Trim()
    if (-not $item) { continue }
    if ($seen.Contains($item)) { $kept.Add($item) } else { $waiting.Add([long]$item) }
}

if ($waiting.Count -eq 0) {
    Write-State $SeenFile $kept
    Write-Log "no new issues labeled $Label"
    exit 0
}

$number = [string]($waiting | Sort-Object | Select-Object -First 1)
# 不接终端时 --comments 只打印评论，正文要另取一次。
$view = (Invoke-Gh 'issue', 'view', $number) -join "`n"
$comments = (Invoke-Gh 'issue', 'view', $number, '--comments') -join "`n"
$view = Limit-Text $view $MaxText "gh issue view $number shows all of it"
$comments = Limit-Text $comments $MaxText "gh issue view $number --comments shows all of them"

Write-State $SeenFile (@($kept) + $number)
Write-Log "dispatching issue #$number"
[Console]::Out.Write("Issue #$number`n`n$view`n`nComments:`n$comments`n")
exit 0
