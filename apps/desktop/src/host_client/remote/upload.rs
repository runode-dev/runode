//! 把本机的文件传到远端主机，好交给那里的 agent：截图粘贴、把文件拖进远端的终端时用。有主连接时复用它，
//! 不另外认证。

use std::{
    collections::HashSet,
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow, bail};
use runode_ssh::bootstrap::{parse_uploaded, upload_command};

use super::{RemoteHost, ssh};

/// 要传的东西。
pub enum Upload {
    /// 本机的文件或目录，目录连同里面的一起传。
    Files(Vec<PathBuf>),
    /// 内存里的一份内容，在远端存成 `name`：只能有字母、数字和 `.`、`-`、`_`。
    Bytes { name: String, bytes: Vec<u8> },
}

/// 传到 `host` 上，回各自在远端的绝对路径，顺序同 `Upload::Files`。会阻塞，在后台线程上调。
pub fn upload(host: &RemoteHost, upload: Upload) -> Result<Vec<String>> {
    match upload {
        Upload::Bytes { name, bytes } => {
            let dir = send(host, Some(&name), Stdio::piped(), Some(&bytes))?;
            Ok(vec![format!("{dir}/{name}")])
        }
        Upload::Files(paths) => {
            let mut uploaded = vec![String::new(); paths.len()];
            // 一个子目录里放不下两个同名的，同名的分几次传。
            let mut left: Vec<usize> = (0..paths.len()).collect();
            while !left.is_empty() {
                let mut names = HashSet::new();
                let (batch, rest): (Vec<usize>, Vec<usize>) =
                    left.into_iter().partition(|&ix| names.insert(file_name(&paths[ix])));
                let mut tar = tar(batch.iter().map(|&ix| paths[ix].as_path()))?;
                let stdout = tar.stdout.take().context("no tar output")?;
                let dir = send(host, None, Stdio::from(stdout), None);
                let packed = tar.wait();
                let dir = dir?;
                if !packed.is_ok_and(|status| status.success()) {
                    bail!("cannot pack {}", paths[batch[0]].display());
                }
                for ix in batch {
                    uploaded[ix] = format!("{dir}/{}", file_name(&paths[ix]));
                }
                left = rest;
            }
            Ok(uploaded)
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| "upload".into(), |name| name.to_string_lossy().into_owned())
}

/// 把 `paths` 打成 tar 写到 stdout，各自只带最后一级名字。
fn tar<'a>(paths: impl Iterator<Item = &'a Path>) -> Result<std::process::Child> {
    let mut command = Command::new("tar");
    command.arg("-cf").arg("-");
    // macOS 的 tar 默认带上扩展属性和 `._` 开头的资源文件，远端用不上，GNU tar 解的时候还会报警告。
    if cfg!(target_os = "macos") {
        command.env("COPYFILE_DISABLE", "1").args(["--no-mac-metadata", "--no-xattrs"]);
    }
    for path in paths {
        let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new("."));
        command.arg("-C").arg(parent).arg(path.file_name().context("no file name")?);
    }
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::subprocess::hide_window(&mut command);
    command.spawn().context("cannot run tar")
}

/// 跑一次 `upload_command`，stdin 是 `stdin`（`bytes` 给了时写进去），回远端放上传的目录。
fn send(host: &RemoteHost, file: Option<&str>, stdin: Stdio, bytes: Option<&[u8]>) -> Result<String> {
    let ssh = ssh();
    let mut command = ssh.exec(&host.dest, &upload_command(&upload_id(), file), ssh.control.is_some());
    if ssh.control.is_none() {
        host.ask_through(&mut command);
    }
    command.stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::subprocess::hide_window(&mut command);
    let mut child = command.spawn().context("cannot run ssh")?;
    if let Some(bytes) = bytes {
        let mut input = child.stdin.take().context("no ssh input")?;
        std::io::Write::write_all(&mut input, bytes).context("the upload was cut off")?;
    }
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    let output = child.wait_with_output().context("ssh failed")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_uploaded(&stdout) {
        Some(dir) if output.status.success() => Ok(dir.to_owned()),
        _ => {
            let line = runode_ssh::last_line(&stderr);
            Err(anyhow!("{}", if line.is_empty() { "ssh failed" } else { line }))
        }
    }
}

/// 这次上传在远端的子目录名：秒数、进程号加计数，同一台电脑上不会重。
fn upload_id() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
    format!("{secs}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}
