//! Windows 远端的脚本。Windows 的 OpenSSH 把命令交给默认 shell（cmd 或 PowerShell），所以一律写成
//! `cmd /d /v:on /c "…"`：两种 shell 都把引号里的原样交给 cmd。脚本里不出现 `"`（结束引号）、换行和 PowerShell
//! 在双引号里会展开的 `$`、`` ` ``。cmd 先进家目录，路径都相对它写，家目录名里有空格也不用加引号。家目录
//! 一律写成延迟展开的 `!USERPROFILE!`：`%USERPROFILE%` 在 cmd 拆命令之前展开，用户名里的 `&`、`^`、`(` 会被当成
//! 操作符，延迟展开的值不再拆。所以脚本里除了延迟展开的变量，不能有 `!` 和 `^`。
//!
//! runode.exe 是界面程序，经 cmd 起时拿不到 ssh 的 stdin、stdout，所以经旁边的控制台程序 rn.exe 起，由它
//! 把 stdin、stdout 交下去。要传的数据用系统自带的 tar.exe 从 stdin 解开，写全路径：PATH 里排在前面的
//! GNU tar（Git、MSYS 带的）解不了 zip。不交给 PowerShell 解：PowerShell 自己也读 stdin，
//! 交给它读会卡住。PowerShell 只跑不读 stdin 的逻辑（核对摘要、换上构建、清理旧构建），脚本经
//! `-EncodedCommand` 传，不用引号；stdin 还要交给后面的程序时，它的 stdin 接到 `nul`。cmd 一行最长 `CMD_LINE_MAX` 个字符，
//! 编码后的脚本是原文的两倍多，脚本要写得紧：`-ErrorAction` 一律写成 `-EA`，`SilentlyContinue` 写成 `0`。

use super::{
    BIN_DIR, BUILD_KEEP_DAYS, Entry, PART_KEEP_DAYS, PROBE_EXIT, PRUNED, SUMS_FILE, UPLOADED, UPLOADS_DIR,
    install_dir_name,
};

/// cmd 一行最多这么多个字符。
const CMD_LINE_MAX: usize = 8191;
/// 拼出的命令离 `CMD_LINE_MAX` 至少留这么多：sshd 在前面加的默认 shell 的路径和参数，`!USERPROFILE!` 展开成的
/// 家目录（最长是 Windows 路径的上限 260）。
const CMD_LINE_ROOM: usize = 512;

