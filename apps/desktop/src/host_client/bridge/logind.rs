//! 远端 Linux 上宿主会不会跟着 ssh 的登录被系统结束：看它在哪个 cgroup 里，再看 logind 的规则。
//!
//! 在登录会话的 scope 里时，会话一结束，logind 按 `KillUserProcesses`（及 `KillOnlyUsers`、
//! `KillExcludeUsers`）决定结束不结束里面的进程，linger 管不着它；在用户服务管理器（`user@UID.service`）里时，
//! 管理器在这个用户最后一个会话结束后跟着退出，除非开着 linger。

use std::process::Command;

/// 进程在 cgroup 树里的位置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Placement {
    /// 某个登录会话的 scope（`session-N.scope`）。
    Session,
    /// 用户服务管理器下面。
    UserManager,
    Other,
}

/// 按 `/proc/PID/cgroup` 的内容判断；cgroup v1 时看 systemd 那一层。
pub(super) fn placement(cgroup: &str) -> Placement {
    let paths = cgroup.lines().filter_map(|line| {
        let mut fields = line.splitn(3, ':');
        let (_, controllers, path) = (fields.next()?, fields.next()?, fields.next()?);
        (controllers.is_empty() || controllers == "name=systemd").then_some(path)
    });
    for path in paths {
        for segment in path.split('/') {
            if segment.starts_with("session-") && segment.ends_with(".scope") {
                return Placement::Session;
            }
            if segment.starts_with("user@") && segment.ends_with(".service") {
                return Placement::UserManager;
            }
        }
    }
    Placement::Other
}

/// logind 结束登录会话里的进程的规则。
#[derive(Debug, Default)]
pub(super) struct KillRules {
    pub(super) kill_user_processes: bool,
    pub(super) only: Vec<String>,
    pub(super) exclude: Vec<String>,
}

impl KillRules {
    /// 向 logind 问；问不到（不是 systemd 的系统）时当它不结束。
    pub(super) fn ask() -> Self {
        let get = |property: &str| {
            let output = Command::new("busctl")
                .args([
                    "get-property",
                    "org.freedesktop.login1",
                    "/org/freedesktop/login1",
                    "org.freedesktop.login1.Manager",
                    property,
                ])
                .output()
                .ok()
                .filter(|output| output.status.success())?;
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        };
        Self {
            kill_user_processes: get("KillUserProcesses").is_some_and(|value| value.trim() == "b true"),
            only: get("KillOnlyUsers").map(|value| string_array(&value)).unwrap_or_default(),
            exclude: get("KillExcludeUsers").map(|value| string_array(&value)).unwrap_or_default(),
        }
    }

    /// 会话结束时 logind 结束不结束 `user` 留下的进程。
    pub(super) fn kills(&self, user: &str) -> bool {
        if self.exclude.iter().any(|name| name == user) {
            return false;
        }
        if !self.only.is_empty() {
            return self.only.iter().any(|name| name == user);
        }
        self.kill_user_processes
    }
}

/// busctl 打出的字符串数组：`as 2 "alice" "bob"`。用户名里没有空格和引号。
fn string_array(value: &str) -> Vec<String> {
    value.split_whitespace().skip(2).map(|name| name.trim_matches('"').to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::{KillRules, Placement, placement, string_array};

    #[test]
    fn places_processes_by_cgroup() {
        assert_eq!(placement("0::/user.slice/user-1000.slice/session-59.scope\n"), Placement::Session);
        let service = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/runode-host.service\n";
        assert_eq!(placement(service), Placement::UserManager);
        assert_eq!(placement("0::/.lxc\n"), Placement::Other);
        let v1 = "12:cpu,cpuacct:/session-3.scope\n1:name=systemd:/user.slice/user-1000.slice/user@1000.service\n";
        assert_eq!(placement(v1), Placement::UserManager);
    }

    #[test]
    fn follows_the_kill_lists() {
        assert_eq!(string_array("as 2 \"alice\" \"bob\"\n"), ["alice", "bob"]);
        assert!(string_array("as 0\n").is_empty());
        let mut rules = KillRules { kill_user_processes: true, only: Vec::new(), exclude: vec!["root".into()] };
        assert!(rules.kills("alice"));
        assert!(!rules.kills("root"));
        rules.only = vec!["bob".into()];
        assert!(!rules.kills("alice"));
        rules.kill_user_processes = false;
        assert!(rules.kills("bob"));
    }
}
