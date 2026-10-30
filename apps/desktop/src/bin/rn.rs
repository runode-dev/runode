//! `rn`：只跑命令行的控制台程序。Windows 上 `runode.exe` 是界面程序，在终端里跑它时输出不到终端，
//! shell 也不等它跑完，命令行由这个程序跑，宿主给 shell 设的 `RUNODE_BIN` 指向它。别的系统上的 `rn`
//! 是指向 `runode` 的符号链接，用不着它。不带命令时拉起旁边的 `runode` 打开窗口。

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|arg| arg.to_string_lossy().into_owned()).collect();
    let env = runode_cli::Env::from_process(env!("RUNODE_BUILD"));
    if let Some(index) = runode_cli::completion_index() {
        std::process::exit(runode_cli::complete(&args, index, &env, &mut std::io::stdout().lock()));
    }
    if args.is_empty() {
        std::process::exit(open_window());
    }
    runode_cli::prepare_console();
    let code = runode_cli::run(&args, &env, &mut std::io::stdout().lock(), &mut std::io::stderr().lock());
    std::process::exit(code);
}

/// 拉起和这个程序放在一起的 `runode`，不等它；返回退出码。
fn open_window() -> i32 {
    let app = std::env::current_exe().map(|exe| exe.with_file_name(format!("runode{}", std::env::consts::EXE_SUFFIX)));
    match app.and_then(|app| std::process::Command::new(app).spawn()) {
        Ok(_) => runode_cli::exit::OK,
        Err(err) => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "runode: cannot open the window: {err}");
            runode_cli::exit::FAILED
        }
    }
}
