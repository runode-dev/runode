//! 任务会话的提示符上打的命令：按各家 shell 的写法从环境变量取提示词、起 agent；Windows 上的 PowerShell
//! 另用按命令行和 cmd 的规矩引好的提示词。

use runode_protocol::todo::TodoSpec;
use runode_types::shell::Shell;

/// 宿主给任务会话的 shell 设的环境变量：交给 agent 的提示词。
pub(super) const ENV_TODO_PROMPT: &str = "RUNODE_TODO_PROMPT";

/// 宿主给新开了工作树的任务会话的 shell 设的环境变量：起 agent 之前先跑的命令（`runode_tasks::worktree_setup`）。
pub(super) const ENV_TODO_SETUP: &str = "RUNODE_TODO_SETUP";

/// 宿主给 Windows 上 PowerShell 的任务会话设的环境变量：按 Windows 命令行的规矩引好的提示词
/// （`windows_quote`），原样放进起 agent 的命令行。
pub(super) const ENV_TODO_ARG: &str = "RUNODE_TODO_ARG";

/// 宿主给 Windows 上 PowerShell 的任务会话设的另一份提示词，agent 是批处理文件（npm 装的 `.cmd`）时代替
/// `ENV_TODO_ARG` 用：按 cmd 的规矩引好（`batch_quote`）。
pub(super) const ENV_TODO_BATCH_ARG: &str = "RUNODE_TODO_BATCH_ARG";

/// Windows 上一条命令行最多这么多个 UTF-16 码元（`CreateProcess`），提示词要整个放进去。
pub(super) const WINDOWS_COMMAND_LINE: usize = 32767;
/// cmd 读的一行最多这么多个字符，起批处理写的 agent（npm 装的 `.cmd`）时程序路径和提示词都算在里面。
const BATCH_COMMAND_LINE: usize = 8191;

/// 在 `shell` 的提示符上打的命令：跑 `agent`，提示词取自环境变量 `ENV_TODO_PROMPT`、整个当一个参数。
/// `setup` 时先用 `sh -c` 跑环境变量 `ENV_TODO_SETUP` 里的命令，成功了才跑 agent。nushell、elvish 不在
/// 双引号里展开 `$名字`，取环境变量也各有写法，也没有 `&&`：外部命令失败时它们自己停下后面的命令。
/// PowerShell 用 `$env:名字`；Windows 上没有 sh，Windows PowerShell 也不认 `&&`，所以 Windows 上初始化命令由
/// PowerShell 自己跑（别处的 pwsh 照样交给 sh，`tasks.json` 的初始化命令按 sh 的写法写），出错或者退出码不为零时抛出异常，这一整行就停在那里，不跑 agent。Windows 上的
/// PowerShell 给原生程序拼命令行时不一定转义参数里的双引号（`passes_command_line`），提示词改从
/// `ENV_TODO_ARG` 取引好的一份，`--%` 后面的 `%名字%` 原样展开进命令行；agent 先找成程序文件
/// （`powershell_program`）。
pub(super) fn agent_command(agent: &str, shell_program: &str, setup: bool) -> String {
    let shell = Shell::detect(shell_program);
    let var = |name: &str| match shell {
        Some(Shell::Nushell) => format!("$env.{name}"),
        Some(Shell::Elvish) => format!("$E:{name}"),
        Some(Shell::PowerShell) => format!("$env:{name}"),
        _ => format!("\"${name}\""),
    };
    let setup = match (setup, shell) {
        (false, _) => String::new(),
        (true, Some(Shell::PowerShell)) if cfg!(windows) => format!(
            "& {{ $ErrorActionPreference = 'Stop'; $global:LASTEXITCODE = 0; Invoke-Expression {}; \
             if ($global:LASTEXITCODE) {{ throw \"setup exited with $global:LASTEXITCODE\" }} }}; ",
            var(ENV_TODO_SETUP)
        ),
        (true, Some(Shell::Nushell | Shell::Elvish)) => format!("sh -c {}; ", var(ENV_TODO_SETUP)),
        (true, _) => format!("sh -c {} && ", var(ENV_TODO_SETUP)),
    };
    if passes_command_line(shell_program) {
        return format!("{setup}{} --% %{ENV_TODO_ARG}%\r", powershell_program(agent));
    }
    format!("{setup}{agent} {}\r", var(ENV_TODO_PROMPT))
}

/// `shell` 是 Windows 上的 PowerShell：Windows PowerShell 5.1 和 PowerShell 7.3 以前给原生程序拼命令行时
/// 不转义参数里的双引号，7.3 以后对 `.cmd` 这类批处理（npm 装的 agent 就是）也照旧，提示词里的双引号会被
/// agent 吞掉、结尾的反斜杠会吃掉收尾的引号。
pub(super) fn passes_command_line(shell: &str) -> bool {
    cfg!(windows) && Shell::detect(shell) == Some(Shell::PowerShell)
}

