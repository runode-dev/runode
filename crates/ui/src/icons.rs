//! 这里的部件自己画的图标，连同文件编进二进制；app 的资源（`AssetSource`）也要从 `FILES` 里取。

crate::icons! {
    pub FILES, "icons/", "../assets/icons/";
    pub CHEVRON_RIGHT_ICON = "chevron-right",
    pub CHEVRON_DOWN_ICON = "chevron-down",
}
