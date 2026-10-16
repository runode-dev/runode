//! 桌面 app 的图标都在这里，连同文件编进二进制：部件自己画的、窗口和各页用的，以及 GitHub Actions 的状态图标
//! （`github_actions`）和各家 agent 的 logo（`agent_logos`）。app 的资源（`AssetSource`）从 `all` 里取，
//! 文件类型的彩色图标在 `file_icons`，`all` 也一并列出。

crate::icons! {
    pub FILES, "icons/", "../assets/icons/";
    pub CHEVRON_RIGHT_ICON = "chevron-right",
    pub CHEVRON_DOWN_ICON = "chevron-down",
    /// 行尾按钮、菜单和标题栏上常用的动作：运行、加、刷新、同步、打开文件、删除、编辑、在终端里跑、
    /// 在浏览器里打开、取消、复制、固定。
    pub PLAY_ICON = "play",
    pub PLUS_ICON = "plus",
    pub REFRESH_ICON = "refresh",
    pub SYNC_ICON = "sync",
    pub OPEN_FILE_ICON = "open-file",
    pub TRASH_ICON = "trash",
    pub PENCIL_ICON = "pencil",
    pub TERMINAL_ICON = "terminal",
    pub GLOBE_ICON = "globe",
    pub BAN_ICON = "ban",
    pub COPY_ICON = "copy",
    pub PIN_ICON = "pin",
    /// 文件树「更多」菜单里用 VS Code 打开，预览栏里切到源码。
    pub CODE_ICON = "code",
    /// 取消暂存、丢弃改动和打勾：Git 面板和预览栏的 diff 共用，打勾还有看板的卡片和 Markdown 的任务列表。
    pub MINUS_ICON = "minus",
    pub DISCARD_ICON = "discard",
    pub CHECK_ICON = "check",
    /// 向下、向上的箭头：Git 面板的图表和预览栏 diff 里跳到下一处、上一处改动。
    pub ARROW_DOWN_ICON = "arrow-down",
    pub ARROW_UP_ICON = "arrow-up",
    /// 放大和还原：分屏标题条和预览栏的标签条。
    pub MAXIMIZE_ICON = "maximize",
    pub MINIMIZE_ICON = "minimize",
    /// 面板标题行上的「更多」菜单，Git 面板和文件树共用。
    pub MORE_ICON = "more",
    /// Git 面板和文件树搜索结果切换树形式、列表形式的按钮，画的是切过去以后的样子。
    pub VIEW_TREE_ICON = "view-tree",
    pub VIEW_LIST_ICON = "view-list",
    /// 浏览文件夹、在访达里打开：新建工作区对话框和文件树的「更多」菜单。
    pub FOLDER_OPEN_ICON = "folder-open",
    /// 关闭、清空：分屏标题条、对话框、看板和文件树的搜索框。
    pub CLOSE_ICON = "close",
    /// 分支：Git 面板、侧栏和看板的卡片。
    pub BRANCH_ICON = "branch",
    /// Git：右侧面板顶上的标签、侧栏和 Git 面板多个仓库时的块头。
    pub GIT_ICON = "git",
    /// 让 AI 写东西：Git 面板提交说明框里的按钮、写提交说明的对话框和设置页的 agent 页。
    pub SPARKLE_ICON = "sparkle",
    /// 文件树搜索框前面的图标。
    pub FILTER_ICON = "filter",
    /// Git 面板储藏那一段每行的应用和弹出。
    pub STASH_APPLY_ICON = "stash-apply",
    pub STASH_POP_ICON = "stash-pop",
    /// 预览栏 diff 标签的图标，和标签条右边的自动换行开关。
    pub DIFF_ICON = "diff",
    pub WRAP_ICON = "wrap",
    /// Markdown 排版视图里 GitHub 提示块（`> [!NOTE]` 这类）标题前的图标。
    pub ALERT_NOTE_ICON = "alert-note",
    pub ALERT_TIP_ICON = "alert-tip",
    pub ALERT_IMPORTANT_ICON = "alert-important",
    pub ALERT_WARNING_ICON = "alert-warning",
    pub ALERT_CAUTION_ICON = "alert-caution",
    /// 侧栏开关的图标。
    pub SIDEBAR_ICON = "sidebar",
    /// 右侧面板顶上文件树标签的图标，以及标题栏右上角开关右侧面板的图标。
    pub FILES_ICON = "files",
    pub PANEL_RIGHT_ICON = "panel-right",
    /// 收在终端区底部的分屏上展开它的箭头。
    pub CHEVRON_UP_ICON = "chevron-up",
    /// 卡片样式下分屏标题条右边向右、向下分屏的按钮。
    pub SPLIT_RIGHT_ICON = "split-right",
    pub SPLIT_DOWN_ICON = "split-down",
    /// 卡片样式下标签图标叠里代表 shell 和普通程序的那块：深色底上的提示符。
    pub PROMPT_ICON = "prompt",
    /// 卡片样式下 agent 等你回答时的标记：像素画的问号。
    pub PIXEL_QUESTION_ICON = "pixel-question",
    /// 窗口底部状态栏上的防止休眠、内存、终端数和端口。
    pub COFFEE_ICON = "coffee",
    pub MEMORY_ICON = "memory",
    pub SHELL_ICON = "shell",
    pub PLUG_ICON = "plug",
    /// 侧栏顶上手机端入口和引导页的图标。
    pub PHONE_ICON = "phone",
    /// 侧栏顶上定时和看板的入口。
    pub CLOCK_ICON = "clock",
    pub KANBAN_ICON = "kanban",
    /// 设置页侧栏顶上的返回，以及各页的图标（终端、文件、agent、远程访问几页借用上面的）。
    pub ARROW_LEFT_ICON = "arrow-left",
    pub SETTINGS_ICON = "settings",
    pub SLIDERS_ICON = "sliders",
    pub PALETTE_ICON = "palette",
    pub KEYBOARD_ICON = "keyboard",
    /// 外观页配色的预览卡（跟着系统换、一个主题用 `PALETTE_ICON`）。
    pub MONITOR_ICON = "monitor",
}

