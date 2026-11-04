//! `rn`：只跑命令行的控制台程序。Windows 上 `runode.exe` 是界面程序，在终端里跑它时输出不到终端，
//! shell 也不等它跑完，命令行由这个程序跑，宿主给 shell 设的 `RUNODE_BIN` 指向它。别的系统上的 `rn`
//! 是指向 `runode` 的符号链接，用不着它。不带命令时拉起旁边的 `runode` 打开窗口。ssh 要问用户时调它当
//! askpass（`runode_ssh::askpass`）：askpass 的回答写在 stdout 上，界面程序的 stdout 交不回去。桌面经 ssh
//! 在这台电脑上跑的 `--ssh-bridge`、`--ssh-project`、`--ssh-screen` 也由它转给 `runode`：经 cmd 起的界面程序
//! 拿不到 ssh 的 stdin、stdout。

fn main() {
    // ssh 调 askpass 时只给一句提示当参数，先于一切参数认出来，免得被当成子命令。
    if let Some(code) = runode_ssh::askpass::run_if_requested() {
        std::process::exit(code);
    }
    let args: Vec<String> = std::env::args_os().skip(1).map(|arg| arg.to_string_lossy().into_owned()).collect();
    if let [arg] = args.as_slice()
        && SSH_ENTRIES.contains(&arg.as_str())
    {
        std::process::exit(forward(arg));
    }
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

/// 交给 `runode` 跑的入口，stdin、stdout 是和桌面说话的协议。
const SSH_ENTRIES: [&str; 3] = ["--ssh-bridge", "--ssh-project", "--ssh-screen"];

/// 和这个程序放在一起的 `runode`。
fn app() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe().map(|exe| exe.with_file_name(format!("runode{}", std::env::consts::EXE_SUFFIX)))
}

/// 用 `arg` 跑旁边的 `runode`，stdin、stdout、stderr 原样交给它，等它退出；返回它的退出码。
fn forward(arg: &str) -> i32 {
    match app().and_then(|app| std::process::Command::new(app).arg(arg).status()) {
        Ok(status) => status.code().unwrap_or(runode_cli::exit::FAILED),
        Err(err) => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "runode: cannot start runode: {err}");
            runode_cli::exit::FAILED
        }
    }
}

/// 拉起和这个程序放在一起的 `runode`，不等它；返回退出码。窗口不继承标准流：调用方读着 `rn` 的输出时，
/// 窗口开着管道就一直不到头。
fn open_window() -> i32 {
    use std::process::Stdio;
    let spawn =
        |app| std::process::Command::new(app).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    match app().and_then(spawn) {
        Ok(_) => runode_cli::exit::OK,
        Err(err) => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "runode: cannot open the window: {err}");
            runode_cli::exit::FAILED
        }
    }
}
