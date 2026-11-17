//! 远端脚本：在本机用 /bin/sh 真跑一遍，探测、安装、换链接都按约定来；Windows 的脚本在 Windows 上像
//! Windows 的 OpenSSH 那样交给 cmd 跑。

#[cfg(any(unix, windows))]
mod common;

use std::process::Command;
#[cfg(unix)]
use std::{fs, io::Write as _, process::Stdio};

#[cfg(unix)]
use common::TempDir;
#[cfg(unix)]
use runode_ssh::bootstrap::{BIN_DIR, PROBE_EXIT, UPLOADS_DIR, parse_exec_failure, parse_pruned, parse_uploaded};
use runode_ssh::bootstrap::{
    Entry, Libc, Os, Platform, SUMS_FILE, Target, Unsupported, connect_command, install_command, install_dir_name,
    prune_command, run_command, upload_command, windows,
};

/// 像 ssh 那样把命令行交给登录 shell（默认 /bin/sh）解释。
#[cfg(unix)]
fn run(home: &std::path::Path, command: &str, stdin: &[u8]) -> std::process::Output {
    run_in("/bin/sh", home, command, stdin)
}

#[cfg(unix)]
fn run_in(shell: &str, home: &std::path::Path, command: &str, stdin: &[u8]) -> std::process::Output {
    let mut child = Command::new(shell)
        .arg("-c")
        .arg(command)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

#[cfg(unix)]
fn sha256_hex(path: &std::path::Path) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .or_else(|_| Command::new("sha256sum").arg(path).output())
        .unwrap();
    String::from_utf8(output.stdout).unwrap().split_whitespace().next().unwrap().to_owned()
}

#[test]
fn scripts_avoid_characters_remote_shells_mangle() {
    for command in [
        connect_command("0.1.16.abc"),
        install_command("0.1.16.abc", &"0".repeat(64), true),
        upload_command("1-2", None),
        upload_command("1-2", Some("a.png")),
        run_command("0.1.16.abc", Entry::Project),
        run_command("0.1.16.abc", Entry::Screen),
        run_command("0.1.16.abc", Entry::Service),
        prune_command("0.1.16.abc"),
    ] {
        let inner = command.strip_prefix("/bin/sh -c '").and_then(|rest| rest.strip_suffix('\'')).unwrap();
        assert!(!inner.contains(['\'', '!', '\n']), "{inner}");
        if cfg!(unix) {
            assert!(Command::new("/bin/sh").args(["-n", "-c", inner]).status().unwrap().success());
        }
    }
    // 发布的便携包里的文件，摘要按最长的写法算。
    let sums: Vec<(String, String)> = [
        "runode.exe",
        "rn.exe",
        "conpty.dll",
        "OpenConsole.exe",
        "conpty-LICENSE.txt",
        "agent-rules-LICENSE.txt",
        "syntaxes-LICENSE.txt",
    ]
    .iter()
    .map(|name| ((*name).to_owned(), "a".repeat(64)))
    .collect();
    let dir = "0.1.16.abc-dirty-dev-1234567-69000000";
    for command in [
        windows::connect_command(dir),
        windows::install_command(dir, ".part-123456-3b9aca00", &sums).unwrap(),
        windows::upload_command("1-2"),
        windows::run_command(dir, Entry::Project),
        windows::run_command(dir, Entry::Screen),
    ] {
        let inner = command.strip_prefix("cmd /d /v:on /c \"").and_then(|rest| rest.strip_suffix('"')).unwrap();
        // 开着延迟展开：`!` 只出现在延迟展开的 `!USERPROFILE!`、`!SystemRoot!` 里，环境变量不经 `%…%` 在拆命令前展开。
        assert!(!inner.contains(['"', '$', '`', '\n', '^']), "{inner}");
        let delayed = inner.matches("!USERPROFILE!").count() + inner.matches("!SystemRoot!").count();
        assert_eq!(inner.matches('!').count(), delayed * 2, "{inner}");
        assert!(!inner.contains("%USERPROFILE%"), "{inner}");
        // cmd 一行最多 8191 个字符，经默认 shell 再交给 cmd 时整条命令都在那一行上；留些余量。
        assert!(command.len() < 7800, "{} characters: {command}", command.len());
    }
    assert!(
        windows::prune_command("0.1.16.abc")
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b" -+/=".contains(&byte))
    );
    assert_eq!(install_dir_name("0.1.16.abc-dirty.x/y"), "0.1.16.abc-dirty.x_y");
    assert_eq!(install_dir_name(".."), "_..");
}