/// 交给 ssh 的一行远端命令：进家目录，再跑 `script`。
fn wrap(script: &str) -> String {
    debug_assert!(!script.contains(['"', '$', '`', '\n', '^']), "脚本里有远端 shell 解释不了的字符：{script}");
    format!(r#"cmd /d /v:on /c "cd /d !USERPROFILE! || exit 1 & {script}""#)
}

/// 家目录下的相对路径，cmd 的写法。
fn local(path: &str) -> String {
    path.replace('/', "\\")
}

/// PowerShell 脚本的开头：`$D` 是放构建的目录；`Test-Build <目录名>` 判断这个构建装好了没有：有 `SUMS_FILE`，
/// 里面记着 rn.exe 和 runode.exe，记着的文件都在、重算的摘要（不分大小写）都对得上。PowerShell 的变量名不分
/// 大小写，各段脚本的变量名不能只差大小写。
fn preamble() -> String {
    format!(
        concat!(
            "$ProgressPreference = 'SilentlyContinue'; $D = Join-Path $env:USERPROFILE '{bin}'; ",
            "function Test-Build($name) {{ try {{ $build = Join-Path $D $name; $seen = @{{}}; ",
            "foreach ($line in @(Get-Content -LiteralPath (Join-Path $build '{sums}') -EA Stop)) {{ ",
            "$text = $line.Trim(); if (-not $text) {{ continue }}; ",
            "$fields = $text -split '\\s+'; if ($fields.Count -ne 2) {{ return $false }}; ",
            "$hash = Get-FileHash -LiteralPath (Join-Path $build $fields[1]) -Algorithm SHA256 -EA Stop; ",
            "if ($hash.Hash -ne $fields[0]) {{ return $false }}; $seen[$fields[1]] = 1 }}; ",
            "return [bool]($seen['rn.exe'] -and $seen['runode.exe']) }} catch {{ return $false }} }}; "
        ),
        bin = local(BIN_DIR),
        sums = SUMS_FILE,
    )
}

/// 连上远端：装好了 `dir` 这个构建（`preamble` 的 `Test-Build`，每次连上都重算一遍摘要）就换成它的桥接
/// （`runode --ssh-bridge`）；没装、没有清单、对不上（装好后被改坏、截断）就打出 `RUNODE-PROBE` 开头的系统和
/// 架构，以 `PROBE_EXIT` 退出，由调用方重新装。
pub fn connect_command(dir: &str) -> String {
    let check = powershell(&format!("{}if (Test-Build '{dir}') {{ exit 0 }}; exit 1", preamble()));
    wrap(&format!(
        concat!(
            "{check} <nul >nul 2>nul || ",
            "(echo RUNODE-PROBE os=Windows& echo RUNODE-PROBE arch=%PROCESSOR_ARCHITECTURE%& exit {exit}) & ",
            "{rn} --ssh-bridge"
        ),
        check = check,
        exit = PROBE_EXIT,
        rn = rn_exe(dir),
    ))
}

fn rn_exe(dir: &str) -> String {
    format!(r"{}\{dir}\rn.exe", local(BIN_DIR))
}

/// 经装着的 `dir` 这个构建的 rn.exe 起它的 `entry`；没装时以 `PROBE_EXIT` 退出。不核对摘要。
pub fn run_command(dir: &str, entry: Entry) -> String {
    let rn = rn_exe(dir);
    wrap(&format!("if exist {rn} ({rn} {}) else (exit {PROBE_EXIT})", entry.arg()))
}

/// `SUMS_FILE` 的各行写成 cmd 的 `echo`，用 `& ` 连起来。文件名和摘要原样拼进脚本，所以先核对：文件名只能有
/// 字母、数字和 `.`、`-`、`_`，不以 `.` 开头，摘要是 64 位十六进制，要有 rn.exe 和 runode.exe。
fn sums_lines(sums: &[(String, String)]) -> Result<String, String> {
    for required in ["rn.exe", "runode.exe"] {
        if !sums.iter().any(|(name, _)| name == required) {
            return Err(format!("the package has no {required}"));
        }
    }
    let mut lines = Vec::with_capacity(sums.len());
    for (name, sha) in sums {
        if install_dir_name(name) != *name || name.eq_ignore_ascii_case(SUMS_FILE) {
            return Err(format!("the package has a file runode cannot install: {name}"));
        }
        if sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("not a SHA-256 of {name}: {sha}"));
        }
        lines.push(format!("echo {sha}  {name}"));
    }
    Ok(lines.join("& "))
}

/// 把 stdin 上的 zip（`runode.exe`、`rn.exe` 和终端要的文件）装成 `dir` 这个构建。`sums` 是包里各个文件的名字和
/// SHA-256（十六进制），由调用方从核对过的包里算出来；不合 `sums_lines` 的写法时交回原因，不出脚本。
///
/// 先解到 `part` 这个临时目录，核对有 runode.exe、试跑一下 rn.exe，写上 `SUMS_FILE`，按它核对解出来的文件，
/// 都过了才改名换上；哪一步没成都删掉临时目录。zip 里每个文件带着 CRC，tar.exe 解的时候核对，传坏了、传不全的
/// 解不出来；摘要对不上时 stderr 上打一行 `RUNODE-INSTALL checksum`。别的连接已经装好了 `dir` 就用它的；`dir` 在
/// 但没装好（没有清单、文件被改坏）就先改名成 `.gone-` 开头，留给 `prune_script` 删，改不了名（里面有程序在跑）
/// 时不装，stderr 上打一行 `RUNODE-INSTALL busy`。换上以后删掉放了超过 `BUILD_KEEP_DAYS` 天的旧构建。`part` 由
/// 调用方每次另起，以 `.part-` 开头，只能有字母、数字和 `.`、`-`、`_`。拼出的命令超过 `CMD_LINE_MAX` 减去
/// `CMD_LINE_ROOM` 时也交回原因。
pub fn install_command(dir: &str, part: &str, sums: &[(String, String)]) -> Result<String, String> {
    let echoes = sums_lines(sums)?;
    let valid = |name: &str| name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte));
    if !part.starts_with(".part-") || !valid(part) || install_dir_name(dir) != dir {
        return Err(format!("cannot install into {dir} by way of {part}"));
    }
    let place = format!(
        concat!(
            "{preamble}$staged = '{part}'; $wanted = '{dir}'; $from = Join-Path $D $staged; $to = Join-Path $D $wanted; ",
            "function Done($code) {{ Remove-Item -LiteralPath $from -Recurse -Force -EA 0; exit $code }}; ",
            "function Kept {{ if (Test-Build $wanted) {{ Done 0 }} }}; ",
            "if (-not (Test-Build $staged)) {{ Done 3 }}; Kept; ",
            "if (Test-Path -LiteralPath $to) {{ ",
            "try {{ Rename-Item -LiteralPath $to -NewName ('.gone-' + $wanted + $staged) -EA Stop }} ",
            "catch {{ Kept; Done 4 }} }}; ",
            "try {{ Rename-Item -LiteralPath $from -NewName $wanted -EA Stop }} catch {{ Kept; Done 1 }}; ",
            "try {{ {prune} }} catch {{ }}; exit 0"
        ),
        preamble = preamble(),
        part = part,
        dir = dir,
        prune = prune_script(dir, Some(BUILD_KEEP_DAYS)),
    );
    let command = wrap(&format!(
        concat!(
            "(if not exist {bin} mkdir {bin}) & cd {bin} || exit 1 & ",
            "mkdir {part} || exit 1 & ",
            "!SystemRoot!\\System32\\tar.exe -xf - -C {part} || (rd /s /q {part} & echo RUNODE-INSTALL checksum 1>&2 & exit 1) & ",
            r"(if not exist {part}\runode.exe (rd /s /q {part} & echo RUNODE-INSTALL exec 1>&2 & exit 1)) & ",
            r"{part}\rn.exe --version >nul 2>nul || (rd /s /q {part} & echo RUNODE-INSTALL exec 1>&2 & exit 1) & ",
            r"({echoes})>{part}\{sums} || (rd /s /q {part} & exit 1) & ",
            "{place} <nul >nul 2>nul && exit 0 & ",
            "(if errorlevel 3 if not errorlevel 4 (echo RUNODE-INSTALL checksum 1>&2)) & ",
            "(if errorlevel 4 if not errorlevel 5 (echo RUNODE-INSTALL busy 1>&2)) & exit 1"
        ),
        bin = local(BIN_DIR),
        part = part,
        echoes = echoes,
        sums = SUMS_FILE,
        place = powershell(&place),
    ));
    let limit = CMD_LINE_MAX - CMD_LINE_ROOM;
    if command.len() > limit {
        return Err(format!(
            "the command to install {} files would be {} characters long, more than the {limit} the remote cmd can take",
            sums.len(),
            command.len()
        ));
    }
    Ok(command)
}

