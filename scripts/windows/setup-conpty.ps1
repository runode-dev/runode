# Windows 10's inbox ConPTY drops mouse mode requests from Codex, pi, and other TUIs.
# portable-pty loads conpty.dll beside the executable. Keep it paired with OpenConsole.exe.
param(
    [Parameter(Mandatory = $true)]
    [string]$Destination,
    [ValidateSet('x64', 'arm64', 'x86')]
    [string]$Architecture = 'x64',
    [switch]$IncludeTests
)

$ErrorActionPreference = 'Stop'
$version = '1.24.261001001'
$sha256 = '4D6AADDC1D2385C9F5897DF28F33879F699F8F2783315D5204CF3D8C3616AC5F'
$repoRoot = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$cache = Join-Path $repoRoot "target/conpty/$version"
$archive = Join-Path $cache 'conpty.zip'
New-Item -ItemType Directory -Force -Path $cache | Out-Null

if (!(Test-Path -LiteralPath $archive) -or (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $sha256) {
    $url = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$version/microsoft.windows.console.conpty.$version.nupkg"
    & curl.exe --fail --location --silent --show-error --retry 3 --connect-timeout 15 --max-time 120 $url --output "$archive.download"
    if ($LASTEXITCODE -ne 0) { throw "Cannot download ConPTY $version" }
    if ((Get-FileHash -LiteralPath "$archive.download" -Algorithm SHA256).Hash -ne $sha256) {
        throw 'ConPTY package SHA-256 does not match'
    }
    Move-Item -LiteralPath "$archive.download" -Destination $archive -Force
}

$package = Join-Path $cache 'package'
Expand-Archive -LiteralPath $archive -DestinationPath $package -Force
$files = @{
    'conpty.dll' = Join-Path $package "runtimes/win-$Architecture/native/conpty.dll"
    'OpenConsole.exe' = Join-Path $package "build/native/runtimes/$Architecture/OpenConsole.exe"
    'conpty-LICENSE.txt' = Join-Path $PSScriptRoot 'conpty-LICENSE.txt'
}
# Cargo test executables and examples must use the same runtime as the application.
$directories = @($Destination)
if ($IncludeTests) { $directories += @((Join-Path $Destination 'deps'), (Join-Path $Destination 'examples')) }
foreach ($directory in $directories) {
    New-Item -ItemType Directory -Force -Path $directory | Out-Null
    foreach ($name in $files.Keys) {
        $source = $files[$name]
        $target = Join-Path $directory $name
        # Do not overwrite identical files: a running application may have loaded the DLL.
        if (!(Test-Path -LiteralPath $target) -or
            (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash) {
            Copy-Item -LiteralPath $source -Destination $target -Force
        }
    }
}
Write-Output "ConPTY $version ($Architecture) ready in $Destination"
