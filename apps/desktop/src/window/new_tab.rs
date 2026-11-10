//! 标题栏上的新建标签按钮：「+」用默认 shell 开新标签（`NewTab`），旁边的箭头弹出菜单。菜单上面是能开的
//! shell：工作区所在主机的宿主列出了可挑的 shell（Windows 上）时每个一项（`NewShellTab`，快捷键按序号），
//! 没列出时只有一项普通终端，默认 shell 那一项加粗；下面是那台主机上装着的各家 agent（`NewAgentTab`，快捷键
//! 按序号），点了在当前终端的目录开一个新标签、在 shell 里起它。shell 和 agent 都由那台主机的宿主找：agent
//! 按那台主机上的终端里 shell 报告的 PATH 找命令，都没报告时（没有集成的 cmd）按新开的 shell 会拿到的 PATH
//! 找。每次打开菜单都在后台重新问一遍，问到之前菜单里是上次的。

use std::time::Duration;

use gpui::Task;
use gpui::{App, Context, Focusable, MouseButton, Window, prelude::*};
use runode_agent::models::{LAUNCHERS, Launcher, listed, same_kind};
use runode_protocol::{ShellProfile, ShellProfileKind};

use super::{Dropdown, NewAgentTab, NewShellTab, NewTab, TITLEBAR_HEIGHT, WindowView, agents::logo::logo_path};
use crate::{
    host_client::hosts::{HostId, link_of},
    ui::{
        a11y::A11yPress as _,
        button::Button,
        icons::{GIT_ICON, POWERSHELL_ICON, TERMINAL_ICON},
        menu::{MenuHost as _, MenuItem},
        theme::ActiveTheme as _,
    },
};

/// 问宿主有哪些 shell、装了哪些 agent 最多等这么久；等不到时菜单里只有普通终端，没有 agent。
const ASK_HOST_TIMEOUT: Duration = Duration::from_secs(5);

/// 找到的一家 agent：`LAUNCHERS` 里第几个，和起它时在 shell 里打的命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Installed {
    pub(super) agent: usize,
    pub(super) command: String,
}

impl Installed {
    /// 这家 agent 是哪个，画 logo 用；认不出时为空。
    pub(super) fn kind(&self) -> Option<runode_types::agent::AgentKind> {
        launcher_kind(self.agent)
    }
}

/// `LAUNCHERS` 里第 `agent` 个是哪家 agent；认不出时为空。
pub(super) fn launcher_kind(agent: usize) -> Option<runode_types::agent::AgentKind> {
    runode_agent::agent_from_name(program(&LAUNCHERS[agent]))
}

/// 起 `launcher` 的程序名，不带子命令：`opencode2 mini` 是挑模型用的小界面，从这里开的是它的全屏界面。
pub(super) fn program(launcher: &Launcher) -> &'static str {
    launcher.command.split_whitespace().next().unwrap_or(launcher.command)
}

/// 要宿主找的命令：各家 agent 的程序名，每个一次。
fn agent_programs() -> Vec<String> {
    let mut programs: Vec<String> = Vec::new();
    for launcher in LAUNCHERS {
        if !programs.iter().any(|known| known == program(launcher)) {
            programs.push(program(launcher).to_owned());
        }
    }
    programs
}

/// 宿主找到了命令的各家 agent，每家一个：菜单里列的那个版本（`listed`）找不到时换同一家的下一个
/// （`same_kind`）。`found` 和 `programs` 一一对应，是宿主找到的命令文件名；起 agent 时打这个带扩展名的
/// 文件名：Windows 上 npm 在 `.cmd` 旁边还放了 `.ps1`，PowerShell 只打程序名时先跑 `.ps1`，执行策略不让跑
/// 脚本时就报错，写全了 PowerShell 和 cmd 都认。
fn installed_agents(programs: &[String], found: &[Option<String>]) -> Vec<Installed> {
    let command = |name: &str| {
        let at = programs.iter().position(|program| program == name)?;
        found.get(at)?.clone()
    };
    (0..LAUNCHERS.len())
        .filter(|agent| listed(*agent))
        .filter_map(|first| {
            std::iter::successors(Some(first), |agent| same_kind(*agent)).find_map(|agent| {
                let command = command(program(&LAUNCHERS[agent]))?;
                Some(Installed { agent, command })
            })
        })
        .collect()
}

impl WindowView {
    /// 开着的下拉菜单是新建标签的。
    pub(super) fn new_tab_menu_open(&self) -> bool {
        self.dropdown_shown(Dropdown::NewTab)
    }

