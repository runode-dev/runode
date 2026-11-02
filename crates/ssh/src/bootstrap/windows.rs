//! Windows 远端的脚本。Windows 的 OpenSSH 把命令交给默认 shell（cmd 或 PowerShell），所以一律写成
//! `cmd /d /c "…"`：两种 shell 都把引号里的原样交给 cmd。脚本里不出现 `"`（结束引号）、换行和 PowerShell
//! 在双引号里会展开的 `$`、`` ` ``。cmd 先进家目录，路径都相对它写，家目录名里有空格也不用加引号。
//!
//! runode.exe 是界面程序，经 cmd 起时拿不到 ssh 的 stdin、stdout，所以经旁边的控制台程序 rn.exe 起，由它
//! 把 stdin、stdout 交下去。要传的数据用系统自带的 tar.exe 从 stdin 解开：PowerShell 自己也读 stdin，
//! 交给它读会卡住。PowerShell 只跑不读 stdin 的逻辑（清理旧构建），脚本经 `-EncodedCommand` 传，不用引号。

use super::{BIN_DIR, BUILD_KEEP_DAYS, Entry, PROBE_EXIT, PRUNED, UPLOADED, UPLOADS_DIR};

/// 交给 ssh 的一行远端命令：进家目录，再跑 `script`。
fn wrap(script: &str) -> String {
    debug_assert!(!script.contains(['"', '$', '`', '\n']), "脚本里有远端 shell 解释不了的字符：{script}");
    format!(r#"cmd /d /c "cd /d %USERPROFILE% || exit 1 & {script}""#)
}

/// 家目录下的相对路径，cmd 的写法。
fn local(path: &str) -> String {
    path.replace('/', "\\")
}

/// 装着 `dir` 这个构建就经它的 rn.exe 跑 `runode.exe <arg>`；没装时以 `PROBE_EXIT` 退出，`probe` 时先打出
/// 平台信息。
fn run_installed(dir: &str, arg: &str, probe: bool) -> String {
    let rn = format!(r"{}\{dir}\rn.exe", local(BIN_DIR));
    let missing = if probe {
        format!("echo RUNODE-PROBE os=Windows& echo RUNODE-PROBE arch=%PROCESSOR_ARCHITECTURE%& exit {PROBE_EXIT}")
    } else {
        format!("exit {PROBE_EXIT}")
    };
    wrap(&format!("if exist {rn} ({rn} {arg}) else ({missing})"))
}

/// 连上远端：装着 `dir` 这个构建就换成它的桥接（`runode --ssh-bridge`）；没装就打出 `RUNODE-PROBE` 开头的系统和
/// 架构，以 `PROBE_EXIT` 退出。
pub fn connect_command(dir: &str) -> String {
    run_installed(dir, "--ssh-bridge", true)
}

/// 起装着的 `dir` 这个构建的 `entry`；没装时以 `PROBE_EXIT` 退出。
pub fn run_command(dir: &str, entry: Entry) -> String {
    run_installed(dir, entry.arg(), false)
}

/// 把 stdin 上的 zip（`runode.exe`、`rn.exe` 和终端要的文件）装成 `dir` 这个构建：先解到 `part` 这个临时目录，
/// 核对有 runode.exe、试跑一下 rn.exe，都过了才改名换上；哪一步没成都删掉临时目录。别的连接已经装好了 `dir`
/// （里面有 rn.exe）就用它的；`dir` 是没装完的残留就先删掉，删不干净时不装：`move` 遇到已有的目录会把临时
/// 目录挪进它里面。zip 里每个文件带着 CRC，tar.exe 解的时候
/// 核对，传坏了、传不全的解不出来。换上以后删掉放了超过 `BUILD_KEEP_DAYS` 天的旧构建。`part` 由调用方每次另起，
/// 只能有字母、数字和 `.`、`-`、`_`。
pub fn install_command(dir: &str, part: &str) -> String {
    wrap(&format!(
        concat!(
            "(if not exist {bin} mkdir {bin}) & cd {bin} || exit 1 & ",
            "mkdir {part} || exit 1 & ",
            "tar -xf - -C {part} || (rd /s /q {part} & echo RUNODE-INSTALL checksum 1>&2 & exit 1) & ",
            r"(if not exist {part}\runode.exe (rd /s /q {part} & echo RUNODE-INSTALL exec 1>&2 & exit 1)) & ",
            r"{part}\rn.exe --version >nul 2>nul || (rd /s /q {part} & echo RUNODE-INSTALL exec 1>&2 & exit 1) & ",
            r"(if exist {dir}\rn.exe (rd /s /q {part} & exit 0)) & ",
            "(if exist {dir} rd /s /q {dir}) & ",
            "(if exist {dir} (rd /s /q {part} & exit 1)) & ",
            "move {part} {dir} >nul || (rd /s /q {part} & exit 1) & ",
            "{prune} >nul 2>nul & exit 0"
        ),
        bin = local(BIN_DIR),
        part = part,
        dir = dir,
        prune = powershell(&prune_script(dir, Some(BUILD_KEEP_DAYS))),
    ))
}

/// 删掉远端除 `keep` 以外的构建，打出删掉了哪些（`super::parse_pruned` 认）。
pub fn prune_command(keep: &str) -> String {
    powershell(&prune_script(keep, None))
}

/// 删掉构建目录的 PowerShell 脚本：跳过 `keep`、以 `.` 开头的（装到一半的）和改不了名的（里面有程序在跑）；
/// 给了 `days` 时只删放满这么多天的。先改名成 `.gone-` 开头再删，删到一半也不会留下缺文件的构建；删干净了
/// 才算删掉，没删干净的 `.gone-` 目录下次再删。
fn prune_script(keep: &str, days: Option<u32>) -> String {
    let cut = days.map_or_else(|| "[DateTime]::MaxValue".to_owned(), |days| format!("(Get-Date).AddDays(-{days})"));
    format!(
        concat!(
            "$ProgressPreference = 'SilentlyContinue'; ",
            "$D = Join-Path $env:USERPROFILE '{bin}'; $cut = {cut}; ",
            "Get-ChildItem -LiteralPath $D -Directory -Filter '.gone-*' -ErrorAction SilentlyContinue | ",
            "ForEach-Object {{ Remove-Item -LiteralPath $_.FullName -Recurse -Force -ErrorAction SilentlyContinue }}; ",
            "Get-ChildItem -LiteralPath $D -Directory -ErrorAction SilentlyContinue | ",
            "Where-Object {{ $_.Name -ne '{keep}' -and -not $_.Name.StartsWith('.') -and $_.LastWriteTime -lt $cut }} | ",
            "ForEach-Object {{ ",
            "$k = [long](((Get-ChildItem -LiteralPath $_.FullName -Recurse -File -Force | Measure-Object Length -Sum).Sum) / 1KB); ",
            "$gone = '.gone-' + $_.Name; ",
            "try {{ Rename-Item -LiteralPath $_.FullName -NewName $gone -ErrorAction Stop }} catch {{ return }}; ",
            "$path = Join-Path $D $gone; ",
            "Remove-Item -LiteralPath $path -Recurse -Force -ErrorAction SilentlyContinue; ",
            "if (-not (Test-Path -LiteralPath $path)) {{ '{pruned}' + $_.Name + ' ' + $k }} }}"
        ),
        bin = local(BIN_DIR),
        cut = cut,
        keep = keep,
        pruned = PRUNED,
    )
}

/// 把 stdin 上的 tar 包解到 `UPLOADS_DIR` 下新建的子目录 `id` 里，打出这个子目录（`parse_uploaded` 认）。
/// 放久了的上传由远端的桥接清掉。
pub fn upload_command(id: &str) -> String {
    debug_assert_eq!(super::install_dir_name(id), id);
    let uploads = local(UPLOADS_DIR);
    wrap(&format!(
        concat!(
            "(if not exist {uploads} mkdir {uploads}) & cd {uploads} || exit 1 & ",
            "mkdir {id} || exit 1 & tar -xf - -C {id} || exit 1 & ",
            r"echo {uploaded}%USERPROFILE%\{uploads}\{id}"
        ),
        uploads = uploads,
        id = id,
        uploaded = UPLOADED,
    ))
}

/// `upload_command` 的输出里这次上传放在远端的目录，分隔符换成 `/`；登录 shell 的启动文件可能先打出别的行，跳过。
pub fn parse_uploaded(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.strip_prefix(UPLOADED))
        .map(|dir| dir.trim_end().replace('\\', "/"))
        .filter(|dir| is_absolute(dir))
}

/// Windows 上的绝对路径（`C:/…`），分隔符已经换成 `/`。
pub fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

/// 跑 PowerShell 脚本 `script` 的一行命令，脚本按 UTF-16LE 编成 base64 交给 `-EncodedCommand`。
fn powershell(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!("powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {}", base64(&bytes))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (ix, &byte)| n | u32::from(byte) << (16 - 8 * ix));
        for ix in 0..4 {
            out.push(if ix <= chunk.len() { char::from(ALPHABET[(n >> (18 - 6 * ix)) as usize & 63]) } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
