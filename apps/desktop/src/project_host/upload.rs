//! 经读写项目的连接把本机的文件传到远端，好交给那里的 agent：配对的电脑没有 ssh，截图粘贴、把文件拖进那边的终端
//! 时走这里。和 SSH 主机一样放在那边家目录下的 `UPLOADS_DIR` 里，每次一个新的子目录，同名的分进不同的子目录。

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow};
use runode_files::ops::OpError;
use runode_ssh::bootstrap::UPLOADS_DIR;

use super::ProjectHost;
use crate::host_client::ssh::upload::{Upload, upload_id};

/// 传到 `project` 所在的那台机器上，`home` 是那边的家目录（用 `/` 分隔）；交回各自在那边的绝对路径，顺序同
/// `Upload::Files`。目录连同里面的一起传，里面的符号链接跳过。会阻塞，在后台线程上调。
pub(crate) fn upload(project: &ProjectHost, home: &Path, upload: Upload) -> Result<Vec<String>> {
    match upload {
        Upload::Bytes { name, bytes } => {
            let dir = new_dir(project, home)?;
            let path = child(&dir, &name);
            write(project, &path, &bytes)?;
            Ok(vec![text(&path)])
        }
        Upload::Files(paths) => {
            let mut uploaded = vec![String::new(); paths.len()];
            for batch in batches(&paths) {
                let dir = new_dir(project, home)?;
                for ix in batch {
                    let local = &paths[ix];
                    let remote = child(&dir, &file_name(local));
                    send(project, local, &remote)?;
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

/// 在那边新建这次上传的子目录。
fn new_dir(project: &ProjectHost, home: &Path) -> Result<PathBuf> {
    let dir = child(&child(home, UPLOADS_DIR), &upload_id());
    project.create(&dir, true).map_err(|err| op_error(&dir, err))?;
    Ok(dir)
}

/// 本机的 `local` 传成那边的 `remote`：文件写过去，目录建出来再一项项传。
fn send(project: &ProjectHost, local: &Path, remote: &Path) -> Result<()> {
    let meta = fs::metadata(local).with_context(|| format!("cannot read {}", local.display()))?;
    if !meta.is_dir() {
        let bytes = fs::read(local).with_context(|| format!("cannot read {}", local.display()))?;
        return write(project, remote, &bytes);
    }
    project.create(remote, true).map_err(|err| op_error(remote, err))?;
    let entries = fs::read_dir(local).with_context(|| format!("cannot read {}", local.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", local.display()))?;
        if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
            continue;
        }
        send(project, &entry.path(), &child(remote, &entry.file_name().to_string_lossy()))?;
    }
    Ok(())
}

fn write(project: &ProjectHost, path: &Path, bytes: &[u8]) -> Result<()> {
    project.replace_file(path, bytes).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

fn op_error(path: &Path, err: OpError) -> anyhow::Error {
    match err {
        OpError::Io(err) => anyhow!("cannot create {}: {err}", path.display()),
        _ => anyhow!("cannot create {}", path.display()),
    }
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

    use super::{batches, child, upload};
    use crate::host_client::ssh::upload::Upload;

    /// 经项目服务传：内容存成文件，同名的两个文件各进一个子目录，目录连同里面的一起传。
    #[test]
    fn uploads_through_the_project_service() {
        let root = std::env::temp_dir().join(format!("runode-upload-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (home, local) = (root.join("home"), root.join("local"));
        for dir in ["a", "b", "c/inner"] {
            fs::create_dir_all(local.join(dir)).unwrap();
        }
        fs::create_dir_all(&home).unwrap();
        fs::write(local.join("a/same.txt"), "from a").unwrap();
        fs::write(local.join("b/same.txt"), "from b").unwrap();
        fs::write(local.join("c/inner/deep.txt"), "deep").unwrap();
        let project = super::super::tests::remote();
        let files = vec![local.join("a/same.txt"), local.join("b/same.txt"), local.join("c")];
        let paths = upload(&project, &home, Upload::Files(files)).unwrap();
        assert_ne!(paths[0], paths[1]);
        let read = |path: &str| fs::read_to_string(path).unwrap();
        assert_eq!((read(&paths[0]), read(&paths[1])), ("from a".into(), "from b".into()));
        assert_eq!(read(&format!("{}/inner/deep.txt", paths[2])), "deep");
        assert!(paths.iter().all(|path| Path::new(path).starts_with(home.join(runode_ssh::bootstrap::UPLOADS_DIR))));
        let shot = upload(&project, &home, Upload::Bytes { name: "shot.png".into(), bytes: b"png".to_vec() }).unwrap();
        assert_eq!(read(&shot[0]), "png");
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
