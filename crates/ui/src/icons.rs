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
}
