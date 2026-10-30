//! 把右侧面板的文件树接到窗口上。文件树本身在 `file_tree`：一个窗口一个 `FileTree`，各 workspace 的树
//! 都在它里面。这里每次对它做什么之前告诉它当前是哪个 workspace、交给它 workspace 的名字、目录和单击打不打开
//! 文件，画面板外框和顶上的标签；后台读到的目录和 git 状态由 `refresh_project` 经 `FileTree::apply_scan` 交给它。
//! 它要窗口办的事（开预览、把路径打进终端、开新标签、预览跟着改名和删除、重读、焦点交回终端、开关变了要存档）
//! 经 `FileTreeEvent` 交回来。
//!
//! 把路径打进终端（`insert_paths`）和拖到终端上（`drop_paths_on_pane`）也在这里，预览栏、Git 面板和分屏都用。

use std::path::{Path, PathBuf};

use crate::file_tree::{FileTree, FileTreeEvent, Input};
use futures::FutureExt as _;
use gpui::{Context, Div, Entity, Focusable, Stateful, Window, div, prelude::*, px};
use runode_config::PreviewClick;
use runode_types::{session::SessionMeta, shell::Shell};

// 菜单部件在 `runode_ui::menu`，窗口里各处从这里引。
pub(super) use crate::ui::menu::{MenuButton, MenuItem};

use super::{WindowView, cards, project::panel_shell};
use crate::{
    config::AppConfig,
    ui::{
        font_size::{self, FontSize},
        theme::ActiveTheme as _,
    },
};

/// 往终端里打路径、地址时照哪种 shell 的写法加引号。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Quoting {
    /// sh、bash（包括 Windows 上的 Git Bash）、zsh、fish：单引号，里面的单引号写成 `'\''`。
    Posix,
    /// PowerShell：单引号里什么都不展开，单引号写两遍；它把 `‘’‚‛` 也当单引号。
    PowerShell,
    /// elvish：单引号里什么都不展开，单引号写两遍。
    Elvish,
    /// cmd：双引号。Windows 的文件名里没有双引号。
    /// shortcut: cmd 在双引号里照样展开定义过的 `%名字%`，交互的 cmd 没有转义写法；有人撞上再拆开引号插 `^`。
    Cmd,
    /// nushell：单引号里不转义，本身带单引号时用 raw string `r#'…'#`。
    Nushell,
}

impl Quoting {
    /// 终端前台的 shell 的写法；前台不是 shell 或者认不出时按 `Quoting::platform`。
    pub(super) fn of_session(meta: &SessionMeta) -> Self {
        let shell = meta.foreground.as_deref().filter(|_| meta.foreground_is_shell).and_then(Shell::detect);
        shell.map_or_else(Self::platform, Self::of_shell)
    }

    /// 程序名是 `program` 的 shell 的写法，认不出时按 `Quoting::platform`。
    pub(super) fn of_program(program: &str) -> Self {
        Shell::detect(program).map_or_else(Self::platform, Self::of_shell)
    }

    fn of_shell(shell: Shell) -> Self {
        match shell {
            Shell::Zsh | Shell::Bash | Shell::Fish => Self::Posix,
            Shell::PowerShell => Self::PowerShell,
            Shell::Elvish => Self::Elvish,
            Shell::Cmd => Self::Cmd,
            Shell::Nushell => Self::Nushell,
        }
    }

    /// 不知道是哪种 shell 时（前台是 agent 这类程序）：Unix 上照 sh 的写法；Windows 上用双引号，
    /// agent 和各种 shell 都认，只是 PowerShell 和 Git Bash 在双引号里还会展开 `$`。
    fn platform() -> Self {
        if cfg!(windows) { Self::Cmd } else { Self::Posix }
    }

    /// 不用引号也不用转义的常见字符。
    fn is_plain(self, c: char) -> bool {
        c.is_alphanumeric()
            || match self {
                Self::Posix => matches!(c, '/' | '.' | '_' | '-' | '+' | '@' | '%' | ':' | ',' | '='),
                // PowerShell 把 `,` 当数组、`@` 当展开，elvish 和 nushell 的裸字也不全认它们；`\` 在这几种里不是转义。
                Self::PowerShell | Self::Elvish | Self::Nushell => {
                    matches!(c, '/' | '\\' | '.' | '_' | '-' | '+' | ':')
                }
                // cmd 把 `,`、`=`、`;` 当参数之间的分隔。
                Self::Cmd => matches!(c, '/' | '\\' | '.' | '_' | '-' | '+' | '@' | ':'),
            }
    }