/// 文件名和摘要原样拼进 cmd 脚本：写不进去的、缺了要核对的程序的，不出脚本。
#[test]
fn windows_install_refuses_what_it_cannot_write_into_the_script() {
    let sum = "0123456789abcdefABCDEF".repeat(3)[..64].to_owned();
    let sums = |extra: &[(&str, &str)]| -> Vec<(String, String)> {
        [("runode.exe", sum.as_str()), ("rn.exe", sum.as_str())]
            .iter()
            .chain(extra)
            .map(|(name, sha)| ((*name).to_owned(), (*sha).to_owned()))
            .collect()
    };
    assert!(windows::install_command("0.1.16", ".part-1", &sums(&[("conpty.dll", &sum)])).is_ok());
    for extra in [
        [("a&b.dll", sum.as_str())],
        [("a b.dll", sum.as_str())],
        [(r"..\x.dll", sum.as_str())],
        [(".hidden", sum.as_str())],
        [("", sum.as_str())],
        [(SUMS_FILE, sum.as_str())],
        [("conpty.dll", &sum[..63])],
        [("conpty.dll", &"g".repeat(64))],
        [("conpty.dll", &format!("{}&", &sum[..63]))],
    ] {
        assert!(windows::install_command("0.1.16", ".part-1", &sums(&extra)).is_err(), "{extra:?}");
    }
    let launcher_only = vec![("rn.exe".to_owned(), sum.clone())];
    assert!(windows::install_command("0.1.16", ".part-1", &launcher_only).is_err());
    assert!(windows::install_command("0.1.16", "part&1", &sums(&[])).is_err());
    assert!(windows::install_command("0.1&16", ".part-1", &sums(&[])).is_err());
}

#[cfg(unix)]
#[test]
fn probes_then_installs_then_bridges() {
    let home = TempDir::new("bootstrap");
    let dir = "0.1.16.test";

    let probe = run(&home.0, &connect_command(dir), b"");
    assert_eq!(probe.status.code(), Some(PROBE_EXIT));
    let platform = Platform::parse(&String::from_utf8_lossy(&probe.stdout)).unwrap();
    assert_eq!(platform.os == Os::MacOs, cfg!(target_os = "macos"));
    assert_eq!(platform.home, home.0.to_string_lossy());
    assert!(platform.installed.is_empty());
    assert_eq!(platform.target().ok(), Target::current());

    // 假的 runode：`--ssh-bridge` 时打一行，别的参数直接成功。
    let fake = home.0.join("fake");
    fs::write(&fake, "#!/bin/sh\n[ \"$1\" = --ssh-bridge ] && echo bridged\nexit 0\n").unwrap();
    let wrong = run(&home.0, &install_command(dir, &"0".repeat(64), false), &fs::read(&fake).unwrap());
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("RUNODE-INSTALL checksum"));
    let installed = run(&home.0, &install_command(dir, &sha256_hex(&fake), false), &fs::read(&fake).unwrap());
    assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
    let link = home.0.join(BIN_DIR).join("runode");
    assert_eq!(fs::read_link(&link).unwrap(), std::path::Path::new(dir).join("runode"));

    let build = home.0.join(BIN_DIR).join(dir);
    assert_eq!(fs::read_to_string(build.join(SUMS_FILE)).unwrap(), format!("{}  runode\n", sha256_hex(&fake)));

    let bridged = run(&home.0, &connect_command(dir), b"");
    assert_eq!(String::from_utf8_lossy(&bridged.stdout), "bridged\n");
    let other = run(&home.0, &connect_command("0.1.17.next"), b"");
    let platform = Platform::parse(&String::from_utf8_lossy(&other.stdout)).unwrap();
    assert_eq!(platform.installed, [dir, "runode"]);

    // 装好后被截断：连上时认出来，当作没装；重新装上就好了。
    let original = fs::read(build.join("runode")).unwrap();
    fs::write(build.join("runode"), &original[..original.len() / 2]).unwrap();
    let damaged = run(&home.0, &connect_command(dir), b"");
    assert_eq!(damaged.status.code(), Some(PROBE_EXIT));
    assert!(Platform::parse(&String::from_utf8_lossy(&damaged.stdout)).is_some());
    let reinstalled = run(&home.0, &install_command(dir, &sha256_hex(&fake), false), &original);
    assert!(reinstalled.status.success(), "{}", String::from_utf8_lossy(&reinstalled.stderr));
    assert_eq!(String::from_utf8_lossy(&run(&home.0, &connect_command(dir), b"").stdout), "bridged\n");

    // 没有清单或清单是空的也当作没装。
    fs::remove_file(build.join(SUMS_FILE)).unwrap();
    assert_eq!(run(&home.0, &connect_command(dir), b"").status.code(), Some(PROBE_EXIT));
    fs::write(build.join(SUMS_FILE), "").unwrap();
    assert_eq!(run(&home.0, &connect_command(dir), b"").status.code(), Some(PROBE_EXIT));
}

