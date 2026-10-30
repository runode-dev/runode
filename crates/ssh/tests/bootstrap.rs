//! 远端脚本：在本机用 /bin/sh 真跑一遍，探测、安装、换链接都按约定来。

mod common;

use std::{
    fs,
    io::Write as _,
    process::{Command, Stdio},
};

use common::TempDir;
use runode_ssh::bootstrap::{
    BIN_DIR, Libc, Os, PROBE_EXIT, Platform, Target, Unsupported, connect_command, install_command, install_dir_name,
};

/// 像 ssh 那样把命令行交给登录 shell（默认 /bin/sh）解释。
fn run(home: &std::path::Path, command: &str, stdin: &[u8]) -> std::process::Output {
    run_in("/bin/sh", home, command, stdin)
}

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
    for command in [connect_command("0.1.16.abc"), install_command("0.1.16.abc", &"0".repeat(64), true)] {
        let inner = command.strip_prefix("/bin/sh -c '").and_then(|rest| rest.strip_suffix('\'')).unwrap();
        assert!(!inner.contains(['\'', '!', '\n']), "{inner}");
        assert!(Command::new("/bin/sh").args(["-n", "-c", inner]).status().unwrap().success());
    }
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