/// 删掉远端除 `keep` 以外的构建，打出删掉了哪些（`super::parse_pruned` 认）。
pub fn prune_command(keep: &str) -> String {
    powershell(&format!("{}{}", preamble(), prune_script(keep, None)))
}

/// 删掉 `$D`（`preamble`）下构建目录的 PowerShell 脚本：跳过 `keep`、以 `.` 开头的（装到一半的）和改不了名的
/// （里面有程序在跑）；给了 `days` 时只删放满这么多天的。先改名成 `.gone-` 开头再删，删到一半也不会留下缺文件的
/// 构建；删干净了才算删掉，没删干净的 `.gone-` 目录下次再删。装到一半被打断留下的 `.part-` 目录放满
/// `PART_KEEP_DAYS` 天也删。
fn prune_script(keep: &str, days: Option<u32>) -> String {
    let cut = days.map_or_else(|| "[DateTime]::MaxValue".to_owned(), |days| format!("(Get-Date).AddDays(-{days})"));
    format!(
        concat!(
            "$cut = {cut}; $stale = (Get-Date).AddDays(-{part_days}); ",
            "Get-ChildItem -LiteralPath $D -Directory -EA 0 | ",
            "Where-Object {{ $_.Name.StartsWith('.gone-') -or ($_.Name.StartsWith('.part-') -and $_.LastWriteTime -lt $stale) }} | ",
            "ForEach-Object {{ Remove-Item -LiteralPath $_.FullName -Recurse -Force -EA 0 }}; ",
            "Get-ChildItem -LiteralPath $D -Directory -EA 0 | ",
            "Where-Object {{ $_.Name -ne '{keep}' -and -not $_.Name.StartsWith('.') -and $_.LastWriteTime -lt $cut }} | ",
            "ForEach-Object {{ ",
            "$k = [long](((Get-ChildItem -LiteralPath $_.FullName -Recurse -File -Force | Measure-Object Length -Sum).Sum) / 1KB); ",
            "$gone = '.gone-' + $_.Name; ",
            "try {{ Rename-Item -LiteralPath $_.FullName -NewName $gone -EA Stop }} catch {{ return }}; ",
            "$path = Join-Path $D $gone; ",
            "Remove-Item -LiteralPath $path -Recurse -Force -EA 0; ",
            "if (-not (Test-Path -LiteralPath $path)) {{ '{pruned}' + $_.Name + ' ' + $k }} }}"
        ),
        cut = cut,
        part_days = PART_KEEP_DAYS,
        keep = keep,
        pruned = PRUNED,
    )
}