/// 在 `BIN_DIR` 下摆一个假的构建 `name`，`old` 时把目录的修改时间挪到很久以前。
#[cfg(unix)]
fn fake_build(home: &std::path::Path, name: &str, old: bool) {
    let dir = home.join(BIN_DIR).join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("runode"), "#!/bin/sh\nexit 0\n").unwrap();
    if old {
        assert!(Command::new("touch").args(["-t", "202001010000"]).arg(&dir).status().unwrap().success());
    }
}

#[cfg(unix)]
#[test]
fn installing_drops_builds_left_long_ago() {
    let home = TempDir::new("bootstrap-prune");
    let bin = home.0.join(BIN_DIR);
    for (name, old) in [("old", true), ("previous", true), ("young", false)] {
        fake_build(&home.0, name, old);
    }
    std::os::unix::fs::symlink("previous/runode", bin.join("runode")).unwrap();
    let fake = home.0.join("fake");
    fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
    let installed = run(&home.0, &install_command("new", &sha256_hex(&fake), false), &fs::read(&fake).unwrap());
    assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
    let mut left: Vec<_> = fs::read_dir(&bin).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    left.sort();
    assert_eq!(left, ["new", "previous", "runode", "young"]);
}

#[cfg(unix)]
#[test]
fn pruning_keeps_the_current_linked_and_running_builds() {
    let home = TempDir::new("bootstrap-prune-all");
    let bin = home.0.join(BIN_DIR);
    for name in ["current", "linked", "running", "stale"] {
        fake_build(&home.0, name, false);
    }
    std::os::unix::fs::symlink("linked/runode", bin.join("runode")).unwrap();
    // 命令行里带着 `running` 的可执行文件路径，`ps` 列得出来；`; :` 免得 sh 直接换成 sleep。
    let mut busy = Command::new("/bin/sh").args(["-c", "sleep 30; :"]).arg(bin.join("running/runode")).spawn().unwrap();
    let output = run(&home.0, &format!("echo from .bashrc; {}", prune_command("current")), b"");
    busy.kill().unwrap();
    busy.wait().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let pruned = parse_pruned(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(pruned.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["stale"]);
    assert!(pruned[0].1 > 0);
    let mut left: Vec<_> = fs::read_dir(&bin).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    left.sort();
    assert_eq!(left, ["current", "linked", "running", "runode"]);
}

#[cfg(unix)]
#[test]
fn refuses_a_build_that_cannot_run_and_says_why() {
    let home = TempDir::new("bootstrap-exec");
    let fake = home.0.join("fake");
    fs::write(&fake, "#!/bin/sh\necho \"error while loading shared libraries: libasound.so.2\" >&2\nexit 127\n")
        .unwrap();
    let output = run(&home.0, &install_command("v1", &sha256_hex(&fake), false), &fs::read(&fake).unwrap());
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(parse_exec_failure(&stderr), Some("error while loading shared libraries: libasound.so.2"));
    assert_eq!(fs::read_dir(home.0.join(BIN_DIR).join("v1")).unwrap().count(), 0);
    assert_eq!(parse_exec_failure("RUNODE-INSTALL exec \r\n"), Some(""));
    assert_eq!(parse_exec_failure("RUNODE-INSTALL checksum\n"), None);
}

#[cfg(unix)]
#[test]
fn installs_from_a_tarball() {
    let home = TempDir::new("bootstrap-tar");
    let staging = home.0.join("staging");
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("runode"), "#!/bin/sh\nexit 0\n").unwrap();
    let tar = home.0.join("runode.tar.gz");
    assert!(
        Command::new("tar").arg("-czf").arg(&tar).arg("-C").arg(&staging).arg("runode").status().unwrap().success()
    );
    let output = run(&home.0, &install_command("v1", &sha256_hex(&tar), true), &fs::read(&tar).unwrap());
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let installed = home.0.join(BIN_DIR).join("v1/runode");
    assert!(installed.is_file());
    // 清单记的是解出来的可执行文件，不是包。
    let sums = fs::read_to_string(home.0.join(BIN_DIR).join("v1").join(SUMS_FILE)).unwrap();
    assert_eq!(sums, format!("{}  runode\n", sha256_hex(&installed)));
    assert_eq!(run(&home.0, &connect_command("v1"), b"").status.code(), Some(0));
}