    /// `text` 只含常见字符时原样，否则括起来。开头是 `=` 或 `%` 时也括起来，zsh 会把 `=foo` 展开成命令的路径。
    pub(super) fn quote(self, text: &str) -> String {
        let plain = !text.is_empty() && !text.starts_with(['=', '%']) && text.chars().all(|c| self.is_plain(c));
        if plain {
            return text.to_owned();
        }
        match self {
            Self::Posix => format!("'{}'", text.replace('\'', r"'\''")),
            Self::PowerShell => {
                let mut quoted = String::with_capacity(text.len() + 2);
                quoted.push('\'');
                for c in text.chars() {
                    if matches!(c, '\'' | '‘' | '’' | '‚' | '‛') {
                        quoted.push(c);
                    }
                    quoted.push(c);
                }
                quoted.push('\'');
                quoted
            }
            Self::Elvish => format!("'{}'", text.replace('\'', "''")),
            Self::Cmd => format!("\"{text}\""),
            Self::Nushell if !text.contains('\'') => format!("'{text}'"),
            Self::Nushell => {
                let mut hashes = "#".to_owned();
                while text.contains(&format!("'{hashes}")) {
                    hashes.push('#');
                }
                format!("r{hashes}'{text}'{hashes}")
            }
        }
    }

    /// 和 `quote` 一样只放过常见字符；sh 的写法里其余字符前面加反斜杠，像访达把文件拖进终端时那样，
    /// 别的写法里反斜杠不是转义，照 `quote` 括起来。
    /// 图片路径用这种写法：Claude Code 按「空格后跟 `/`」拆开几个路径，引号只在整段两头时才去掉，
    /// 几个带引号的路径连在一起它就认不出来了；反斜杠转义它和 Codex 都能还原。
    fn escape(self, text: &str) -> String {
        if self != Self::Posix {
            return self.quote(text);
        }
        let mut escaped = String::with_capacity(text.len());
        for (i, c) in text.chars().enumerate() {
            let plain = self.is_plain(c) && !(i == 0 && matches!(c, '=' | '%'));
            if !plain {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        escaped
    }
}

/// Claude Code、Codex 会把粘贴进来的图片路径换成图片附件，这几种扩展名两边都认。
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| IMAGE_EXTENSIONS.iter().any(|image| ext.eq_ignore_ascii_case(image)))
}

impl WindowView {
    /// 当前 workspace 交给文件树的名字、目录和单击打不打开文件。
    fn file_tree_input(&self, cx: &Context<Self>) -> Input {
        let workspace = self.workspace();
        Input {
            name: workspace.name.clone(),
            dir: workspace.dir.clone(),
            single_click: cx.global::<AppConfig>().0.file_tree_preview_click == PreviewClick::Single,
        }
    }