    /// 菜单里点了第几家 agent、或者按了它的快捷键：在当前终端的目录开新标签，在它的 shell 里起这家 agent。
    /// 还没找过当前工作区的主机上装着哪些 agent 时先在后台找，找到了再开。
    pub(super) fn new_agent_tab(&mut self, action: &NewAgentTab, window: &mut Window, cx: &mut Context<Self>) {
        let index = action.0;
        if self.listed_agents().is_some() {
            self.open_agent(index, window, cx);
            return;
        }
        let (host, job) = self.ask_installed_agents(self.workspace().host.clone(), cx);
        cx.spawn_in(window, async move |this, cx| {
            let agents = job.await;
            this.update_in(cx, |this, window, cx| {
                if this.store_installed_agents(host, agents, cx) {
                    this.open_agent(index, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 开第 `index` 家 agent；序号超出了什么都不做。
    fn open_agent(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(command) = self.listed_agents().unwrap_or_default().get(index).map(|agent| agent.command.clone())
        else {
            return;
        };
        if let Some(view) = self.spawn_beside_focused(window, cx) {
            view.update(cx, |view, cx| view.run_command(command, cx));
            self.insert_tab(self.workspace().active + 1, view, window, cx);
        }
    }

    /// 菜单里点了第几个 shell、或者按了它的快捷键：在当前终端的目录开新标签起它。还没问过当前工作区的主机
    /// 有哪些 shell 时先在后台问，问到了再开。
    pub(super) fn new_shell_tab(&mut self, action: &NewShellTab, window: &mut Window, cx: &mut Context<Self>) {
        let index = action.0;
        if let Some(shells) = self.listed_shells() {
            let program = shells.get(index).map(|shell| shell.program.clone());
            self.open_shell(index, shells.is_empty(), program, window, cx);
            return;
        }
        let (host, job) = self.ask_shells(cx);
        cx.spawn_in(window, async move |this, cx| {
            let shells = job.await;
            this.update_in(cx, |this, window, cx| {
                if !this.store_shells(host, shells, cx) {
                    return;
                }
                let shells = this.listed_shells().unwrap_or_default();
                let program = shells.get(index).map(|shell| shell.program.clone());
                this.open_shell(index, shells.is_empty(), program, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// 开第 `index` 个 shell：主机没列出可挑的（`none_listed`）时第一个是默认 shell，序号超出了什么都不做。
    fn open_shell(
        &mut self,
        index: usize,
        none_listed: bool,
        program: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if none_listed && index == 0 {
            self.new_tab(&NewTab, window, cx);
        } else if let Some(program) = program
            && let Some(view) = self.spawn_shell_beside_focused(Some(program), window, cx)
        {
            self.insert_tab(self.workspace().active + 1, view, window, cx);
        }
    }

    /// 上次问到的当前工作区的主机上能挑的 shell，去掉不认识的种类；还没问到时为 `None`。
    fn listed_shells(&self) -> Option<Vec<&ShellProfile>> {
        let (host, shells) = self.shells.as_ref()?;
        (*host == self.workspace().host)
            .then(|| shells.iter().filter(|shell| shell.kind != ShellProfileKind::Other).collect())
    }

    /// 给弹出新建标签菜单的按钮挂上菜单：按下弹出、再按收起，菜单画在按钮下面往右展开。
    pub(super) fn with_new_tab_menu(&self, button: Button, window: &Window, cx: &mut Context<Self>) -> Button {
        let view = cx.entity().downgrade();
        let open = self.new_tab_menu_open();
        button
            .aria_expanded(open)
            .when(open, |button| button.bg(cx.theme().mix(0.10)))
            // 菜单开着时在捕获阶段就关掉、不再往下传：菜单自己的「点到外面就关」和下面再打开的都不跑。
            .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                if this.new_tab_menu_open() {
                    cx.stop_propagation();
                    this.close_menu(window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_new_tab_menu(false, cx);
                }),
            )
            // 按下鼠标就开，辅助工具的按下另外登记：开时选中第一项，开着时收起。
            .on_a11y_press(view, |this, window, cx| {
                if this.new_tab_menu_open() {
                    this.close_menu(window, cx);
                } else {
                    this.open_new_tab_menu(true, cx);
                }
            })
            .when(open, |button| button.children(self.render_dropdown_menu_rightward(TITLEBAR_HEIGHT, window, cx)))
    }

    /// 在新建标签按钮下面弹出菜单；从键盘打开时选中第一项。
    fn open_new_tab_menu(&mut self, from_keyboard: bool, cx: &mut Context<Self>) {
        let items = self.new_tab_menu_items(cx);
        let target = self.focus_handle(cx);
        // `show_dropdown` 选中给的那一项之后能选的第一项，从最后一项往后绕回第一项。
        let select_after = from_keyboard.then(|| items.len() - 1);
        self.show_dropdown(Dropdown::NewTab, items, target, select_after, cx);
        self.find_installed_agents(cx);
        self.list_shells(cx);
    }

    /// 菜单的各项：当前工作区的主机上能挑的 shell（没有时是普通终端），分隔线，装着的各家 agent，前面是它的
    /// logo。
    fn new_tab_menu_items(&self, cx: &App) -> Vec<Option<MenuItem>> {
        let shells = self.listed_shells().unwrap_or_default();
        let mut items: Vec<Option<MenuItem>> = shells
            .iter()
            .enumerate()
            .map(|(index, shell)| {
                let (label, icon) = shell_item(shell.kind);
                let item = MenuItem::new(label, Box::new(NewShellTab(index)), true, cx).with_icon(icon);
                Some(if shell.default { item.bold() } else { item })
            })
            .collect();
        if items.is_empty() {
            let terminal = MenuItem::new(rust_i18n::t!("agent.launch.terminal"), Box::new(NewTab), true, cx);
            items.push(Some(terminal.with_icon(TERMINAL_ICON).bold()));
        }
        let agents = self.listed_agents().unwrap_or_default();
        if !agents.is_empty() {
            items.push(None);
        }
        items.extend(agents.iter().enumerate().map(|(index, installed)| {
            let launcher = &LAUNCHERS[installed.agent];
            let item = MenuItem::new(launcher.label, Box::new(NewAgentTab(index)), true, cx);
            let logo = installed.kind().and_then(logo_path);
            Some(match logo {
                Some(logo) => item.with_icon(logo),
                None => item,
            })
        }));
        items
    }

    /// 在后台问当前工作区的主机能挑哪些 shell；和上次问到的不一样时换上，菜单开着就换掉它的各项。宿主答不上来
    /// （旧版本的宿主不认这个请求、连不上、超时）时当作没有可挑的。
    fn list_shells(&mut self, cx: &mut Context<Self>) {
        let (host, job) = self.ask_shells(cx);
        cx.spawn(async move |this, cx| {
            let shells = job.await;
            this.update(cx, |this, cx| this.store_shells(host, shells, cx)).ok();
        })
        .detach();
    }

    /// 在后台问当前工作区的主机能挑哪些 shell，返回问的是哪台主机和等回话的任务。
    fn ask_shells(&self, cx: &mut Context<Self>) -> (HostId, Task<Vec<ShellProfile>>) {
        let host = self.workspace().host.clone();
        let link = link_of(&host);
        let job = cx.background_spawn(async move {
            link.list_shells(ASK_HOST_TIMEOUT)
                .inspect_err(|err| tracing::debug!("the host did not list its shells: {err:#}"))
                .unwrap_or_default()
        });
        (host, job)
    }

    /// 记下 `host` 回的 shell，菜单开着且有变化时换掉它的各项。问的时候所在的工作区已经换到别的主机上时
    /// 不记、返回假：那边的回话可能先到，不拿这个晚到的盖掉它。
    fn store_shells(&mut self, host: HostId, shells: Vec<ShellProfile>, cx: &mut Context<Self>) -> bool {
        if host != self.workspace().host {
            return false;
        }
        let found = Some((host, shells));
        if self.shells != found {
            self.shells = found;
            if self.new_tab_menu_open() {
                let items = self.new_tab_menu_items(cx);
                self.replace_menu_items(items, cx);
            }
        }
        true
    }

    /// 上次问到的当前工作区的主机上装着的 agent；还没问到时为 `None`。
    fn listed_agents(&self) -> Option<&[Installed]> {
        match &self.installed_agents {
            Some((host, agents)) if *host == self.workspace().host => Some(agents),
            _ => None,
        }
    }

    /// 在后台请当前工作区的主机找装着的 agent，找到的和上次不一样时换上，菜单开着就换掉它的各项。
    fn find_installed_agents(&mut self, cx: &mut Context<Self>) {
        let (host, job) = self.ask_installed_agents(self.workspace().host.clone(), cx);
        cx.spawn(async move |this, cx| {
            let agents = job.await;
            this.update(cx, |this, cx| this.store_installed_agents(host, agents, cx)).ok();
        })
        .detach();
    }

    /// 在后台请主机 `host` 找装着的 agent，返回问的是哪台主机和等回话的任务：按那台主机上的终端里 shell
    /// 报告的 PATH 找，都没报告时宿主按新起的 shell 会拿到的 PATH 找；宿主答不上来时当作一家都没装。
    pub(super) fn ask_installed_agents(&self, host: HostId, cx: &mut Context<Self>) -> (HostId, Task<Vec<Installed>>) {
        let reported = self.terminal_path(Some(&host), cx);
        let link = link_of(&host);
        let job = cx.background_spawn(async move {
            let programs = agent_programs();
            let found = link
                .find_commands(reported, programs.clone(), ASK_HOST_TIMEOUT)
                .inspect_err(|err| tracing::debug!("the host did not look for the agents: {err:#}"))
                .unwrap_or_default();
            installed_agents(&programs, &found)
        });
        (host, job)
    }

    /// 记下 `host` 上找到的 agent，菜单开着且有变化时换掉它的各项。问的时候所在的工作区已经换到别的主机上时
    /// 不记、返回假：那边的回话可能先到，不拿这个晚到的盖掉它。
    fn store_installed_agents(&mut self, host: HostId, agents: Vec<Installed>, cx: &mut Context<Self>) -> bool {
        if host != self.workspace().host {
            return false;
        }
        let found = Some((host, agents));
        if self.installed_agents != found {
            self.installed_agents = found;
            if self.new_tab_menu_open() {
                let items = self.new_tab_menu_items(cx);
                self.replace_menu_items(items, cx);
            }
        }
        true
    }
}

/// 菜单里一个 shell 显示的名字和图标。
fn shell_item(kind: ShellProfileKind) -> (std::borrow::Cow<'static, str>, &'static str) {
    match kind {
        ShellProfileKind::PowerShell => (rust_i18n::t!("agent.launch.powershell"), POWERSHELL_ICON),
        ShellProfileKind::WindowsPowerShell => (rust_i18n::t!("agent.launch.windows_powershell"), POWERSHELL_ICON),
        ShellProfileKind::Cmd => (rust_i18n::t!("agent.launch.cmd"), TERMINAL_ICON),
        ShellProfileKind::GitBash => (rust_i18n::t!("agent.launch.git_bash"), GIT_ICON),
        ShellProfileKind::Other => (rust_i18n::t!("agent.launch.terminal"), TERMINAL_ICON),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr,
        path::{Path, PathBuf},
    };

    use super::*;

    /// 按 `path` 找到的各家 agent，和宿主找的一样。
    fn found_in(path: &OsStr) -> Vec<Installed> {
        let programs = agent_programs();
        installed_agents(&programs, &runode_terminal::shells::find_commands(Some(path), &programs))
    }

    /// 临时目录里放着 `programs` 这几个命令，当作 PATH 里的一个目录：Unix 上带可执行位，Windows 上是 npm
    /// 装出来的 `.cmd`。
    fn dir_with(name: &str, programs: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rdr-new-tab-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for program in programs {
            let file = dir.join(if cfg!(windows) { format!("{program}.cmd") } else { (*program).to_owned() });
            std::fs::write(&file, "").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        dir
    }

    fn commands(dir: &Path) -> Vec<&'static str> {
        found_in(dir.as_os_str()).into_iter().map(|found| LAUNCHERS[found.agent].command).collect()
    }

    #[test]
    fn lists_only_agents_whose_command_is_on_the_path() {
        let dir = dir_with("listed", &["claude", "pi", "unrelated"]);
        assert_eq!(commands(&dir), ["claude", "pi"]);
        assert!(found_in(OsStr::new("")).is_empty());
        assert!(installed_agents(&agent_programs(), &[]).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn looks_through_every_directory_on_the_path() {
        let (first, second) = (dir_with("first", &["codex"]), dir_with("second", &["claude"]));
        let path = std::env::join_paths([&first, &second]).unwrap();
        let found = found_in(&path).into_iter().map(|found| LAUNCHERS[found.agent].command);
        assert_eq!(found.collect::<Vec<_>>(), ["claude", "codex"]);
        std::fs::remove_dir_all(first).unwrap();
        std::fs::remove_dir_all(second).unwrap();
    }

    /// 和命令同名、shell 却跑不了的文件不算：Unix 上没有可执行位的，Windows 上没有扩展名的（npm 在 `.cmd`
    /// 旁边放的 sh 脚本）和交给脚本宿主打开的。
    #[test]
    fn skips_files_the_shell_cannot_run() {
        let dir = dir_with("plain", &[]);
        std::fs::write(dir.join("claude"), "").unwrap();
        std::fs::write(dir.join("pi.js"), "").unwrap();
        std::fs::create_dir(dir.join("codex")).unwrap();
        assert!(commands(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lists_one_version_of_each_agent() {
        let old = dir_with("old", &["opencode"]);
        assert_eq!(commands(&old), ["opencode"]);
        let both = dir_with("both", &["opencode", "opencode2"]);
        assert_eq!(commands(&both), ["opencode2 mini"]);
        std::fs::remove_dir_all(old).unwrap();
        std::fs::remove_dir_all(both).unwrap();
    }

    /// 起 agent 时打程序名，不带子命令；Windows 上带着找到的扩展名。
    #[test]
    fn launches_the_program_found_without_its_subcommand() {
        let dir = dir_with("launch", &["opencode2"]);
        let found = found_in(dir.as_os_str());
        let command: Vec<_> = found.iter().map(|found| found.command.as_str()).collect();
        assert_eq!(command, [if cfg!(windows) { "opencode2.cmd" } else { "opencode2" }]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
