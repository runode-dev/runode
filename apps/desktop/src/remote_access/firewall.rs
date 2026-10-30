//! Windows 防火墙挡没挡着远程访问的局域网连接。宿主第一次监听时系统弹框问放不放行，普通用户取消
//! （放行要管理员）后系统给 runode.exe 记下阻止的规则，手机在同一个网络里也连不上，只能经中继连过来。
//! 这里查当前网络的防火墙规则，和要管理员确认后放行。

use std::{
    io,
    os::windows::ffi::OsStrExt as _,
    path::Path,
    process::{Command, Stdio},
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_CANCELLED},
    System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
    UI::{
        Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::SW_HIDE,
    },
};

/// 局域网连得进来没有。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inbound {
    Allowed,
    Blocked,
}

/// 当前连着的网络（域、专用、公用）里开着防火墙的那几种，有没有一条对 `RUNODE_EXE` 生效的入站规则放行它在
/// `RUNODE_PORT` 上的 TCP 连接：对它的阻止规则不论协议和端口都压过放行的（放行时一起删掉）；一条都没有时看默认的
/// 入站动作。规则里的值（`Block`、`Private, Public`、`TCP` 这些）不随系统语言变，netsh 的输出会。
const CHECK: &str = r#"
$ErrorActionPreference = 'Stop'
$exe = $env:RUNODE_EXE
$port = [int]$env:RUNODE_PORT
$names = @(Get-NetConnectionProfile | ForEach-Object {
    switch ([string]$_.NetworkCategory) { 'DomainAuthenticated' { 'Domain' } 'Private' { 'Private' } default { 'Public' } }
} | Select-Object -Unique)
$on = @(Get-NetFirewallProfile -PolicyStore ActiveStore | Where-Object {
    [string]$_.Enabled -eq 'True' -and $names -contains [string]$_.Name
})
if ($on.Count -eq 0) { 'allowed'; exit }
$rules = @(Get-NetFirewallApplicationFilter -PolicyStore ActiveStore |
    Where-Object { $_.Program -and ($_.Program -eq $exe -or [Environment]::ExpandEnvironmentVariables($_.Program) -eq $exe) } |
    Get-NetFirewallRule | Where-Object {
        $profile = [string]$_.Profile
        [string]$_.Enabled -eq 'True' -and [string]$_.Direction -eq 'Inbound' -and
            ($profile -eq 'Any' -or @($on | Where-Object { $profile -match $_.Name }).Count -gt 0)
    })
function Covers($rule) {
    $filter = $rule | Get-NetFirewallPortFilter
    if (@('Any', 'TCP', '6') -notcontains [string]$filter.Protocol) { return $false }
    foreach ($local in @($filter.LocalPort)) {
        $range = [string]$local -split '-'
        if ($local -eq 'Any') { return $true }
        if ($range.Count -eq 2 -and $range[0] -match '^\d+$' -and $range[1] -match '^\d+$') {
            if ([int]$range[0] -le $port -and $port -le [int]$range[1]) { return $true }
        } elseif ([string]$local -match '^\d+$' -and [int]$local -eq $port) { return $true }
    }
    $false
}
if (@($rules | Where-Object { [string]$_.Action -eq 'Block' }).Count -gt 0) { 'blocked'; exit }
if (@($rules | Where-Object { [string]$_.Action -eq 'Allow' -and (Covers $_) }).Count -gt 0) { 'allowed'; exit }
if (@($on | Where-Object { [string]$_.DefaultInboundAction -ne 'Allow' }).Count -eq 0) { 'allowed'; exit }
'blocked'
"#;

/// 查 `exe` 在 TCP 端口 `port` 上的入站连接挡没挡着。要起 PowerShell、读整张规则表，要一两秒，在后台线程上调；
/// 查不出来时为空。
pub fn inbound(exe: &Path, port: u16) -> Option<Inbound> {
    let mut command = Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", CHECK])
        .env("RUNODE_EXE", exe)
        .env("RUNODE_PORT", port.to_string())
        .stdin(Stdio::null());
    crate::subprocess::hide_window(&mut command);
    let output = command.output().inspect_err(|err| tracing::debug!("cannot check the firewall: {err}")).ok()?;
    match String::from_utf8_lossy(&output.stdout).trim() {
        "allowed" => Some(Inbound::Allowed),
        "blocked" => Some(Inbound::Blocked),
        other => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::debug!("cannot check the firewall: {other:?} {}", stderr.trim());
            None
        }
    }
}