/// PowerShell 里起 `agent` 的写法：开头的程序名只在 PATH 上找程序文件。只打名字的话 PowerShell 先跑 npm
/// 放在 `.cmd` 旁边的 `.ps1`，执行策略不让跑脚本时报错，`--%` 对脚本也不管用。找到的是批处理时提示词换成
/// `ENV_TODO_BATCH_ARG` 那份，放不进 cmd 的一行（`BATCH_COMMAND_LINE`）时报错；找不到时也报错，这一行停在
/// 那里。程序名带引号、空格这类字符时原样打。
fn powershell_program(agent: &str) -> String {
    let (program, args) = agent.split_once(' ').unwrap_or((agent, ""));
    if !plain_program(program) {
        return agent.to_owned();
    }
    format!(
        "$__runode_agent = Get-Command -ErrorAction Stop -CommandType Application -TotalCount 1 -Name '{program}'; \
         if ($__runode_agent.Extension -in '.bat', '.cmd') {{ \
         if ($__runode_agent.Path.Length + $env:{ENV_TODO_BATCH_ARG}.Length -ge {BATCH_COMMAND_LINE}) {{ \
         throw 'the prompt is too long for {program}, a batch file' }}; \
         $env:{ENV_TODO_ARG} = $env:{ENV_TODO_BATCH_ARG} }}; \
         & $__runode_agent.Path{}{args}",
        if args.is_empty() { "" } else { " " }
    )
}

/// 程序名不带引号、空格这类字符，能原样放进 PowerShell 的单引号里。
fn plain_program(program: &str) -> bool {
    !program.is_empty() && program.chars().all(|ch| ch.is_alphanumeric() || "-_.:\\/".contains(ch))
}

/// 在 `shell` 的提示符上打的续上 agent 对话的命令 `resume`（不带提示词）。Windows 上的 PowerShell 里开头的
/// 程序名先找成程序文件再跑：只打名字会先跑 npm 放的 `.ps1`，执行策略不让跑脚本时报错。
pub(super) fn resume_command_line(resume: &str, shell_program: &str) -> String {
    if !passes_command_line(shell_program) {
        return resume.to_owned();
    }
    powershell_resume(resume)
}

fn powershell_resume(resume: &str) -> String {
    let (program, args) = resume.split_once(' ').unwrap_or((resume, ""));
    if !plain_program(program) {
        return resume.to_owned();
    }
    format!(
        "& (Get-Command -ErrorAction Stop -CommandType Application -TotalCount 1 -Name '{program}').Path{}{args}",
        if args.is_empty() { "" } else { " " }
    )
}

/// 按 Windows 命令行的规矩（`CommandLineToArgvW`、C 运行库）把 `arg` 引成一个参数：整个包进双引号，
/// 双引号和紧挨着它、紧挨着收尾引号的反斜杠都转义。
pub(super) fn windows_quote(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(ch);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// 把 `arg` 引成批处理文件收得到的一个参数：cmd 先读一遍命令行，双引号里的 `&`、`|` 才不分隔命令，所以
/// 双引号写成两个（C 运行库在引号里把它读成一个），紧挨着它的反斜杠照 `windows_quote` 加倍；`%` 后面插一段
/// 展开为空的 `%cd:~,%`，cmd 就不把 `%名字%` 换成环境变量。cmd 读到换行就不往下读了，换行换成空格。
pub(super) fn batch_quote(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for ch in arg.replace("\r\n", " ").chars() {
        match ch {
            '\\' => {
                backslashes += 1;
                quoted.push(ch);
                continue;
            }
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push_str("\"\"");
            }
            '%' => quoted.push_str("%%cd:~,%"),
            '\r' | '\n' => quoted.push(' '),
            _ => quoted.push(ch),
        }
        backslashes = 0;
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes));
    quoted.push('"');
    quoted
}

/// 任务要跑的 agent 命令，带上免确认和自定义 agent 的参数（`runode_agent::models::with_options`）。
pub(super) fn agent_with_options(spec: &TodoSpec) -> String {
    runode_agent::models::with_options(&spec.agent, spec.auto, spec.subagent.as_deref())
}

