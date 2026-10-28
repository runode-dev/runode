//! 几处界面共用的编辑动作：终端、输入框、多行输入框、预览和文件树各自响应其中的一部分，
//! 快捷键和菜单项按 key context 分给它们。另有关标签、关分屏和放大分屏这几个窗口的动作：焦点在
//! 预览栏里时由预览栏接住，关的是预览标签、放大的是预览栏。

use gpui::actions;

actions!(
    runode,
    [
        Copy,
        Paste,
        SelectAll,
        /// 以下几个只在输入框里用：终端里没有可编辑的文字。
        Cut,
        Undo,
        Redo,
        CloseTab,
        /// 关掉当前分屏；标签里只剩它时关掉整个标签。
        ClosePane,
        TogglePaneZoom,
        /// 把面板里选中或最后点过的文件路径打进终端。
        InsertFilePath,
        /// 焦点从面板交回终端。
        FocusTerminal
    ]
);