/// 装到一半被打断留下的临时文件，放久了的下次装或清理时删掉，刚留下的（可能还在装）不动。
#[cfg(unix)]
#[test]
fn pruning_drops_leftovers_of_interrupted_installs() {
    let home = TempDir::new("bootstrap-parts");
    fake_build(&home.0, "current", false);
    let build = home.0.join(BIN_DIR).join("current");
    for name in [".part-1", ".part-1.in", ".part-2", ".part-3"] {
        fs::write(build.join(name), "half").unwrap();
    }
    for name in [".part-1", ".part-1.in"] {
        assert!(Command::new("touch").args(["-t", "202001010000"]).arg(build.join(name)).status().unwrap().success());
    }
    // 放了一天多、不满两天的也删。
    let day_and_a_bit = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 3600);
    fs::File::options().write(true).open(build.join(".part-3")).unwrap().set_modified(day_and_a_bit).unwrap();
    let output = run(&home.0, &prune_command("current"), b"");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let mut left: Vec<_> = fs::read_dir(&build).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    left.sort();
    assert_eq!(left, [".part-2", "runode"]);
}

#[test]
fn picks_targets_and_refuses_unsupported_platforms() {
    let platform = |os: Os, arch: &str, libc: Libc| Platform {
        os,
        arch: arch.into(),
        arm64: false,
        libc,
        home: "/home/me".into(),
        installed: Vec::new(),
    };
    assert_eq!(platform(Os::Linux, "x86_64", Libc::Glibc(2, 31)).target(), Ok(Target::LinuxX86_64));
    assert_eq!(platform(Os::Linux, "aarch64", Libc::Unknown).target(), Ok(Target::LinuxAarch64));
    assert_eq!(platform(Os::Linux, "x86_64", Libc::Musl).target(), Err(Unsupported::Musl));
    assert_eq!(platform(Os::Linux, "x86_64", Libc::Glibc(2, 17)).target(), Err(Unsupported::OldGlibc(2, 17)));
    assert_eq!(platform(Os::Linux, "riscv64", Libc::Glibc(2, 35)).target(), Err(Unsupported::Arch("riscv64".into())));
    let mut rosetta = platform(Os::MacOs, "x86_64", Libc::Unknown);
    rosetta.arm64 = true;
    assert_eq!(rosetta.target(), Ok(Target::MacArm64));
    assert_eq!(platform(Os::MacOs, "x86_64", Libc::Unknown).target(), Ok(Target::MacX86_64));
    assert_eq!(
        platform(Os::Other("FreeBSD".into()), "amd64", Libc::Unknown).target(),
        Err(Unsupported::Os("FreeBSD".into()))
    );

    let probed = Platform::parse(
        "noise\nRUNODE-PROBE os=Linux\nRUNODE-PROBE arch=x86_64\nRUNODE-PROBE glibc=glibc 2.17\nRUNODE-PROBE musl=0\nRUNODE-PROBE home=/root\n",
    )
    .unwrap();
    assert_eq!(probed.libc, Libc::Glibc(2, 17));
    let alpine = Platform::parse("RUNODE-PROBE os=Linux\nRUNODE-PROBE glibc=\nRUNODE-PROBE musl=1\n").unwrap();
    assert_eq!(alpine.libc, Libc::Musl);
    assert_eq!(Platform::parse("hello"), None);

    let windows = Platform::parse("RUNODE-PROBE os=Windows\r\nRUNODE-PROBE arch=AMD64\r\n").unwrap();
    assert_eq!(windows.os, Os::Windows);
    assert_eq!(windows.target(), Ok(Target::WindowsX86_64));
    assert_eq!(platform(Os::Windows, "ARM64", Libc::Unknown).target(), Ok(Target::WindowsX86_64));
    assert_eq!(platform(Os::Windows, "x86", Libc::Unknown).target(), Err(Unsupported::Arch("x86".into())));
}

