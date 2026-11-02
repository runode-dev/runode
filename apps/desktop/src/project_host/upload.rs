//! 经读写项目的连接把本机的文件传到远端，好交给那里的 agent：配对的电脑没有 ssh，截图粘贴、把文件拖进那边的终端
//! 时走这里。放在那边 home 目录下的 `UPLOADS_DIR` 里，每次一个新的子目录，同名的分进不同的子目录。目录由那边
//! 定、由那边建（`UploadOp`），只让自己进：截图和屏幕上的文字不能让那台机器上别的用户读到。

use std::{
    collections::HashSet,
    fs,
    io::{self, Write as _},
    path::{Component, Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use super::{
    ProjectHost,
    client::Client,
    wire::{Call, Reply},
};
use crate::host_client::ssh::upload::{Upload, private_dir, upload_id};

/// 上传的每个请求最多等这么久回话。
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// 远端为上传做的一步。路径都要在放上传的目录里。
#[derive(Debug, Serialize, Deserialize)]
pub(super) enum UploadOp {
    /// 新建这次上传的子目录，交回它（用 `/` 分隔）。
    Begin,
    /// 在这次上传里新建目录。
    Dir { path: PathBuf },
    /// 把请求后面跟着的字节写成这次上传里的新文件 `path`。
    File { path: PathBuf },
}

impl UploadOp {
    pub(super) fn for_each_path(&mut self, f: &mut dyn FnMut(&mut PathBuf)) {
        match self {
            Self::Begin => {}
            Self::Dir { path } | Self::File { path } => f(path),
        }
    }

    /// 在远端做，`root` 是放上传的目录（没有 home 目录时为空），`content` 是请求后面跟着的字节。`Begin` 交回
    /// 新建的子目录。新建的目录只有自己能进，文件只有自己能读写。
    pub(super) fn run(self, root: Option<&Path>, content: &[u8]) -> io::Result<Option<PathBuf>> {
        let root = root.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
        match self {
            Self::Begin => {
                runode_paths::create_private(root)?;
                let dir = root.join(upload_id());
                private_dir(&dir)?;
                Ok(Some(slashed(dir)))
            }
            Self::Dir { path } => {
                within(root, &path)?;
                private_dir(&path).map(|()| None)
            }
            Self::File { path } => {
                within(root, &path)?;
                new_file(&path)?.write_all(content).map(|()| None)
            }
        }
    }
}

/// `path` 在 `root` 的某个子目录里（不是子目录本身），中间没有 `..` 这类跳出去的。
fn within(root: &Path, path: &Path) -> io::Result<()> {
    let rest = path.strip_prefix(root).ok();
    let nested = rest.is_some_and(|rest| {
        rest.components().all(|part| matches!(part, Component::Normal(_))) && rest.components().count() >= 2
    });
    if nested {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not in an upload", path.display())))
    }
}

/// 新建只有自己能读写的文件 `path`；已经有了就失败。
fn new_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Windows 上的路径分隔符换成 `/`：桌面那头按 `/` 拼路径、打进终端。
fn slashed(path: PathBuf) -> PathBuf {
    if cfg!(windows) { PathBuf::from(text(&path).replace('\\', "/")) } else { path }
}

/// 传到 `project` 所在的那台机器上；交回各自在那边的绝对路径，顺序同 `Upload::Files`。目录连同里面的一起传，
/// 里面的符号链接跳过。会阻塞，在后台线程上调。
pub(crate) fn upload(project: &ProjectHost, upload: Upload) -> Result<Vec<String>> {
    let ProjectHost::Remote(client) = project else {
        bail!("this computer is not remote");
    };
    match upload {
        Upload::Bytes { name, bytes } => {
            let dir = begin(client)?;
            let path = child(&dir, &name);
            write(client, &path, &bytes)?;
            Ok(vec![text(&path)])
        }
        Upload::Files(paths) => {
            let mut uploaded = vec![String::new(); paths.len()];
            for batch in batches(&paths) {
                let dir = begin(client)?;
                for ix in batch {
                    let local = &paths[ix];
                    let remote = child(&dir, &file_name(local));
                    send(client, local, &remote)?;
                    uploaded[ix] = text(&remote);
                }
            }
            Ok(uploaded)
        }
    }
}

/// 把 `paths` 按名字分批：一批里没有同名的，按先后。交回各批里的下标。
fn batches(paths: &[PathBuf]) -> Vec<Vec<usize>> {
    let mut left: Vec<usize> = (0..paths.len()).collect();
    let mut batches = Vec::new();
    while !left.is_empty() {
        let mut names = HashSet::new();
        let (batch, rest): (Vec<usize>, Vec<usize>) =
            left.into_iter().partition(|&ix| names.insert(file_name(&paths[ix])));
        batches.push(batch);
        left = rest;
    }
    batches
}

fn call(client: &Client, op: UploadOp, content: &[u8]) -> Result<Option<PathBuf>> {
    match client.call_with(Call::Upload(op), content, UPLOAD_TIMEOUT, None).map_err(|err| anyhow!(err))? {
        (Reply::Upload(path), _) => Ok(path),
        (other, _) => Err(anyhow!(super::unexpected(&other))),
    }
}

/// 让那边新建这次上传的子目录。
fn begin(client: &Client) -> Result<PathBuf> {
    call(client, UploadOp::Begin, &[])
        .and_then(|dir| dir.ok_or_else(|| anyhow!("no upload directory")))
        .context("cannot start the upload")
}

/// 本机的 `local` 传成那边的 `remote`：文件写过去，目录建出来再一项项传。
fn send(client: &Client, local: &Path, remote: &Path) -> Result<()> {
    let meta = fs::metadata(local).with_context(|| format!("cannot read {}", local.display()))?;
    if !meta.is_dir() {
        let bytes = fs::read(local).with_context(|| format!("cannot read {}", local.display()))?;
        return write(client, remote, &bytes);
    }
    call(client, UploadOp::Dir { path: remote.to_path_buf() }, &[])
        .with_context(|| format!("cannot create {}", remote.display()))?;
    let entries = fs::read_dir(local).with_context(|| format!("cannot read {}", local.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", local.display()))?;
        if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
            continue;
        }
        send(client, &entry.path(), &child(remote, &entry.file_name().to_string_lossy()))?;
    }
    Ok(())
}

fn write(client: &Client, path: &Path, bytes: &[u8]) -> Result<()> {
    call(client, UploadOp::File { path: path.to_path_buf() }, bytes)
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

/// 远端目录 `dir` 下的 `name`，一律按 `/` 拼：Windows 远端也认 `/`，Windows 上的 `Path::join` 插的却是 `\`。
fn child(dir: &Path, name: &str) -> PathBuf {
    let dir = text(dir);
    PathBuf::from(format!("{}/{name}", dir.trim_end_matches('/')))
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| "upload".into(), |name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use super::{UploadOp, batches, child, upload};
    use crate::{host_client::ssh::upload::Upload, project_host::tests::remote_uploading_to};

    /// 这个测试自己的空目录。
    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("runode-upload-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// 经项目服务传：内容存成文件，同名的两个文件各进一个子目录，目录连同里面的一起传。
    #[test]
    fn uploads_through_the_project_service() {
        let root = scratch("files");
        let (uploads, local) = (root.join("home/.runode/uploads"), root.join("local"));
        for dir in ["a", "b", "c/inner"] {
            fs::create_dir_all(local.join(dir)).unwrap();
        }
        fs::write(local.join("a/same.txt"), "from a").unwrap();
        fs::write(local.join("b/same.txt"), "from b").unwrap();
        fs::write(local.join("c/inner/deep.txt"), "deep").unwrap();
        let project = remote_uploading_to(uploads.clone());
        let files = vec![local.join("a/same.txt"), local.join("b/same.txt"), local.join("c")];
        let paths = upload(&project, Upload::Files(files)).unwrap();
        assert_ne!(paths[0], paths[1]);
        let read = |path: &str| fs::read_to_string(path).unwrap();
        assert_eq!((read(&paths[0]), read(&paths[1])), ("from a".into(), "from b".into()));
        assert_eq!(read(&format!("{}/inner/deep.txt", paths[2])), "deep");
        assert!(paths.iter().all(|path| Path::new(path).starts_with(&uploads)));
        let shot = upload(&project, Upload::Bytes { name: "shot.png".into(), bytes: b"png".to_vec() }).unwrap();
        assert_eq!(read(&shot[0]), "png");
        let _ = fs::remove_dir_all(&root);
    }

    /// 放上传的目录、每次上传的子目录和传过去的目录只有自己能进，文件只有自己能读写；原来不是这样的放上传的
    /// 目录也收紧。
    #[cfg(unix)]
    #[test]
    fn uploads_are_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = scratch("private");
        let uploads = root.join("uploads");
        fs::create_dir_all(&uploads).unwrap();
        fs::set_permissions(&uploads, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir_all(root.join("local/folder")).unwrap();
        fs::write(root.join("local/folder/inner.txt"), "inner").unwrap();
        let paths =
            upload(&remote_uploading_to(uploads.clone()), Upload::Files(vec![root.join("local/folder")])).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let folder = PathBuf::from(&paths[0]);
        let dirs = [uploads.as_path(), folder.parent().unwrap(), &folder];
        assert!(dirs.iter().all(|dir| mode(dir) == 0o700), "{:?}", dirs.map(mode));
        assert_eq!(mode(&folder.join("inner.txt")), 0o600);
        let _ = fs::remove_dir_all(&root);
    }

    /// 那边只在放上传的目录的子目录里建东西，路径跳不出去。
    #[test]
    fn uploads_stay_inside_the_upload_directory() {
        let root = scratch("inside");
        let uploads = root.join("uploads");
        let dir = UploadOp::Begin.run(Some(&uploads), &[]).unwrap().unwrap();
        let write = |path: PathBuf| UploadOp::File { path }.run(Some(&uploads), b"x");
        assert!(write(child(&dir, "ok.txt")).is_ok());
        assert!(write(uploads.join("loose.txt")).is_err());
        assert!(write(dir.join("../../escaped.txt")).is_err());
        assert!(write(root.join("elsewhere.txt")).is_err());
        assert!(!root.join("escaped.txt").exists() && !root.join("elsewhere.txt").exists());
        assert!(UploadOp::Begin.run(None, &[]).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn same_names_go_to_separate_batches() {
        let paths: Vec<PathBuf> = ["a/x.png", "b/x.png", "c/y.png", "d/x.png"].iter().map(PathBuf::from).collect();
        assert_eq!(batches(&paths), vec![vec![0, 2], vec![1], vec![3]]);
        assert!(batches(&[]).is_empty());
    }

    #[test]
    fn remote_paths_use_slashes() {
        assert_eq!(child(Path::new("/home/me"), ".runode/uploads"), PathBuf::from("/home/me/.runode/uploads"));
        assert_eq!(child(Path::new("C:/Users/me/"), "shot.png"), PathBuf::from("C:/Users/me/shot.png"));
    }
}