    /// 对当前 workspace 的文件树做 `f`：先交给它 `file_tree_input`，告诉它是哪个 workspace（换了时它换上那个
    /// workspace 的搜索词重搜）。会让文件树重画，不能在画的时候调。
    pub(super) fn with_file_tree<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut FileTree, &mut Context<FileTree>) -> R,
    ) -> R {
        let input = self.file_tree_input(cx);
        let id = self.workspace().id;
        self.file_tree.update(cx, |tree, cx| {
            tree.set_input(input);
            tree.switch_workspace(id, cx);
            f(tree, cx)
        })
    }

    /// 右侧面板的文件树页：外框和顶上的标签在这里画，里面是文件树。
    pub(super) fn render_files_panel(&mut self, width: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        let input = self.file_tree_input(cx);
        self.file_tree.update(cx, |tree, _| tree.set_input(input));
        let font_size = FontSize::Medium.points() * font_size::scale(cx);
        panel_shell("files-panel", width, cards(cx), cx)
            .bg(cx.theme().mix(0.03))
            .text_size(px(font_size))
            .child(self.render_panel_tabs(cx))
            .child(div().flex_1().min_h_0().child(self.file_tree.clone()))
    }

    /// 文件树要窗口办的事。
    pub(super) fn on_file_tree_event(
        &mut self,
        _: &Entity<FileTree>,
        event: &FileTreeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            FileTreeEvent::Open { path, line: None, pin } => self.open_preview(path, *pin, true, cx),
            FileTreeEvent::Open { path, line: Some(line), pin } => self.open_preview_at(path, *line, *pin, true, cx),
            FileTreeEvent::InsertPath(path) => self.insert_paths(vec![path.clone()], None, window, cx),
            FileTreeEvent::OpenTerminal(dir) => {
                if let Some(view) = self.spawn_terminal(Some(dir), window, cx) {
                    self.insert_tab(self.workspace().active + 1, view, window, cx);
                }
            }
            FileTreeEvent::Moved { from, to } => self.move_previews(from, to, cx),
            FileTreeEvent::Removed(path) => self.close_previews_under(path, window, cx),
            FileTreeEvent::Refresh => self.refresh_project(cx),
            FileTreeEvent::FocusTerminal => self.focus_terminal(window, cx),
            FileTreeEvent::OptionsChanged => self.save(cx),
        }
    }

    /// 把从文件树或访达拖来的 `paths` 放到终端 `pane` 上：切到那个终端，把路径一个个打进去。
    /// 一个路径粘贴一次，因为 Codex 只在一次粘贴里正好是一个路径时才把它当图片。
    pub(super) fn drop_paths_on_pane(
        &mut self,
        pane: gpui::EntityId,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_pane_in_active_tab(pane, window, cx);
        self.insert_paths(paths.to_vec(), None, window, cx);
    }

    /// 把 `paths` 一个个打进当前终端，在它的目录下时写相对路径，后面带上首尾行号 `lines` 和一个空格，
    /// 再把焦点交回终端。比目录要解符号链接，在单独的线程上算好再打进去（`fs_probe::off_thread`）。
    pub(super) fn insert_paths(
        &mut self,
        paths: Vec<PathBuf>,
        lines: Option<(u32, u32)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.focused_view().cloned() else {
            return;
        };
        let cwd = view.read(cx).cwd();
        let quoting = Quoting::of_session(view.read(cx).meta());
        window.focus(&view.focus_handle(cx), cx);
        // 起不了线程时不比目录，照原样打进去。
        let fallback = paths.iter().map(|path| path_text(path, lines, None, quoting)).collect();
        let job = crate::fs_probe::off_thread(
            move || paths.iter().map(|path| path_text(path, lines, cwd.as_deref(), quoting)).collect(),
            fallback,
        );
        // 打进算的时候拿着焦点的那个终端；它关掉了就不打。上一次的先算完时等它打完，连点两次不会倒过来。
        let view = view.downgrade();
        let previous = self.inserting.take();
        let task = cx.spawn_in(window, async move |_, cx| {
            let texts: Vec<String> = job.await;
            if let Some(previous) = previous {
                previous.await;
            }
            view.update_in(cx, |view, window, cx| {
                for text in texts {
                    view.paste_text(format!("{text} "), window, cx);
                }
            })
            .ok();
        });
        self.inserting = Some(task.shared());
    }
}