/// GitHub Actions 页的标签和工作流的图标，运行和 step 各种状态的图标（出自 vscode-github-actions，MIT，
/// 见图标目录里的 LICENSE）。
pub mod github_actions {
    crate::icons! {
        pub FILES, "icons/actions/", "../assets/icons/actions/";
        pub WORKFLOW_ICON = "workflow",
        pub SUCCESS_ICON = "success",
        pub FAILURE_ICON = "failure",
        pub CANCELLED_ICON = "cancelled",
        pub SKIPPED_ICON = "skipped",
        pub INPROGRESS_ICON = "inprogress",
        pub QUEUED_ICON = "queued",
        pub WAITING_ICON = "waiting",
        pub PENDING_ICON = "pending",
    }
}

/// 各家 agent 的 logo，出处和许可写在图标目录的 `LICENSE` 里；哪个 agent 用哪个、按原色还是染色由 app 定。
pub mod agent_logos {
    crate::icons! {
        pub FILES, "icons/agents/", "../assets/icons/agents/";
        pub PI = "pi",
        pub CLAUDE = "claude",
        pub CODEX = "codex",
        pub GEMINI = "gemini",
        pub CURSOR = "cursor",
        pub DEVIN = "devin",
        pub ANTIGRAVITY = "antigravity",
        pub CLINE = "cline",
        pub MASTRACODE = "mastracode",
        pub OPENCODE = "opencode",
        pub GITHUB_COPILOT = "github-copilot",
        pub KIMI = "kimi",
        pub KIRO = "kiro",
        pub AMP = "amp",
        pub GROK = "grok",
        pub HERMES = "hermes",
        pub KILO = "kilo",
        pub QODER = "qoder",
        pub QWEN = "qwen",
        pub GOOSE = "goose",
        pub JUNIE = "junie",
        pub OPENHANDS = "openhands",
        pub TRAE = "trae",
        pub CODEBUDDY = "codebuddy",
        pub MISTRAL_VIBE = "mistral-vibe",
        pub JULES = "jules",
        pub OMP = "omp",
        pub MUSE = "muse",
        pub DEEPSEEK = "deepseek",
    }
}

/// 所有图标的资源路径和内容，app 的 `AssetSource` 按路径从这里取。
pub fn all() -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
    FILES.iter().chain(github_actions::FILES).chain(agent_logos::FILES).chain(crate::file_icons::FILES)
}
