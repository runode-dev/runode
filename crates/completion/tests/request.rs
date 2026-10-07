//! 按 Tab 的补全请求：光标处的候选、命令名、动态补全的生成器和有规格的常见命令。

use std::{path::Path, sync::Arc};

use runode_completion::{Candidate, Kind, Request, Shell, rank, usage};
use runode_shared_types::shell::ShellNames;

fn request(input: &str) -> Option<Request> {
    let cursor = input.find('^').unwrap();
    Request::new(&input.replacen('^', "", 1), cursor)
}

/// `input` 光标处（`^`）排好序的本地候选的值。
fn names(input: &str) -> Vec<String> {
    let request = request(input).unwrap();
    let candidates = request.local_candidates(None, &Shell::default());
    rank(&candidates, request.typed()).into_iter().map(|i| candidates[i].value.clone()).collect()
}

#[test]
fn ls_lists_the_home_directory() {
    let Some(home) = runode_paths::Dirs::from_env().home else {
        return;
    };
    let request = request("ls ~/^").unwrap();
    let candidates = request.local_candidates(Some(Path::new("/")), &Shell::default());
    let expected: Vec<String> = std::fs::read_dir(&home)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    assert_eq!(candidates.iter().filter(|c| c.from == 2).count(), expected.len());
    // 目录结尾带 `/`，接受后不补空格；替换从 `~/` 之后开始。
    let folder = candidates.iter().find(|c| c.value.ends_with('/'));
    if let Some(folder) = folder {
        assert!(folder.value.ends_with('/') && !folder.finish && folder.from == 2);
        let edit = request.accept(folder);
        assert_eq!(edit.backspace, 0);
        assert!(!edit.text.ends_with(' '));
    }
}

#[test]
fn the_first_word_lists_shell_names_and_executables() {
    let shell = Shell {
        path: Some("/bin:/usr/bin".into()),
        names: ShellNames {
            aliases: vec!["gst".into(), "ls".into()],
            alias_values: vec![("gst".into(), "git status".into())],
            functions: vec!["greet".into()],
            builtins: vec!["cd".into()],
            keywords: vec!["if".into()],
        },
        usage: Arc::new(usage::Usage::from_commands(["git status", "git log", "gst"])),
    };
    let listed = |input: &str| -> Vec<Candidate> {
        let request = request(input).unwrap();
        let candidates = request.local_candidates(Some(Path::new("/")), &shell);
        rank(&candidates, request.typed()).into_iter().map(|i| candidates[i].clone()).collect()
    };
    let names = |input: &str| -> Vec<(String, Kind)> { listed(input).into_iter().map(|c| (c.value, c.kind)).collect() };
    let g = names("g^");
    assert!(g.contains(&("gst".to_owned(), Kind::Alias)) && g.contains(&("greet".to_owned(), Kind::Function)));
    // 历史里用得多的在前：`git` 两次、`gst` 一次。
    if std::path::Path::new("/usr/bin/git").exists() {
        assert_eq!(&g[..2], [("git".to_owned(), Kind::Command), ("gst".to_owned(), Kind::Alias)]);
        // 有规格的命令带着规格里的说明。
        let git = listed("gi^").into_iter().find(|c| c.value == "git").unwrap();
        assert_eq!(git.description.as_deref(), Some("The stupid content tracker"));
    }
    // 别名的说明是它展开成什么。
    let gst = listed("gs^").into_iter().find(|c| c.value == "gst").unwrap();
    assert_eq!(gst.description.as_deref(), Some("git status"));
    // 同名的别名和可执行文件只列一次，别名在前。
    let ls = names("ls^");
    assert_eq!(ls[0], ("ls".to_owned(), Kind::Alias));
    assert_eq!(ls.iter().filter(|(v, _)| v == "ls").count(), 1);
    assert!(names("s^").contains(&("sh".to_owned(), Kind::Command)));
    // 写成路径：可执行文件和目录。
    let bin = names("/bin/s^");
    assert!(bin.contains(&("sh".to_owned(), Kind::File)), "{bin:?}");
}

#[test]
fn git_checkout_runs_a_branch_generator() {
    let request = request("git checkout ^").unwrap();
    let jobs = request.generator_jobs();
    assert!(
        jobs.iter().any(|job| job.command.contains("git")),
        "{:?}",
        jobs.iter().map(|j| &j.command).collect::<Vec<_>>()
    );
    assert_eq!(request.before_word(), "git checkout ");
    assert_eq!(request.cells_before_cursor(), 0);
}

#[test]
fn cargo_and_docker_have_specs() {
    assert!(names("cargo b^").contains(&"build".to_owned()));
    assert!(names("docker ru^").contains(&"run".to_owned()));
    assert!(names("npm i^").contains(&"install".to_owned()));
}

#[test]
fn runode_completes_its_own_commands() {
    assert_eq!(names("runode re^"), ["read", "remote"]);
    assert_eq!(names("runode remote ^"), ["devices", "pair", "revoke"]);
    assert!(names("runode wait ab12 --for ^").contains(&"done".to_owned()));
    assert!(names("runode send ab12 --key ctrl-^").contains(&"ctrl-c".to_owned()));
    // `--key` 可以写好几次。
    assert!(names("runode send ab12 --key esc --^").contains(&"--key".to_owned()));
    // 会话和设备由 runode 自己列。
    let jobs = |input: &str| -> Vec<String> {
        request(input).unwrap().generator_jobs().into_iter().map(|job| job.command).collect()
    };
    assert!(jobs("runode kill ^").iter().any(|command| command.contains("runode}\" list --json")));
    assert!(jobs("runode open --near ^").iter().any(|command| command.contains("runode}\" list --json")));
    assert!(jobs("runode remote revoke ^").iter().any(|command| command.contains("runode}\" remote devices --json")));
    // app 自己用的启动参数平时不列，写全了才列。
    assert!(!names("runode --^").contains(&"--host".to_owned()));
    assert_eq!(names("runode --host^"), ["--host"]);
    // `open --` 后面是另一条命令，按那条命令的规格补。
    assert!(names("runode open -- git chec^").contains(&"checkout".to_owned()));
}
