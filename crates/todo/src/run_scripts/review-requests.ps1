# runode 启动时会用自带的版本覆盖本文件；要改就复制到别处，再把定时的先跑命令指向副本。
#
# 定时的先跑命令：找请你评审、还没派过的拉取请求（不算草稿），挑编号最小的一个，把它的说明和改到的文件写到
# 标准输出，runode 据此开任务；没有时标准输出为空。派过的记在状态目录的 seen 里（编号和 head 的 commit），
# 推了新 commit，或者离开列表后又请求你评审时会再派。日志一律写标准错误。
# 用法：review-requests.ps1
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$OutputEncoding = [Text.UTF8Encoding]::new($false)

$Utf8 = [Text.UTF8Encoding]::new($false)
$Schedule = if ($env:RUNODE_SCHEDULE) { $env:RUNODE_SCHEDULE } else { 'manual' }
$StateDir = Join-Path (Join-Path $PSScriptRoot 'state') $Schedule
$SeenFile = Join-Path $StateDir 'seen'

function Write-Log([string]$Message) { [Console]::Error.WriteLine("review-requests: $Message") }

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

if (-not (Get-Command gh -CommandType Application -ErrorAction SilentlyContinue)) { Write-Log 'cannot find gh'; exit 1 }
New-Item -ItemType Directory -Force -Path $StateDir | Out-Null

$list = Invoke-Gh 'pr', 'list', '--state', 'open', '--search', 'draft:false review-requested:@me', '--limit', '100', `
    '--json', 'number,headRefOid', '--jq', '.[] | [.number, .headRefOid] | @tsv'

# 编号和 head 都记在 seen 里的是派过的，seen 只留这些；其余的按编号从小到大等着派。
$seen = Read-State $SeenFile
$kept = [Collections.Generic.List[string]]::new()
$waiting = [Collections.Generic.List[object]]::new()
foreach ($line in $list) {
    $fields = $line -split "`t"
    if ($fields.Count -lt 2 -or -not $fields[0]) { continue }
    $key = "$($fields[0]) $($fields[1])"
    if ($seen.Contains($key)) {
        $kept.Add($key)
    } else {
        $waiting.Add([pscustomobject]@{ Number = [long]$fields[0]; Head = $fields[1] })
    }
}
$pick = $waiting | Sort-Object Number | Select-Object -First 1

if (-not $pick) {
    Write-State $SeenFile $kept
    Write-Log 'no new review requests'
    exit 0
}

$number = [string]$pick.Number
$view = (Invoke-Gh 'pr', 'view', $number) -join "`n"
$files = (Invoke-Gh 'pr', 'diff', $number, '--name-only') -join "`n"
$view = Limit-Text $view $MaxText "gh pr view $number shows all of it"
$files = Limit-Text $files $MaxText "gh pr diff $number --name-only lists all of them"

Write-State $SeenFile (@($kept) + "$number $($pick.Head)")
Write-Log "dispatching pull request #$number"
[Console]::Out.Write("Pull request #$number (head $($pick.Head))`n`n$view`n`nFiles changed:`n$files`n")
exit 0
