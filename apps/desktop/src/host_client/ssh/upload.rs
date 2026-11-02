//! 把本机的文件传到远端主机，好交给那里的 agent：截图粘贴、把文件拖进远端的终端时用。有主连接时复用它，
//! 不另外认证。

use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow, bail};
use runode_ssh::{
    bootstrap::{Family, parse_uploaded, upload_command, windows},
    process::{Piped, Pipes},
};

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
        // Windows 远端只收 tar 包：先落成本机的临时文件，再按文件传。临时目录新建、只给自己进：Linux 的
        // 临时目录别的用户也能进，截图和屏幕上的文字不能让他们读到。
        Upload::Bytes { name, bytes } if host.family() == Family::Windows => {
            let dir = std::env::temp_dir().join(format!("runode-upload-{}", upload_id()));
            private_dir(&dir).context("cannot stage the upload")?;
            let file = dir.join(&name);
            let sent = std::fs::write(&file, bytes)
                .context("cannot stage the upload")
                .and_then(|()| self::upload(host, Upload::Files(vec![file])));
            let _ = std::fs::remove_dir_all(&dir);
            sent
        }
        Upload::Bytes { name, bytes } => {
            let dir = send(host, Some(&name), &mut bytes.as_slice())?;
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
                let mut stdout = tar.stdout.take().context("no tar output")?;
                let dir = send(host, None, &mut stdout);
                // 没读完就关掉，tar 才不会一直卡在写上。
                drop(stdout);
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

/// 新建只有自己能进的目录 `dir`；已经有了（包括同名的链接）就失败。
fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700).create(dir);
    #[cfg(not(unix))]
    std::fs::create_dir(dir)
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
        // 写成 `./名字`：以 `-` 开头的名字不被当成 tar 的选项。
        command.arg("-C").arg(parent).arg(Path::new(".").join(path.file_name().context("no file name")?));
    }
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::subprocess::hide_window(&mut command);
    command.spawn().context("cannot run tar")
}

/// 跑一次 `upload_command`，把 `input` 读到头写给它，回远端放上传的目录。Windows 远端的 `input` 只能是 tar 包。
fn send(host: &RemoteHost, file: Option<&str>, input: &mut dyn Read) -> Result<String> {
    let family = host.family();
    let script = match family {
        Family::Unix => upload_command(&upload_id(), file),
        Family::Windows => windows::upload_command(&upload_id()),
    };
    let mut command = host.exec(&ssh(), &script, true, false);
    let piped = runode_ssh::process::spawn(&mut command, Pipes { stdin: true, stdout: true, stderr: true })
        .context("cannot run ssh")?;
    let Piped { mut child, stdin, stdout, stderr } = piped;
    // stderr 另起线程读：远端报的字写满管道时 ssh 就不再读 stdin，下面的写会一直卡着。
    let err = stderr.map(|mut stderr| {
        std::thread::spawn(move || {
            let mut err = String::new();
            let _ = stderr.read_to_string(&mut err);
            err
        })
    });
    let sent = stdin
        .context("no ssh input")
        .and_then(|mut stdin| std::io::copy(input, &mut stdin).context("the upload was cut off"));
    let mut out = String::new();
    if let Some(mut stdout) = stdout {
        let _ = stdout.read_to_string(&mut out);
    }
    let err = err.and_then(|reader| reader.join().ok()).unwrap_or_default();
    let status = child.wait().context("ssh failed")?;
    let dir = match family {
        Family::Unix => parse_uploaded(&out).map(str::to_owned),
        Family::Windows => windows::parse_uploaded(&out),
    };
    match dir {
        Some(dir) if status.success() && sent.is_ok() => Ok(dir),
        _ => match runode_ssh::last_line(&err) {
            "" => Err(sent.err().unwrap_or_else(|| anyhow!("ssh failed"))),
            line => bail!("{line}"),
        },
    }
}

/// 这次上传在远端的子目录名：秒数、进程号加计数，同一台电脑上不会重。
pub(crate) fn upload_id() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
    format!("{secs}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}
