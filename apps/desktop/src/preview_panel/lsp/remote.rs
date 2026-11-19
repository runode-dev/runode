//! 项目在 SSH 主机上时语言服务器跑在那台主机上：桌面经 ssh 起那边的 `runode --ssh-lsp <程序> <参数…>`
//! （`spawn`），它在那个用户登录 shell 的 PATH 里找到程序，把自己的 stdin、stdout 交给它（`run`）。

use std::{
    ffi::OsString,
    io,
    process::{Command, Stdio},
    sync::{Arc, mpsc},
    thread,
};

use runode_lsp::servers;
use runode_ssh::{
    bootstrap::{Entry, Family},
    process::{Pipes, spawn as spawn_piped},
};

use super::{EXIT_POLL, EXIT_POLLS, Process};
use crate::{
    host_client::{
        hosts::{HostId, Status},
        ssh::{self, RemoteHost, deploy},
    },
    project_host::ProjectHost,
};

/// `run` 在这台电脑上没找到要起的程序时的退出码。
pub(super) const NOT_INSTALLED: i32 = 127;

/// `runode --ssh-lsp`：起 `command`（程序名和参数），把 stdin 转给它、stdout 原样交给它，等它退出；返回退出码。
/// stdin 到头（桌面那头断了）以后它还不退的结束掉：有的语言服务器不因为 stdin 关了就退出。
pub(crate) fn run(command: &[OsString]) -> i32 {
    let Some((program, args)) = command.split_first() else {
        return 1;
    };
    let Some((program, path)) = super::locate(&program.to_string_lossy()) else {
        return NOT_INSTALLED;
    };
    let Ok(mut child) = Command::new(program).args(args).env("PATH", path).stdin(Stdio::piped()).spawn() else {
        return 1;
    };
    let (ended, end) = mpsc::channel();
    if let Some(mut input) = child.stdin.take() {
        thread::spawn(move || {
            io::copy(&mut io::stdin().lock(), &mut input).ok();
            drop(input);
            ended.send(()).ok();
        });
    }
    let mut polls_left = None;
    loop {
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => return status.code().unwrap_or(1),
            Err(_) => return 1,
        }
        if polls_left.is_none() && end.try_recv().is_ok() {
            polls_left = Some(EXIT_POLLS);
        }
        match &mut polls_left {
            Some(0) => {
                child.kill().ok();
                child.wait().ok();
                return 1;
            }
            Some(left) => *left -= 1,
            None => {}
        }
        thread::sleep(EXIT_POLL);
    }
}

/// `project` 所在的、能在上面起语言服务器的 SSH 主机：连着的 Unix 主机。
pub(super) fn host(project: &ProjectHost) -> Option<Arc<RemoteHost>> {
    let ProjectHost::Remote(client) = project else {
        return None;
    };
    let HostId::Ssh(name) = client.host() else {
        return None;
    };
    ssh::find(name).filter(|host| host.family() == Family::Unix && matches!(host.status(), Status::Connected(_)))
}

/// 经 ssh 在 `host` 上起 `spec`。要认证时不问用户，直接失败。
pub(super) fn spawn(host: &RemoteHost, spec: servers::Server) -> io::Result<Process> {
    let entry = Entry::Lsp { program: spec.program, args: spec.args };
    let script = host.family().run_command(&deploy::install_dir(), entry);
    let mut command = host.exec(&ssh::ssh(), &script, false, false);
    let piped = spawn_piped(&mut command, Pipes { stdin: true, stdout: true, stderr: false })?;
    let (Some(input), Some(output)) = (piped.stdin, piped.stdout) else {
        return Err(io::Error::other("ssh gave no pipes"));
    };
    Ok(Process { child: piped.child, input: Box::new(input), output: Box::new(output) })
}

#[cfg(all(test, unix))]
mod tests {
    use std::{os::unix::fs::PermissionsExt as _, path::Path};

    use super::*;
    use crate::host_client::ssh::tests::FakeRemote;

    /// 经 ssh 起远端的语言服务器、握上手；那边说没装这个程序时算没装，不算出错。远端的 runode 换成脚本：
    /// 只认 `--ssh-lsp rust-analyzer`，读到请求就答一个握手的应答。`make test-ssh` 编好 runode 再跑它。
    #[test]
    #[ignore = "runs a built runode over a fake ssh; make test-ssh"]
    fn starts_a_language_server_over_a_fake_ssh() {
        assert!(std::env::var_os("RUNODE_SSH_BINARY").is_some(), "RUNODE_SSH_BINARY points at a built runode");
        let fake = FakeRemote::new("fake-ssh-lsp");
        let status = fake.connected();
        assert!(matches!(status, Status::Connected(_)), "not connected: {status:?}");
        let installed = fake.home.join(runode_ssh::bootstrap::BIN_DIR).join(deploy::install_dir()).join("runode");
        // 跑着的宿主用的是原来那个文件，换名盖上不碰它。
        let replace = |script: &str| {
            let part = installed.with_extension("part");
            std::fs::write(&part, script).unwrap();
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::rename(&part, &installed).unwrap();
        };
        let start = || {
            let spec = servers::for_path(Path::new("a.rs")).expect("a server for Rust");
            let (diagnostics, _kept) = futures::channel::mpsc::unbounded();
            super::super::start(spec, Path::new("/p/src/a.rs"), Some("/p".into()), Some(fake.host.clone()), diagnostics)
                .expect("the handshake")
        };

        let reply =
            r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"completionProvider":{"triggerCharacters":["."]}}}}"#;
        replace(&format!(
            concat!(
                "#!/bin/sh\n",
                "[ \"$*\" = \"--ssh-lsp rust-analyzer\" ] || exit 2\n",
                "read -r header\n",
                "printf 'Content-Length: %s\\r\\n\\r\\n%s' {} '{}'\n",
                "cat >/dev/null\n",
            ),
            reply.len(),
            reply
        ));
        assert_eq!(start().expect("a running server").triggers, ["."]);
        replace(&format!("#!/bin/sh\nexit {NOT_INSTALLED}\n"));
        assert!(start().is_none());
    }
}