#[test]
fn reads_windows_upload_directories() {
    let output = "noise\r\nRUNODE-UPLOADED C:\\Users\\Jo Do\\.runode\\uploads\\1-a\r\n";
    assert_eq!(windows::parse_uploaded(output).as_deref(), Some("C:/Users/Jo Do/.runode/uploads/1-a"));
    assert_eq!(windows::parse_uploaded("RUNODE-UPLOADED .runode\\uploads\\1-a"), None);
    assert!(windows::is_absolute("c:/"));
    assert!(!windows::is_absolute("/home/me"));
}

/// 登录 shell 是 fish、tcsh、csh、dash 时脚本照样跑得通（机器上没装的跳过）。
#[cfg(unix)]
#[test]
fn runs_under_other_login_shells() {
    let home = TempDir::new("bootstrap-shells");
    for shell in
        ["/bin/dash", "/bin/tcsh", "/bin/csh", "/opt/homebrew/bin/fish", "/usr/bin/fish", "/usr/bin/zsh", "/bin/zsh"]
    {
        if !std::path::Path::new(shell).exists() {
            continue;
        }
        let probe = run_in(shell, &home.0, &connect_command("v1"), b"");
        assert_eq!(probe.status.code(), Some(PROBE_EXIT), "{shell}: {}", String::from_utf8_lossy(&probe.stderr));
        let platform = Platform::parse(&String::from_utf8_lossy(&probe.stdout)).unwrap();
        assert_eq!(platform.home, home.0.to_string_lossy(), "{shell}");
    }
}

