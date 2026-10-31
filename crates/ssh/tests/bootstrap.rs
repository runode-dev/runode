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
use runode_ssh::bootstrap::{BIN_DIR, PROBE_EXIT, UPLOADS_DIR, parse_pruned, parse_uploaded};
use runode_ssh::bootstrap::{
    Libc, Os, Platform, Target, Unsupported, connect_command, install_command, install_dir_name, project_command,
    prune_command, screen_command, service_command, upload_command, windows,
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
        project_command("0.1.16.abc"),
        screen_command("0.1.16.abc"),
        service_command("0.1.16.abc"),
        prune_command("0.1.16.abc"),
    ] {
        let inner = command.strip_prefix("/bin/sh -c '").and_then(|rest| rest.strip_suffix('\'')).unwrap();
        assert!(!inner.contains(['\'', '!', '\n']), "{inner}");
        if cfg!(unix) {
            assert!(Command::new("/bin/sh").args(["-n", "-c", inner]).status().unwrap().success());
        }
    }
    for command in [
        windows::connect_command("0.1.16.abc"),
        windows::install_command("0.1.16.abc", ".part-1"),
        windows::upload_command("1-2"),
        windows::project_command("0.1.16.abc"),
        windows::screen_command("0.1.16.abc"),
    ] {
        let inner = command.strip_prefix("cmd /d /c \"").and_then(|rest| rest.strip_suffix('"')).unwrap();
        assert!(!inner.contains(['"', '$', '`', '\n']), "{inner}");
    }
    assert!(
        windows::prune_command("0.1.16.abc")
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b" -+/=".contains(&byte))
    );
    assert_eq!(install_dir_name("0.1.16.abc-dirty.x/y"), "0.1.16.abc-dirty.x_y");
    assert_eq!(install_dir_name(".."), "_..");
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

    let bridged = run(&home.0, &connect_command(dir), b"");
    assert_eq!(String::from_utf8_lossy(&bridged.stdout), "bridged\n");
    let other = run(&home.0, &connect_command("0.1.17.next"), b"");
    let platform = Platform::parse(&String::from_utf8_lossy(&other.stdout)).unwrap();
    assert_eq!(platform.installed, [dir, "runode"]);
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
    assert!(home.0.join(BIN_DIR).join("v1/runode").is_file());
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
    assert_eq!(platform(Os::Windows, "ARM64", Libc::Unknown).target(), Err(Unsupported::Arch("ARM64".into())));
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
    let status = Command::new("tar").arg("-a").arg("-cf").arg(out).arg("-C").arg(dir).args(files).status().unwrap();
    assert!(status.success());
}

#[cfg(windows)]
#[test]
fn windows_probes_installs_and_prunes() {
    use std::fs;
    // 家目录名里带空格：脚本靠 `cd` 进家目录，路径都相对它写。
    let home = common::TempDir::new("boot strap");
    let dir = "0.1.16.test";
    let probe = run_cmd(&home.0, &windows::connect_command(dir), b"");
    assert_eq!(probe.status.code(), Some(runode_ssh::bootstrap::PROBE_EXIT));
    let platform = Platform::parse(&String::from_utf8_lossy(&probe.stdout)).unwrap();
    assert_eq!(platform.target(), Ok(Target::WindowsX86_64));

    // 假的 rn.exe 是系统的 tar.exe：`--version` 时成功退出。
    let staging = home.0.join("staging");
    fs::create_dir(&staging).unwrap();
    fs::copy(r"C:\Windows\System32\tar.exe", staging.join("rn.exe")).unwrap();
    fs::write(staging.join("conpty.dll"), b"dll").unwrap();
    // 只有 rn.exe、没有 runode.exe 的包不装。
    let launcher_only = home.0.join("launcher.zip");
    zip(&staging, &["rn.exe", "conpty.dll"], &launcher_only);
    let refused = run_cmd(&home.0, &windows::install_command(dir, ".part-0"), &fs::read(&launcher_only).unwrap());
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("RUNODE-INSTALL exec"));

    fs::write(staging.join("runode.exe"), b"app").unwrap();
    let archive = home.0.join("runode.zip");
    zip(&staging, &["rn.exe", "runode.exe", "conpty.dll"], &archive);
    let bytes = fs::read(&archive).unwrap();
    let cut = run_cmd(&home.0, &windows::install_command(dir, ".part-1"), &bytes[..bytes.len() / 2]);
    assert!(!cut.status.success());
    assert!(String::from_utf8_lossy(&cut.stderr).contains("RUNODE-INSTALL checksum"));

    let bin = home.0.join(runode_ssh::bootstrap::BIN_DIR);
    fs::create_dir_all(bin.join("stale")).unwrap();
    fs::write(bin.join("stale/rn.exe"), b"old").unwrap();
    let installed = run_cmd(&home.0, &windows::install_command(dir, ".part-2"), &bytes);
    assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
    assert!(bin.join(dir).join("rn.exe").is_file());
    assert!(bin.join(dir).join("conpty.dll").is_file());
    assert!(!bin.join(".part-0").exists());
    // 别的连接已经装好了同一个构建：用它的，丢掉这次解出来的。
    let again = run_cmd(&home.0, &windows::install_command(dir, ".part-3"), &bytes);
    assert!(again.status.success(), "{}", String::from_utf8_lossy(&again.stderr));
    assert!(!bin.join(".part-3").exists());
    // `stale` 刚放进去，没满 `BUILD_KEEP_DAYS` 天，留着。
    let mut left: Vec<_> = fs::read_dir(&bin).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    left.sort();
    assert_eq!(left, [dir, "stale"]);

    let output = run_cmd(&home.0, &windows::prune_command(dir), b"");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let pruned = runode_ssh::bootstrap::parse_pruned(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(pruned.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["stale"]);
    let left: Vec<_> = fs::read_dir(&bin).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(left, [dir]);

    let missing = run_cmd(&home.0, &windows::project_command("0.1.17.next"), b"");
    assert_eq!(missing.status.code(), Some(runode_ssh::bootstrap::PROBE_EXIT));
}

#[cfg(windows)]
#[test]
fn windows_uploads_a_tar_into_a_fresh_directory() {
    use std::fs;
    let home = common::TempDir::new("up load");
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