/// 把 stdin 上的 tar 包解到 `UPLOADS_DIR` 下新建的子目录 `id` 里，打出这个子目录（`parse_uploaded` 认）。
/// 放上传的目录由远端的桥接收紧访问控制、删掉放久了的。
pub fn upload_command(id: &str) -> String {
    debug_assert_eq!(super::install_dir_name(id), id);
    let uploads = local(UPLOADS_DIR);
    wrap(&format!(
        concat!(
            "(if not exist {uploads} mkdir {uploads}) & cd {uploads} || exit 1 & ",
            "mkdir {id} || exit 1 & !SystemRoot!\\System32\\tar.exe -xf - -C {id} || exit 1 & ",
            r"echo {uploaded}!USERPROFILE!\{uploads}\{id}"
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

/// 跑 PowerShell 脚本 `script` 的一行命令。
fn powershell(script: &str) -> String {
    format!("powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {}", encoded_command(script))
}

/// PowerShell 的 `-EncodedCommand` 的参数：`script` 按 UTF-16LE 编成 base64，不经任何 shell 的引号和展开。
pub fn encoded_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
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
    use super::{CMD_LINE_MAX, CMD_LINE_ROOM, base64, encoded_command, install_command, preamble};

    /// `encoded_command` 的逆：base64 解回 UTF-16LE 再解成文字。
    fn decode(encoded: &str) -> String {
        const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bytes = Vec::new();
        for chunk in encoded.as_bytes().chunks(4) {
            let digits: Vec<u32> = chunk
                .iter()
                .take_while(|&&byte| byte != b'=')
                .map(|byte| ALPHABET.iter().position(|a| a == byte).unwrap() as u32)
                .collect();
            let n = digits.iter().enumerate().fold(0u32, |n, (ix, digit)| n | digit << (18 - 6 * ix));
            bytes.extend((0..digits.len() - 1).map(|ix| (n >> (16 - 8 * ix)) as u8));
        }
        let units: Vec<u16> = bytes.chunks(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn install_checks_the_unpacked_build_then_the_installed_one() {
        let sum = "ab".repeat(32);
        let sums = [("runode.exe".to_owned(), sum.clone()), ("rn.exe".to_owned(), sum.clone())];
        let command = install_command("0.1.16", ".part-1", &sums).unwrap();
        assert!(command.contains(&format!(r"(echo {sum}  runode.exe& echo {sum}  rn.exe)>.part-1\SHA256SUMS")));
        let encoded = command.split("-EncodedCommand ").nth(1).unwrap().split(' ').next().unwrap();
        let script = decode(encoded);
        assert!(script.starts_with(&format!("{}$staged = '.part-1'; $wanted = '0.1.16'; ", preamble())), "{script}");
        let part = script.find("if (-not (Test-Build $staged)) { Done 3 }; Kept; ").unwrap();
        let replace = script.find("-NewName ('.gone-' + $wanted + $staged)").unwrap();
        let place = script.find("-NewName $wanted").unwrap();
        let prune = script.find("$cut = (Get-Date).AddDays(-7)").unwrap();
        assert!(part < replace && replace < place && place < prune, "{script}");
        assert_eq!(decode(&encoded_command("Get-ChildItem")), "Get-ChildItem");
    }

    /// 发布的 zip 里的文件、开发构建的目录名和每次另起的临时目录名都放得下；文件多到命令超长时交回原因。
    #[test]
    fn install_refuses_a_command_too_long_for_cmd() {
        let sum = "ab".repeat(32);
        let names = [
            "runode.exe",
            "rn.exe",
            "conpty.dll",
            "OpenConsole.exe",
            "conpty-LICENSE.txt",
            "agent-rules-LICENSE.txt",
            "syntaxes-LICENSE.txt",
        ];
        let sums: Vec<_> = names.iter().map(|name| ((*name).to_owned(), sum.clone())).collect();
        let dir = "0.12.345-abcdef123456-dev-1234567890-ffffffffff";
        let part = format!(".part-{}-{:x}", u32::MAX, u32::MAX);
        let command = install_command(dir, &part, &sums).unwrap();
        assert!(command.len() <= CMD_LINE_MAX - CMD_LINE_ROOM, "{}", command.len());
        let many: Vec<_> = (0..60).map(|n| (format!("file-{n}.dll"), sum.clone())).chain(sums).collect();
        let err = install_command(dir, &part, &many).unwrap_err();
        assert!(err.contains("67 files"), "{err}");
    }

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