#[cfg(unix)]
#[test]
fn uploads_a_file_or_a_tar_into_a_fresh_directory() {
    let home = TempDir::new("upload");
    let noise = "echo from .bashrc; ";
    let one = run(&home.0, &format!("{noise}{}", upload_command("1-a", Some("shot.png"))), b"png bytes");
    assert!(one.status.success(), "{}", String::from_utf8_lossy(&one.stderr));
    let stdout = String::from_utf8_lossy(&one.stdout);
    let dir = std::path::PathBuf::from(parse_uploaded(&stdout).unwrap());
    assert_eq!(dir, home.0.join(UPLOADS_DIR).join("1-a"));
    assert_eq!(fs::read(dir.join("shot.png")).unwrap(), b"png bytes");

    let src = home.0.join("src");
    fs::create_dir_all(src.join("folder")).unwrap();
    fs::write(src.join("folder/inner.txt"), "inner").unwrap();
    fs::write(src.join("note.txt"), "note").unwrap();
    let tar =
        Command::new("tar").arg("-cf").arg("-").arg("-C").arg(&src).args(["folder", "note.txt"]).output().unwrap();
    let many = run(&home.0, &upload_command("1-b", None), &tar.stdout);
    assert!(many.status.success(), "{}", String::from_utf8_lossy(&many.stderr));
    let dir = std::path::PathBuf::from(parse_uploaded(&String::from_utf8_lossy(&many.stdout)).unwrap());
    assert_eq!(fs::read_to_string(dir.join("folder/inner.txt")).unwrap(), "inner");
    assert_eq!(fs::read_to_string(dir.join("note.txt")).unwrap(), "note");

    // 同一个子目录不会再用第二次。
    assert!(!run(&home.0, &upload_command("1-a", Some("shot.png")), b"again").status.success());
    assert_eq!(parse_uploaded("RUNODE-UPLOADED relative"), None);
}

/// 放上传的目录只让自己进，原来别人进得去的也收紧；传过去的目录和文件也只给自己。
#[cfg(unix)]
#[test]
fn uploads_are_private() {
    use std::os::unix::fs::PermissionsExt as _;

    let home = TempDir::new("upload-private");
    let uploads = home.0.join(UPLOADS_DIR);
    fs::create_dir_all(&uploads).unwrap();
    fs::set_permissions(&uploads, fs::Permissions::from_mode(0o755)).unwrap();
    let src = home.0.join("src");
    fs::create_dir_all(src.join("folder")).unwrap();
    fs::write(src.join("folder/inner.txt"), "inner").unwrap();
    let tar = Command::new("tar").arg("-cf").arg("-").arg("-C").arg(&src).arg("folder").output().unwrap();
    let sent = run(&home.0, &upload_command("1-a", None), &tar.stdout);
    assert!(sent.status.success(), "{}", String::from_utf8_lossy(&sent.stderr));
    let dir = std::path::PathBuf::from(parse_uploaded(&String::from_utf8_lossy(&sent.stdout)).unwrap());
    let mode = |path: &std::path::Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!([mode(&uploads), mode(&dir), mode(&dir.join("folder"))], [0o700; 3]);
    assert_eq!(mode(&dir.join("folder/inner.txt")), 0o600);
}

/// 像 Windows 的 OpenSSH 那样把命令行原样交给 cmd 解释，家目录设成 `home`。stdin 先写进文件再交过去：
/// tar.exe 从匿名管道读 zip 时报错，sshd 交来的 stdin 不是匿名管道。
#[cfg(windows)]
fn run_cmd(home: &std::path::Path, command: &str, stdin: &[u8]) -> std::process::Output {
    use std::{os::windows::process::CommandExt as _, process::Stdio};
    let input = home.join(format!("stdin-{}", std::time::SystemTime::UNIX_EPOCH.elapsed().unwrap().as_nanos()));
    std::fs::write(&input, stdin).unwrap();
    let output = Command::new("cmd.exe")
        .raw_arg(format!("/c {command}"))
        .env("USERPROFILE", home)
        .stdin(Stdio::from(std::fs::File::open(&input).unwrap()))
        .output()
        .unwrap();
    std::fs::remove_file(&input).unwrap();
    output
}

/// 用系统的 tar.exe 把 `dir` 里的 `files` 打成 zip。
#[cfg(windows)]
fn zip(dir: &std::path::Path, files: &[&str], out: &std::path::Path) {
    let root = std::env::var_os("SystemRoot").unwrap();
    let tar = std::path::Path::new(&root).join(r"System32\tar.exe");
    let status = Command::new(tar).arg("-a").arg("-cf").arg(out).arg("-C").arg(dir).args(files).status().unwrap();
    assert!(status.success());
}

