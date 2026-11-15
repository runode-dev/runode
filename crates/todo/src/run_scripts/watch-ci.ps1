# runode 启动时会用自带的版本覆盖本文件；要改就复制到别处，再把定时的先跑命令指向副本。
#
# 定时的先跑命令：查一次分支上的 GitHub Actions，某个 workflow 最新一次有结论的运行失败了、还没派过时，把失败
# 日志的末尾写到标准输出，runode 据此开任务；没事时标准输出为空。派过的 workflow 记在状态目录的 pending 里，
# 它在这个分支上重新通过之后再失败才会再派。日志一律写标准错误。
# 用法：watch-ci.ps1 [分支]，不给时用仓库的默认分支。
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$OutputEncoding = [Text.UTF8Encoding]::new($false)

# 一次最多派几个 workflow，没轮到的下次再派。三个数一起管着标准输出的大小，要在宿主收得下的提示词里留出定时
# 自己的提示词。
$MaxRuns = 3
# 每个 workflow 留失败日志的最后几行。
$LogLines = 40
# 日志每行最多留几个字节（UTF-8）。
$LineBytes = 300

$Utf8 = [Text.UTF8Encoding]::new($false)
$Schedule = if ($env:RUNODE_SCHEDULE) { $env:RUNODE_SCHEDULE } else { 'manual' }
$StateDir = Join-Path (Join-Path $PSScriptRoot 'state') $Schedule
$PendingFile = Join-Path $StateDir 'pending'

function Write-Log([string]$Message) { [Console]::Error.WriteLine("watch-ci: $Message") }

# 跑 gh，按行返回标准输出，标准错误照转。PowerShell 5.1 在自己的标准错误被重定向时把原生程序写的标准错误包成
# 错误记录，Stop 下会中止，所以这里临时用 Continue；退出码看 $LASTEXITCODE。
function Invoke-GhLines([string[]]$GhArgs) {
    $ErrorActionPreference = 'Continue'
    $lines = @(& gh @GhArgs 2>&1 | ForEach-Object {
            if ($_ -is [Management.Automation.ErrorRecord]) { [Console]::Error.WriteLine($_.ToString()) } else { [string]$_ }
        })
    return , $lines
}

# 跑 gh，失败就退出。
function Invoke-Gh([string[]]$GhArgs) {
    $lines = Invoke-GhLines $GhArgs
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

# 截到 UTF-8 下不超过 $Max 个字节。
function Limit-Bytes([string]$Text, [int]$Max) {
    if ($Text.Length -gt $Max) { $Text = $Text.Substring(0, $Max) }
    while ($Utf8.GetByteCount($Text) -gt $Max) { $Text = $Text.Substring(0, $Text.Length - 1) }
    return $Text
}

# --log-failed 每行是 `job<TAB>step<TAB>时间戳 内容`（时间戳前可能有 BOM），只留内容的最后几行。
function Get-LogTail([string]$Id) {
    $lines = Invoke-GhLines 'run', 'view', $Id, '--log-failed'
    if ($LASTEXITCODE -ne 0) {
        Write-Log "cannot fetch the failed log of run $Id"
        return "(gh run view $Id --log-failed failed; look at the run on GitHub)"
    }
    $clean = foreach ($line in $lines) {
        $text = $line -replace '^[^\t]*\t[^\t]*\t', '' -replace '^﻿', '' -replace '^\d{4}-\d\d-\d\dT[\d:.]+Z ', ''
        Limit-Bytes $text $LineBytes
    }
    return (@($clean | Select-Object -Last $LogLines) -join "`n")
}

if (-not (Get-Command gh -CommandType Application -ErrorAction SilentlyContinue)) { Write-Log 'cannot find gh'; exit 1 }
New-Item -ItemType Directory -Force -Path $StateDir | Out-Null

$Branch = if ($args.Count -gt 0) { [string]$args[0] } else { '' }
if (-not $Branch) {
    $out = Invoke-Gh 'repo', 'view', '--json', 'defaultBranchRef', '--jq', '.defaultBranchRef.name'
    if ($out.Count -gt 0) { $Branch = $out[0].Trim() }
    if (-not $Branch) { Write-Log 'cannot tell the default branch'; exit 1 }
}

$runs = Invoke-Gh 'run', 'list', '--branch', $Branch, '--status', 'completed', '--limit', '60', `
    '--json', 'databaseId,workflowName,headSha,conclusion', `
    '--jq', '.[] | [.databaseId, .workflowName, .headSha, .conclusion] | @tsv'

# gh 按新到旧给，每个 workflow 只看最新一次有结论的运行（cancelled、skipped 不算结论）。最新一次通过的移出
# pending；失败了、不在 pending 里的派，一次最多 $MaxRuns 个。
$pending = Read-State $PendingFile
$latest = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
$send = [Collections.Generic.List[object]]::new()
foreach ($line in $runs) {
    $fields = $line -split "`t"
    if ($fields.Count -lt 4) { continue }
    $id, $workflow, $sha, $conclusion = $fields[0], $fields[1], $fields[2], $fields[3]
    if ($conclusion -ceq 'cancelled' -or $conclusion -ceq 'skipped' -or -not $latest.Add($workflow)) { continue }
    if ($conclusion -ceq 'success' -or $conclusion -ceq 'neutral') {
        [void]$pending.Remove($workflow)
    } elseif (($conclusion -ceq 'failure' -or $conclusion -ceq 'timed_out' -or $conclusion -ceq 'startup_failure') `
            -and -not $pending.Contains($workflow) -and $send.Count -lt $MaxRuns) {
        $send.Add([pscustomobject]@{ Id = $id; Workflow = $workflow; Sha = $sha; Conclusion = $conclusion })
    }
}

$sections = foreach ($run in $send) {
    $short = if ($run.Sha.Length -gt 9) { $run.Sha.Substring(0, 9) } else { $run.Sha }
    "## $($run.Workflow) (run $($run.Id), commit $short, $($run.Conclusion))`n$(Get-LogTail $run.Id)`n"
}

$names = @($send | ForEach-Object { $_.Workflow })
Write-State $PendingFile (@($pending) + $names)

if ($send.Count -eq 0) {
    Write-Log "nothing new failed on $Branch"
    exit 0
}
Write-Log "dispatching: $($names -join ',')"
$header = "These GitHub Actions workflows failed on $Branch. The end of each failed log follows (gh run view <id> --log-failed prints the whole log).`n"
[Console]::Out.Write($header + (-join @($sections | ForEach-Object { "`n$_" })))
exit 0
