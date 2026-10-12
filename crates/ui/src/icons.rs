//! 这里的部件自己画的图标，连同文件编进二进制；app 的资源（`AssetSource`）也要从 `FILES` 里取。

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
    /// 取消暂存、丢弃改动和打勾：Git 面板和预览栏的 diff 共用，打勾还有任务板和 Markdown 的任务列表。
    pub MINUS_ICON = "minus",
    pub DISCARD_ICON = "discard",
    pub CHECK_ICON = "check",
    /// 向下的箭头：Git 面板的图表和预览栏 diff 里跳到下一处改动。
    pub ARROW_DOWN_ICON = "arrow-down",
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
    /// 关闭、清空：分屏标题条、对话框、任务板和文件树的搜索框。
    pub CLOSE_ICON = "close",
}
