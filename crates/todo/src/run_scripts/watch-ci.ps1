# runode 启动时会用自带的版本覆盖本文件；要改就复制到别处，再把定时的先跑命令指向副本。
#
# 定时的先跑命令：查一次分支上的 GitHub Actions，某个 workflow 最新一次有结论的运行失败了、还没派过时，把失败
# 日志的末尾写到标准输出，runode 据此开任务；没事时标准输出为空。派过的 workflow 记在状态目录的 pending 里，
# 它在这个分支上重新通过之后再失败才会再派。workflow 在这个分支上最新的一次还在排队或在跑时不看更早的运行：
# 新的提交可能已经修好；但这次运行里已经有 job 失败了就不等它跑完，按失败的 job 派。日志一律写标准错误。
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

# 读 `job<TAB>step<TAB>时间戳 内容` 格式的日志行（时间戳前可能有 BOM），只留内容的最后几行。每个 job 从
# `Post job cleanup.` 起是 runner 收尾的输出，和 `##[start-action`、`##[end-action` 标记一样丢掉，末尾留下的才是
# 报错。
function Format-LogTail([string[]]$Lines) {
    $cleanup = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $clean = foreach ($line in $lines) {
        $job = ($line -split "`t", 2)[0]
        $text = $line -replace '^[^\t]*\t[^\t]*\t', '' -replace '^﻿', '' -replace '^\d{4}-\d\d-\d\dT[\d:.]+Z ', ''
        if ($text -ceq 'Post job cleanup.') { [void]$cleanup.Add($job) }
        if ($cleanup.Contains($job) -or $text.StartsWith('##[start-action', [StringComparison]::Ordinal) `
                -or $text.StartsWith('##[end-action', [StringComparison]::Ordinal)) { continue }
        Limit-Bytes $text $LineBytes
    }
    return (@($clean | Select-Object -Last $LogLines) -join "`n")
}

# 一个 job 的原始日志行，取不到时返回 $null。运行没跑完时 `gh run view` 不给日志，只能经 API 取单个 job 的；新版
# gh 遇到日志里的终端转义序列要 --allow-escape-sequences 才肯输出，旧版不认这个选项。
function Get-JobLog([string]$JobId) {
    $path = "repos/{owner}/{repo}/actions/jobs/$JobId/logs"
    $lines = Invoke-GhLines 'api', $path, '--allow-escape-sequences'
    if ($LASTEXITCODE -ne 0) { $lines = Invoke-GhLines 'api', $path }
    if ($LASTEXITCODE -ne 0) { return $null }
    return , $lines
}

if (-not (Get-Command gh -CommandType Application -ErrorAction SilentlyContinue)) { Write-Log 'cannot find gh'; exit 1 }
New-Item -ItemType Directory -Force -Path $StateDir | Out-Null

$Branch = if ($args.Count -gt 0) { [string]$args[0] } else { '' }
if (-not $Branch) {
    $out = Invoke-Gh 'repo', 'view', '--json', 'defaultBranchRef', '--jq', '.defaultBranchRef.name'
    if ($out.Count -gt 0) { $Branch = $out[0].Trim() }
    if (-not $Branch) { Write-Log 'cannot tell the default branch'; exit 1 }
}

$runs = Invoke-Gh 'run', 'list', '--branch', $Branch, '--limit', '60', `
    '--json', 'databaseId,workflowName,headSha,status,conclusion', `
    '--jq', '.[] | [.databaseId, .workflowName, .headSha, .status, .conclusion] | @tsv'

# gh 按新到旧给，每个 workflow 只看最新一次有结论的运行（cancelled、skipped、action_required 不算结论）；在那之前
# 碰到还没跑完的就不看更早的运行，它不在 pending 里时查它已经失败的 job，等人批准的（waiting）跳过，它不知道要
# 等多久。最新一次通过的移出 pending；失败了、不在 pending 里的派，一次最多 $MaxRuns 个。
$pending = Read-State $PendingFile
$latest = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
$candidates = [Collections.Generic.List[object]]::new()
foreach ($line in $runs) {
    $fields = $line -split "`t"
    if ($fields.Count -lt 5) { continue }
    $id, $workflow, $sha, $status, $conclusion = $fields[0], $fields[1], $fields[2], $fields[3], $fields[4]
    if ($latest.Contains($workflow)) { continue }
    if ($status -ceq 'waiting') { continue }
    if ($status -cne 'completed') {
        [void]$latest.Add($workflow)
        if (-not $pending.Contains($workflow)) {
            $candidates.Add([pscustomobject]@{ Id = $id; Workflow = $workflow; Sha = $sha; Conclusion = $null })
        }
        continue
    }
    if ($conclusion -ceq 'cancelled' -or $conclusion -ceq 'skipped' -or $conclusion -ceq 'action_required') { continue }
    [void]$latest.Add($workflow)
    if ($conclusion -ceq 'success' -or $conclusion -ceq 'neutral') {
        [void]$pending.Remove($workflow)
    } elseif (($conclusion -ceq 'failure' -or $conclusion -ceq 'timed_out' -or $conclusion -ceq 'startup_failure') `
            -and -not $pending.Contains($workflow)) {
        $candidates.Add([pscustomobject]@{ Id = $id; Workflow = $workflow; Sha = $sha; Conclusion = $conclusion })
    }
}

$names = [Collections.Generic.List[string]]::new()
$running = $false
$sections = foreach ($run in $candidates) {
    if ($names.Count -ge $MaxRuns) { break }
    if ($run.Conclusion) {
        $what = $run.Conclusion
        $lines = Invoke-GhLines 'run', 'view', $run.Id, '--log-failed'
        if ($LASTEXITCODE -eq 0) {
            $tail = Format-LogTail $lines
        } else {
            Write-Log "cannot fetch the failed log of run $($run.Id)"
            $tail = "(gh run view $($run.Id) --log-failed failed; look at the run on GitHub)"
        }
    } else {
        $jobs = Invoke-GhLines 'run', 'view', $run.Id, '--json', 'jobs', '--jq', '.jobs[] | [.databaseId, .name, .conclusion] | @tsv'
        if ($LASTEXITCODE -ne 0) { Write-Log "cannot list the jobs of run $($run.Id)"; continue }
        $failed = @(foreach ($job in $jobs) {
                $parts = $job -split "`t"
                if ($parts.Count -ge 3 -and ($parts[2] -ceq 'failure' -or $parts[2] -ceq 'timed_out')) { , $parts }
            })
        if ($failed.Count -eq 0) { continue }
        # API 给的日志行没有 `job<TAB>step<TAB>` 前缀，补上才能和 --log-failed 的一样处理。
        $lines = foreach ($parts in $failed) {
            $log = Get-JobLog $parts[0]
            if ($null -eq $log) { Write-Log "cannot fetch the log of job $($parts[0])"; continue }
            foreach ($line in $log) { "$($parts[1])`t-`t$line" }
        }
        $what = "failed in $(@($failed | ForEach-Object { "$($_[1]) (job $($_[0]))" }) -join ', ') while the run is still going"
        $tail = Format-LogTail @($lines)
        if (-not $tail) { $tail = '(cannot fetch the job logs; look at the run on GitHub)' }
        $running = $true
    }
    $names.Add($run.Workflow)
    $short = if ($run.Sha.Length -gt 9) { $run.Sha.Substring(0, 9) } else { $run.Sha }
    "## $($run.Workflow) (run $($run.Id), commit $short, $what)`n$tail`n"
}

Write-State $PendingFile (@($pending) + $names)

if ($names.Count -eq 0) {
    Write-Log "nothing new failed on $Branch"
    exit 0
}
Write-Log "dispatching: $($names -join ',')"
$header = "These GitHub Actions workflows failed on $Branch. The end of each failed log follows (gh run view <id> --log-failed prints the whole log).`n"
if ($running) {
    $header += "gh run view prints no logs for a run that is still going; gh api repos/{owner}/{repo}/actions/jobs/<job id>/logs prints the log of one finished job.`n"
}
[Console]::Out.Write($header + (-join @($sections | ForEach-Object { "`n$_" })))
exit 0