/// 任务这次开始时给 agent 的对话号，续接时由同样的任务标识和开始时刻再算出来，不用另存。写成 UUID 的样子，
/// Claude 只认 UUID；每次重新开始的时刻不同，号也不同，不撞上一次的对话。
pub(super) fn agent_session(todo: &str, started: u64) -> String {
    format!("{todo}-0000-4000-8000-{started:012x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_sessions_look_like_uuids() {
        assert_eq!(agent_session("0a1b2c3d", 1_760_000_000), "0a1b2c3d-0000-4000-8000-000068e77800");
    }

    #[test]
    fn agent_commands_read_the_prompt_the_way_each_shell_does() {
        assert_eq!(agent_command("claude", "/bin/zsh", false), "claude \"$RUNODE_TODO_PROMPT\"\r");
        assert_eq!(agent_command("claude", "/opt/homebrew/bin/fish", false), "claude \"$RUNODE_TODO_PROMPT\"\r");
        assert_eq!(agent_command("codex --full-auto", "-nu", false), "codex --full-auto $env.RUNODE_TODO_PROMPT\r");
        assert_eq!(agent_command("claude", "/usr/local/bin/elvish", false), "claude $E:RUNODE_TODO_PROMPT\r");
        assert_eq!(
            agent_command("claude", "/bin/zsh", true),
            "sh -c \"$RUNODE_TODO_SETUP\" && claude \"$RUNODE_TODO_PROMPT\"\r"
        );
        assert_eq!(
            agent_command("claude", "/usr/bin/nu", true),
            "sh -c $env.RUNODE_TODO_SETUP; claude $env.RUNODE_TODO_PROMPT\r"
        );
        let claude = if cfg!(windows) {
            "$__runode_agent = Get-Command -ErrorAction Stop -CommandType Application -TotalCount 1 -Name 'claude'; \
             if ($__runode_agent.Extension -in '.bat', '.cmd') { \
             if ($__runode_agent.Path.Length + $env:RUNODE_TODO_BATCH_ARG.Length -ge 8191) { \
             throw 'the prompt is too long for claude, a batch file' }; \
             $env:RUNODE_TODO_ARG = $env:RUNODE_TODO_BATCH_ARG }; \
             & $__runode_agent.Path --% %RUNODE_TODO_ARG%"
        } else {
            "claude $env:RUNODE_TODO_PROMPT"
        };
        assert_eq!(agent_command("claude", "pwsh.exe", false), format!("{claude}\r"));
        let setup = if cfg!(windows) {
            "& { $ErrorActionPreference = 'Stop'; $global:LASTEXITCODE = 0; Invoke-Expression $env:RUNODE_TODO_SETUP; \
             if ($global:LASTEXITCODE) { throw \"setup exited with $global:LASTEXITCODE\" } }; "
        } else {
            "sh -c $env:RUNODE_TODO_SETUP && "
        };
        assert_eq!(agent_command("claude", "powershell.exe", true), format!("{setup}{claude}\r"));
    }

    #[test]
    fn powershell_runs_the_agent_program_file() {
        assert_eq!(
            powershell_program("codex --full-auto"),
            "$__runode_agent = Get-Command -ErrorAction Stop -CommandType Application -TotalCount 1 -Name 'codex'; \
             if ($__runode_agent.Extension -in '.bat', '.cmd') { \
             if ($__runode_agent.Path.Length + $env:RUNODE_TODO_BATCH_ARG.Length -ge 8191) { \
             throw 'the prompt is too long for codex, a batch file' }; \
             $env:RUNODE_TODO_ARG = $env:RUNODE_TODO_BATCH_ARG }; \
             & $__runode_agent.Path --full-auto"
        );
        assert_eq!(powershell_program("'C:\\my tools\\agent.exe' -x"), "'C:\\my tools\\agent.exe' -x");
    }

    #[test]
    fn powershell_resumes_through_the_agent_program_file() {
        assert_eq!(
            powershell_resume("claude --resume 0f3a"),
            "& (Get-Command -ErrorAction Stop -CommandType Application -TotalCount 1 -Name 'claude').Path --resume 0f3a"
        );
        assert_eq!(powershell_resume("'C:\\my tools\\agent.exe' -r"), "'C:\\my tools\\agent.exe' -r");
        assert_eq!(resume_command_line("claude --resume 0f3a", "/bin/zsh"), "claude --resume 0f3a");
    }

    #[test]
    fn batch_quotes_keep_cmd_out_of_the_prompt() {
        assert_eq!(batch_quote("plain"), "\"plain\"");
        assert_eq!(batch_quote(r#"a" & echo hi & "b"#), r#""a"" & echo hi & ""b""#);
        assert_eq!(batch_quote(r#"a\"b"#), r#""a\\""b""#);
        assert_eq!(batch_quote(r#"C:\dir\"#), r#""C:\dir\\""#);
        assert_eq!(batch_quote("100% %PATH%"), r#""100%%cd:~,% %%cd:~,%PATH%%cd:~,%""#);
        assert_eq!(batch_quote("one\r\ntwo\nthree"), "\"one two three\"");
    }

    #[test]
    fn windows_quotes_survive_the_command_line_rules() {
        assert_eq!(windows_quote("plain"), "\"plain\"");
        assert_eq!(windows_quote(r#"say "hi" \ now"#), r#""say \"hi\" \ now""#);
        assert_eq!(windows_quote(r#"C:\dir\"#), r#""C:\dir\\""#);
        assert_eq!(windows_quote(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(windows_quote(""), "\"\"");
    }
}
