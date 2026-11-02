# Builds runode on this Windows machine and packs the zip an SSH remote installs: runode.exe and rn.exe
# with the ConPTY files beside them, for the architecture of this machine.
param(
    [ValidateSet('debug', 'release')]
    [string]$BuildProfile = 'debug',
    [Parameter(Mandatory = $true)]
    [string]$Output
)

$output = [IO.Path]::GetFullPath((Join-Path (Get-Location) $Output))
$repoRoot = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$architecture = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repoRoot 'target' }
$binDir = Join-Path $targetDir $BuildProfile

$cargoArgs = @('build', '-p', 'runode')
if ($BuildProfile -eq 'release') { $cargoArgs += '--release' }
Push-Location $repoRoot
try {
    # Native stderr must not become a terminating error: cargo reports progress there.
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
} finally {
    Pop-Location
}

& (Join-Path $PSScriptRoot 'setup-conpty.ps1') -Destination $binDir -Architecture $architecture
if (Test-Path -LiteralPath $output) { Remove-Item -LiteralPath $output }
& tar.exe -a -cf $output -C $binDir runode.exe rn.exe conpty.dll OpenConsole.exe conpty-LICENSE.txt
if ($LASTEXITCODE -ne 0) { throw "tar failed with exit code $LASTEXITCODE" }
Write-Output "Packed $output ($architecture, $BuildProfile)"
