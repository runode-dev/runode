//! 桌面 app 的图标都在这里，连同文件编进二进制：部件自己画的、窗口和各页用的，以及 GitHub Actions 的状态图标
//! （`github_actions`）和各家 agent 的 logo（`agent_logos`）。app 的资源（`AssetSource`）从 `all` 里取，
//! 文件类型的彩色图标在 `file_icons`，`all` 也一并列出。

crate::icons! {
    pub FILES, "icons/", "../assets/icons/";
    pub CHEVRON_RIGHT_ICON = "chevron-right",
    pub CHEVRON_DOWN_ICON = "chevron-down",
    /// 常用动作：运行、加、刷新、同步、打开文件、删除、编辑、在终端里跑、在浏览器里打开、取消、复制、固定。
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
    pub CODE_ICON = "code",
    /// 取消暂存、丢弃改动和打勾。
    pub MINUS_ICON = "minus",
    pub DISCARD_ICON = "discard",
    pub CHECK_ICON = "check",
    pub ARROW_DOWN_ICON = "arrow-down",
    pub ARROW_UP_ICON = "arrow-up",
    pub MAXIMIZE_ICON = "maximize",
    pub MINIMIZE_ICON = "minimize",
    /// 「更多」菜单。
    pub MORE_ICON = "more",
    /// 切换树形式、列表形式的按钮，画的是切过去以后的样子。
    pub VIEW_TREE_ICON = "view-tree",
    pub VIEW_LIST_ICON = "view-list",
    /// 浏览文件夹、在访达里打开。
    pub FOLDER_OPEN_ICON = "folder-open",
    /// 关闭、清空。
    pub CLOSE_ICON = "close",
    pub BRANCH_ICON = "branch",
    pub GIT_ICON = "git",
    /// 让 AI 写东西。
    pub SPARKLE_ICON = "sparkle",
    /// 搜索框前面的过滤图标。
    pub FILTER_ICON = "filter",
    pub STASH_APPLY_ICON = "stash-apply",
    pub STASH_POP_ICON = "stash-pop",
    pub DIFF_ICON = "diff",
    pub WRAP_ICON = "wrap",
    /// GitHub 提示块（`> [!NOTE]` 这类）标题前的图标。
    pub ALERT_NOTE_ICON = "alert-note",
    pub ALERT_TIP_ICON = "alert-tip",
    pub ALERT_IMPORTANT_ICON = "alert-important",
    pub ALERT_WARNING_ICON = "alert-warning",
    pub ALERT_CAUTION_ICON = "alert-caution",
    pub SIDEBAR_ICON = "sidebar",
    /// 文件树和右侧面板。
    pub FILES_ICON = "files",
    pub PANEL_RIGHT_ICON = "panel-right",
    /// 向上的箭头（展开）。
    pub CHEVRON_UP_ICON = "chevron-up",
    pub SPLIT_RIGHT_ICON = "split-right",
    pub SPLIT_DOWN_ICON = "split-down",
    /// 深色底上的提示符。
    pub PROMPT_ICON = "prompt",
    pub PIXEL_QUESTION_ICON = "pixel-question",
    /// 防止休眠、内存、终端数和端口。
    pub COFFEE_ICON = "coffee",
    pub MEMORY_ICON = "memory",
    pub SHELL_ICON = "shell",
    pub PLUG_ICON = "plug",
    pub PHONE_ICON = "phone",
    /// 定时和看板。
    pub CLOCK_ICON = "clock",
    pub KANBAN_ICON = "kanban",
    /// 返回和设置各页的图标。
    pub ARROW_LEFT_ICON = "arrow-left",
    pub SETTINGS_ICON = "settings",
    pub SLIDERS_ICON = "sliders",
    pub PALETTE_ICON = "palette",
    pub KEYBOARD_ICON = "keyboard",
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