/// `dir` 里 `files` 的名字和 SHA-256，用系统的 certutil 算。
#[cfg(windows)]
fn windows_sums(dir: &std::path::Path, files: &[&str]) -> Vec<(String, String)> {
    files
        .iter()
        .map(|name| {
            let output = Command::new("certutil").arg("-hashfile").arg(dir.join(name)).arg("SHA256").output().unwrap();
            assert!(output.status.success());
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            let hex: String = text.lines().nth(1).unwrap().chars().filter(|c| !c.is_whitespace()).collect();
            assert_eq!(hex.len(), 64, "{text}");
            ((*name).to_owned(), hex)
        })
        .collect()
}

#[cfg(windows)]
#[test]
fn windows_probes_installs_and_prunes() {
    use std::fs;
    // 家目录名里带空格和 `&`：脚本靠 `cd` 进家目录，路径都相对它写。
    let home = common::TempDir::new("boot strap&x");
    let dir = "0.1.16.test";
    let probe = run_cmd(&home.0, &windows::connect_command(dir), b"");
    assert_eq!(probe.status.code(), Some(runode_ssh::bootstrap::PROBE_EXIT));
    let platform = Platform::parse(&String::from_utf8_lossy(&probe.stdout)).unwrap();
    assert_eq!(platform.target(), Ok(Target::WindowsX86_64));

    // 假的 rn.exe 是系统的 tar.exe：`--version` 时成功退出，`--ssh-bridge` 时报错退出，不打 `RUNODE-PROBE`。
    let staging = home.0.join("staging");
    fs::create_dir(&staging).unwrap();
    fs::copy(r"C:\Windows\System32\tar.exe", staging.join("rn.exe")).unwrap();
    fs::write(staging.join("conpty.dll"), b"dll").unwrap();
    fs::write(staging.join("runode.exe"), b"app").unwrap();
    let files = ["rn.exe", "runode.exe", "conpty.dll"];
    let sums = windows_sums(&staging, &files);
    let install = |part: &str, sums: &[(String, String)], stdin: &[u8]| {
        run_cmd(&home.0, &windows::install_command(dir, part, sums).unwrap(), stdin)
    };
    let bridged = |output: &std::process::Output| {
        output.status.code() != Some(runode_ssh::bootstrap::PROBE_EXIT)
            && !String::from_utf8_lossy(&output.stdout).contains("RUNODE-PROBE")
    };
    // 只有 rn.exe、没有 runode.exe 的包不装。
    let launcher_only = home.0.join("launcher.zip");
    zip(&staging, &["rn.exe", "conpty.dll"], &launcher_only);
    let refused = install(".part-0", &sums, &fs::read(&launcher_only).unwrap());
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("RUNODE-INSTALL exec"), "{stderr}");

    let archive = home.0.join("runode.zip");
    zip(&staging, &files, &archive);
    let bytes = fs::read(&archive).unwrap();
    let cut = install(".part-1", &sums, &bytes[..bytes.len() / 2]);
    assert!(!cut.status.success());
    assert!(String::from_utf8_lossy(&cut.stderr).contains("RUNODE-INSTALL checksum"));

    // 解出来的和本机算的摘要对不上：不装。
    let mut wrong = sums.clone();
    wrong.iter_mut().find(|(name, _)| name == "runode.exe").unwrap().1 = "0".repeat(64);
    let mismatched = install(".part-9", &wrong, &bytes);
    assert!(!mismatched.status.success());
    assert!(String::from_utf8_lossy(&mismatched.stderr).contains("RUNODE-INSTALL checksum"));

    let bin = home.0.join(runode_ssh::bootstrap::BIN_DIR);
    assert!(!bin.join(dir).exists());
    fs::create_dir_all(bin.join("stale")).unwrap();
    fs::write(bin.join("stale/rn.exe"), b"old").unwrap();
    let installed = install(".part-2", &sums, &bytes);
    assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
    assert!(bin.join(dir).join("rn.exe").is_file());
    assert!(bin.join(dir).join("conpty.dll").is_file());
    assert!(bin.join(dir).join(SUMS_FILE).is_file());
    assert!(!bin.join(".part-0").exists());
    assert!(bridged(&run_cmd(&home.0, &windows::connect_command(dir), b"")));
    // 别的连接已经装好了同一个构建：用它的，丢掉这次解出来的。
    let again = install(".part-3", &sums, &bytes);
    assert!(again.status.success(), "{}", String::from_utf8_lossy(&again.stderr));
    assert!(!bin.join(".part-3").exists());
    // `stale` 刚放进去，没满 `BUILD_KEEP_DAYS` 天，留着。
    let mut left: Vec<_> = fs::read_dir(&bin).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    left.sort();
    assert_eq!(left, [dir, "stale"]);

    // 装好后被改坏、没有清单：连上时当作没装，再装时换掉。
    let damages: [fn(&std::path::Path); 3] = [
        |build| fs::write(build.join("runode.exe"), b"ap").unwrap(),
        |build| fs::remove_file(build.join(SUMS_FILE)).unwrap(),
        |build| fs::write(build.join(SUMS_FILE), b"").unwrap(),
    ];
    for damage in damages {
        damage(&bin.join(dir));
        let probe = run_cmd(&home.0, &windows::connect_command(dir), b"");
        assert_eq!(probe.status.code(), Some(runode_ssh::bootstrap::PROBE_EXIT));
        assert!(Platform::parse(&String::from_utf8_lossy(&probe.stdout)).is_some());
        let replaced = install(".part-4", &sums, &bytes);
        assert!(replaced.status.success(), "{}", String::from_utf8_lossy(&replaced.stderr));
        assert!(bridged(&run_cmd(&home.0, &windows::connect_command(dir), b"")));
        let mut left: Vec<_> = fs::read_dir(&bin).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        left.sort();
        assert_eq!(left, [dir, "stale"], "the damaged build is not cleaned up");
    }

    // 坏了的构建里有文件开着、改不了名：不装，说清楚。
    fs::write(bin.join(dir).join(SUMS_FILE), b"").unwrap();
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // 只许别人读：不许删、不许改名。
        let _held = fs::OpenOptions::new().read(true).share_mode(1).open(bin.join(dir).join("conpty.dll")).unwrap();
        let busy = install(".part-5", &sums, &bytes);
        assert!(!busy.status.success());
        assert!(String::from_utf8_lossy(&busy.stderr).contains("RUNODE-INSTALL busy"));
        assert!(bin.join(dir).join("rn.exe").is_file());
        assert!(!bin.join(".part-5").exists());
    }
    assert!(install(".part-6", &sums, &bytes).status.success());

    let output = run_cmd(&home.0, &windows::prune_command(dir), b"");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let pruned = runode_ssh::bootstrap::parse_pruned(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(pruned.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["stale"]);
    let left: Vec<_> = fs::read_dir(&bin).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(left, [dir]);

    let missing = run_cmd(&home.0, &windows::run_command("0.1.17.next", Entry::Project), b"");
    assert_eq!(missing.status.code(), Some(runode_ssh::bootstrap::PROBE_EXIT));
}

#[cfg(windows)]
#[test]
fn windows_uploads_a_tar_into_a_fresh_directory() {
    use std::fs;
    let home = common::TempDir::new("up load&x");
    let src = home.0.join("src");
    fs::create_dir_all(src.join("folder")).unwrap();
    fs::write(src.join("folder/inner.txt"), "inner").unwrap();
    let tar = Command::new("tar").arg("-cf").arg("-").arg("-C").arg(&src).arg("folder").output().unwrap();
    let output = run_cmd(&home.0, &windows::upload_command("1-a"), &tar.stdout);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let dir = windows::parse_uploaded(&String::from_utf8_lossy(&output.stdout)).unwrap();
    let expected = home.0.join(runode_ssh::bootstrap::UPLOADS_DIR).join("1-a");
    assert_eq!(dir, expected.to_string_lossy().replace('\\', "/"));
    assert_eq!(fs::read_to_string(std::path::Path::new(&dir).join("folder/inner.txt")).unwrap(), "inner");
    assert!(!run_cmd(&home.0, &windows::upload_command("1-a"), &tar.stdout).status.success());
}
