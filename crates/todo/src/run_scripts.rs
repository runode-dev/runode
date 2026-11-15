//! 定时先跑命令（`Schedule::run`）用的内置检查脚本：正文编进二进制，由桌面写到 `Dirs::schedule_scripts_dir`。

/// 定时先跑命令（`Schedule::run`）用的内置脚本；`name` 写进了用户的定时命令里，是长期约定，不改名。
pub struct RunScript {
    pub name: &'static str,
    /// POSIX sh 版本，Windows 以外的系统用。
    pub sh: &'static str,
    /// PowerShell 版本，Windows 上用。文件以 UTF-8 的 BOM 开头：Windows PowerShell 5.1 读没有 BOM 的脚本时按
    /// 系统代码页解码，脚本里的非 ASCII 字符就坏了。
    pub ps1: &'static str,
}

pub const RUN_SCRIPTS: &[RunScript] = &[
    RunScript {
        name: "watch-ci",
        sh: include_str!("run_scripts/watch-ci.sh"),
        ps1: include_str!("run_scripts/watch-ci.ps1"),
    },
    RunScript {
        name: "review-requests",
        sh: include_str!("run_scripts/review-requests.sh"),
        ps1: include_str!("run_scripts/review-requests.ps1"),
    },
    RunScript {
        name: "new-issues",
        sh: include_str!("run_scripts/new-issues.sh"),
        ps1: include_str!("run_scripts/new-issues.ps1"),
    },
];

impl RunScript {
    /// 本机用的那份：Windows 上是 `<name>.ps1`，别的系统是 `<name>.sh`；文件名和正文。
    pub fn for_this_os(&self) -> (String, &'static str) {
        if cfg!(windows) { (format!("{}.ps1", self.name), self.ps1) } else { (format!("{}.sh", self.name), self.sh) }
    }
}
