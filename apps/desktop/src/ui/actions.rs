//! 几处界面共用的编辑动作：终端、输入框、多行输入框、预览和文件树各自响应其中的一部分，
//! 快捷键和菜单项按 key context 分给它们。

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
        Redo
    ]
);