/// 放行 `$exe`：先删掉对它的入站规则（取消弹框时系统记下的阻止规则压过放行的），再加一条放行域和专用网络的。
/// 出错时以非零退出。
const ALLOW: &str = r#"
$ErrorActionPreference = 'Stop'
try {
    Get-NetFirewallApplicationFilter |
        Where-Object { $_.Program -and ($_.Program -eq $exe -or [Environment]::ExpandEnvironmentVariables($_.Program) -eq $exe) } |
        Get-NetFirewallRule | Where-Object { [string]$_.Direction -eq 'Inbound' } | Remove-NetFirewallRule
    New-NetFirewallRule -DisplayName Runode -Direction Inbound -Action Allow -Profile Private,Domain -Program $exe |
        Out-Null
} catch { exit 1 }
"#;

/// 要管理员确认后照 `ALLOW` 放行 `exe` 在域和专用网络里的入站连接。提权起的进程拿不到这边设的环境变量，路径
/// 写成 PowerShell 的单引号字符串放在脚本里，整段经 `-EncodedCommand` 交过去，不经 cmd 解析，`%` 和引号都不展开。
/// 等到脚本跑完才返回；用户在确认框里拒绝时返回 `ErrorKind::PermissionDenied`。
pub fn allow(exe: &Path) -> io::Result<()> {
    let exe =
        exe.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the program path is not Unicode"))?;
    let parameters =
        format!("-NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {}", encode_script(exe));
    let wide = |text: &str| std::ffi::OsStr::new(text).encode_wide().chain([0]).collect::<Vec<u16>>();
    let (verb, file, parameters) = (wide("runas"), wide("powershell.exe"), wide(&parameters));
    // SAFETY: 全零是 SHELLEXECUTEINFOW 合法的初值，下面填的指针在调用期间都活着。
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: `info` 填好了，字符串都以 0 结尾。
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the administrator prompt was declined"));
        }
        return Err(err);
    }
    let process = info.hProcess;
    if process.is_null() {
        return Ok(());
    }
    let mut code = 0;
    // SAFETY: `process` 是 ShellExecuteExW 交来的进程句柄，用完关掉。
    let got = unsafe {
        WaitForSingleObject(process, INFINITE);
        let got = GetExitCodeProcess(process, &mut code);
        CloseHandle(process);
        got
    };
    if got != 0 && code != 0 {
        return Err(io::Error::other(format!("PowerShell exited with {code}")));
    }
    Ok(())
}

/// `-EncodedCommand` 的参数：开头给 `$exe` 赋上 `exe`，接着是 `ALLOW`，整段按 UTF-16LE 编成 Base64。
fn encode_script(exe: &str) -> String {
    let script = format!("$exe = '{}'\n{ALLOW}", exe.replace('\'', "''"));
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                text.push(char::from(TABLE[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_the_last_group() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn the_program_path_stays_literal() {
        let script = encode_script(r"C:\100%USERNAME%\it's\runode.exe");
        let bytes: Vec<u8> = (0..script.len()).step_by(4).flat_map(|i| decode_group(&script[i..i + 4])).collect();
        let units: Vec<u16> = bytes.chunks(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        let decoded = String::from_utf16(&units).unwrap();
        assert!(decoded.starts_with("$exe = 'C:\\100%USERNAME%\\it''s\\runode.exe'\n"));
    }

    fn decode_group(group: &str) -> Vec<u8> {
        const TABLE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let digits: Vec<u32> = group.chars().take_while(|&c| c != '=').map(|c| TABLE.find(c).unwrap() as u32).collect();
        let n = digits.iter().enumerate().fold(0, |n, (i, d)| n | d << (18 - 6 * i));
        (0..digits.len() - 1).map(|i| (n >> (16 - 8 * i)) as u8).collect()
    }
}
