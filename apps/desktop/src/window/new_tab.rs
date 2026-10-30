//! 标题栏上的新建标签按钮：按下弹出菜单，第一项开普通终端（`NewTab`），下面是本机装着的各家 agent，
//! 点了在当前终端的目录开一个新标签、在 shell 里起它。装没装按终端里 shell 报告的 PATH 找命令，shell 没报告
//! 时（没有集成的 PowerShell、cmd）按新开的 shell 会拿到的 PATH 找；每次打开菜单都在后台重找一遍，找完之前
//! 菜单里是上次找到的。

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use gpui::{Action, App, Context, Focusable, MouseButton, Window, prelude::*};
use runode_agent::models::{LAUNCHERS, Launcher, listed, same_kind};

use super::{NewTab, TITLEBAR_HEIGHT, WindowView, agents::logo::logo_path};
use crate::ui::{
    a11y::A11yPress as _,
    button::Button,
    icons::TERMINAL_ICON,
    menu::{MenuHost as _, MenuItem},
    theme::ActiveTheme as _,
};

/// 新建标签菜单里的一家 agent：在新标签的 shell 里打 `command` 起它。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct NewAgentTab {
    command: String,
}

/// 找到的一家 agent：`LAUNCHERS` 里第几个，和起它时在 shell 里打的命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Installed {
    agent: usize,
    command: String,
}

/// 命令文件名在程序名后面接的扩展名。Windows 上只认程序和批处理（npm 装的 agent 是 `.cmd`），不照搬
/// `PATHEXT`：那里还有 `.js`、`.vbs` 这类交给脚本宿主打开的，同名的脚本不是 agent。
const COMMAND_EXTENSIONS: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };

/// `file` 是 shell 能跑的命令：Unix 上是有可执行位的文件（符号链接按它指向的算），Windows 上扩展名已经说明了。
fn is_command(file: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(file) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// 起 `launcher` 的程序名，不带子命令：`opencode2 mini` 是挑模型用的小界面，从这里开的是它的全屏界面。
fn program(launcher: &Launcher) -> &'static str {
    launcher.command.split_whitespace().next().unwrap_or(launcher.command)
}

/// `dirs` 里找得到 `program` 时起它要打的命令：带上找到的那个扩展名。Windows 上 npm 在 `.cmd` 旁边还放了
/// `.ps1`，PowerShell 只打程序名时先跑 `.ps1`，执行策略不让跑脚本时就报错；写全了 PowerShell 和 cmd 都认。
fn on_path(dirs: &[PathBuf], program: &str) -> Option<String> {
    dirs.iter().find_map(|dir| {
        let ext = COMMAND_EXTENSIONS.iter().find(|ext| is_command(&dir.join(format!("{program}{ext}"))))?;
        Some(format!("{program}{ext}"))
    })
}

/// `path` 里找得到命令的各家 agent，每家一个：菜单里列的那个版本（`listed`）找不到时换同一家的下一个
/// （`same_kind`）。
fn installed_agents(path: Option<&OsStr>) -> Vec<Installed> {
    // 只搜绝对路径的项：空项和相对路径落在 app 的当前目录，那里的同名文件不是装着的 agent。
    let dirs: Vec<PathBuf> =
        path.map(|path| std::env::split_paths(path).filter(|dir| dir.is_absolute()).collect()).unwrap_or_default();
    (0..LAUNCHERS.len())
        .filter(|agent| listed(*agent))
        .filter_map(|first| {
            std::iter::successors(Some(first), |agent| same_kind(*agent)).find_map(|agent| {
                let command = on_path(&dirs, program(&LAUNCHERS[agent]))?;
                Some(Installed { agent, command })
            })
        })
        .collect()
}

impl WindowView {
    /// 开着的下拉菜单是新建标签的。
    pub(super) fn new_tab_menu_open(&self) -> bool {
        self.new_tab_menu && self.dropdown_open()
    }

    /// 菜单里点了一家 agent：在当前终端的目录开新标签，在它的 shell 里起这家 agent。
    pub(super) fn new_agent_tab(&mut self, action: &NewAgentTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.spawn_beside_focused(window, cx) {
            let command = action.command.clone();
            view.update(cx, |view, cx| view.run_command(command, cx));
            self.insert_tab(self.workspace().active + 1, view, window, cx);
        }
    }

    /// 给新建标签按钮挂上菜单：按下弹出、再按收起，菜单画在按钮下面往右展开。
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
        // `open_dropdown` 选中给的那一项之后能选的第一项，从最后一项往后绕回第一项。
        let select_after = from_keyboard.then(|| items.len() - 1);
        self.open_dropdown(items, target, select_after, cx);
        self.new_tab_menu = true;
        self.find_installed_agents(cx);
    }

    /// 菜单的各项：普通终端，分隔线，装着的各家 agent，前面是它的 logo。
    fn new_tab_menu_items(&self, cx: &App) -> Vec<Option<MenuItem>> {
        let terminal = MenuItem::new(rust_i18n::t!("agent.launch.terminal"), Box::new(NewTab), true, cx);
        let mut items = vec![Some(terminal.with_icon(TERMINAL_ICON))];
        if !self.installed_agents.is_empty() {
            items.push(None);
        }
        items.extend(self.installed_agents.iter().map(|installed| {
            let launcher = &LAUNCHERS[installed.agent];
            let action = NewAgentTab { command: installed.command.clone() };
            let item = MenuItem::new(launcher.label, Box::new(action), true, cx);
            let logo = runode_agent::agent_from_name(program(launcher)).and_then(logo_path);
            Some(match logo {
                Some(logo) => item.with_icon(logo),
                None => item,
            })
        }));
        items
    }

    /// 在后台按 shell 报告的 PATH 找装着的 agent，没报告时按新起的 shell 会拿到的 PATH（`spawn_path`）；
    /// 和上次找到的不一样时换上，菜单开着就换掉它的各项。
    fn find_installed_agents(&mut self, cx: &mut Context<Self>) {
        let reported = self.terminal_path(cx);
        let job = cx.background_spawn(async move {
            let path = reported.or_else(runode_terminal::pty::spawn_path);
            installed_agents(path.as_deref())
        });
        cx.spawn(async move |this, cx| {
            let found = job.await;
            this.update(cx, |this, cx| {
                if this.installed_agents == found {
                    return;
                }
                this.installed_agents = found;
                if this.new_tab_menu_open() {
                    let items = this.new_tab_menu_items(cx);
                    this.replace_menu_items(items, cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        installed_agents(Some(dir.as_os_str())).into_iter().map(|found| LAUNCHERS[found.agent].command).collect()
    }

    #[test]
    fn lists_only_agents_whose_command_is_on_the_path() {
        let dir = dir_with("listed", &["claude", "pi", "unrelated"]);
        assert_eq!(commands(&dir), ["claude", "pi"]);
        assert!(installed_agents(None).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn looks_through_every_directory_on_the_path() {
        let (first, second) = (dir_with("first", &["codex"]), dir_with("second", &["claude"]));
        let path = std::env::join_paths([&first, &second]).unwrap();
        let found = installed_agents(Some(&path)).into_iter().map(|found| LAUNCHERS[found.agent].command);
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
        let found = installed_agents(Some(dir.as_os_str()));
        let command: Vec<_> = found.iter().map(|found| found.command.as_str()).collect();
        assert_eq!(command, [if cfg!(windows) { "opencode2.cmd" } else { "opencode2" }]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