/// `path` 照 `quoting` 打进终端时的写法，见 `WindowView::insert_paths`。图片写绝对路径：Claude Code 只把
/// 绝对路径认成图片附件。
fn path_text(path: &Path, lines: Option<(u32, u32)>, cwd: Option<&Path>, quoting: Quoting) -> String {
    if lines.is_none() && is_image(path) {
        let absolute = match cwd {
            Some(cwd) if path.is_relative() => cwd.join(path),
            _ => path.to_path_buf(),
        };
        return quoting.escape(&absolute.display().to_string());
    }
    // 终端报的目录和面板里的路径可能一个经过符号链接、一个没有，对不上时都换成真实路径再比。
    let relative = |path: &Path, cwd: &Path| {
        path.strip_prefix(cwd).ok().map(Path::to_path_buf).or_else(|| {
            let (path, cwd) = (path.canonicalize().ok()?, cwd.canonicalize().ok()?);
            path.strip_prefix(cwd).ok().map(Path::to_path_buf)
        })
    };
    let rel = cwd.and_then(|cwd| relative(path, cwd)).filter(|rel| !rel.as_os_str().is_empty());
    let mut text = rel.as_deref().unwrap_or(path).display().to_string();
    match lines {
        Some((first, last)) if first == last => text = format!("{text}:{first}"),
        Some((first, last)) => text = format!("{text}:{first}-{last}"),
        None => {}
    }
    quoting.quote(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_paths_for_posix_shells() {
        let quote = |text| Quoting::Posix.quote(text);
        assert_eq!(quote("src/main.rs:12"), "src/main.rs:12");
        assert_eq!(quote("my file.txt"), "'my file.txt'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("文件.md"), "文件.md");
        assert_eq!(quote("=foo"), "'=foo'");
        assert_eq!(quote("a=b"), "a=b");
        assert_eq!(quote(r"C:\src\a.rs"), r"'C:\src\a.rs'");
    }

    #[test]
    fn quotes_paths_for_windows_shells() {
        assert_eq!(Quoting::PowerShell.quote(r"C:\src\main.rs:12"), r"C:\src\main.rs:12");
        assert_eq!(Quoting::PowerShell.quote(r"C:\me\$(whoami).txt"), r"'C:\me\$(whoami).txt'");
        assert_eq!(Quoting::PowerShell.quote("Bob's ’file’"), "'Bob''s ’’file’’'");
        assert_eq!(Quoting::PowerShell.quote("a,b"), "'a,b'");
        assert_eq!(Quoting::Elvish.quote("it's"), "'it''s'");
        assert_eq!(Quoting::Cmd.quote(r"C:\Users\me\Screen Shot.png"), r#""C:\Users\me\Screen Shot.png""#);
        assert_eq!(Quoting::Cmd.quote("a=b"), r#""a=b""#);
        assert_eq!(Quoting::Nushell.quote(r"C:\my dir"), r"'C:\my dir'");
        assert_eq!(Quoting::Nushell.quote("it's"), "r#'it's'#");
        assert_eq!(Quoting::Nushell.quote("a'#b"), "r##'a'#b'##");
    }

    #[test]
    fn picks_the_quoting_of_the_foreground_shell() {
        let meta = |foreground: &str, foreground_is_shell| SessionMeta {
            foreground: Some(foreground.to_owned()),
            foreground_is_shell,
            ..SessionMeta::default()
        };
        assert_eq!(Quoting::of_session(&meta("pwsh.exe", true)), Quoting::PowerShell);
        assert_eq!(Quoting::of_session(&meta("bash.exe", true)), Quoting::Posix);
        assert_eq!(Quoting::of_session(&meta("nu", true)), Quoting::Nushell);
        assert_eq!(Quoting::of_session(&meta("cmd.exe", true)), Quoting::Cmd);
        assert_eq!(Quoting::of_session(&meta("pwsh.exe", false)), Quoting::platform());
        assert_eq!(Quoting::of_program("powershell.exe"), Quoting::PowerShell);
    }

    #[test]
    fn writes_line_ranges_after_relative_paths() {
        let cwd = Path::new("/repo");
        let text = |path, lines| path_text(Path::new(path), lines, Some(cwd), Quoting::Posix);
        assert_eq!(text("/repo/src/a.rs", None), "src/a.rs");
        assert_eq!(text("/repo/src/a.rs", Some((3, 3))), "src/a.rs:3");
        assert_eq!(text("/repo/src/a.rs", Some((3, 9))), "src/a.rs:3-9");
    }

    #[test]
    fn escapes_image_paths_with_backslashes() {
        let escape = |text| Quoting::Posix.escape(text);
        assert_eq!(escape("/tmp/shot.png"), "/tmp/shot.png");
        assert_eq!(escape("/tmp/Screen Shot (1).png"), r"/tmp/Screen\ Shot\ \(1\).png");
        assert_eq!(escape("/tmp/it's.png"), r"/tmp/it\'s.png");
        assert_eq!(escape("/tmp/截图.png"), "/tmp/截图.png");
        assert_eq!(escape("=a=b"), r"\=a=b");
        assert_eq!(Quoting::PowerShell.escape(r"C:\tmp\shot.png"), r"C:\tmp\shot.png");
        assert_eq!(Quoting::Cmd.escape(r"C:\Users\me\it's.png"), r#""C:\Users\me\it's.png""#);
    }

    #[test]
    fn recognizes_images_by_extension() {
        assert!(is_image(Path::new("/tmp/a.PNG")));
        assert!(is_image(Path::new("b.jpeg")));
        assert!(!is_image(Path::new("c.svg")));
        assert!(!is_image(Path::new("png")));
    }
}
