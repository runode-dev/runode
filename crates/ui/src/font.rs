//! 界面文字的字体。macOS 和 Windows 上用 GPUI 的系统字体（`.SystemUIFont`），不设 `UiFont`。Linux 上 GPUI
//! 把系统字体认作它自己带的 IBM Plex Sans，Runode 不带这个字体，没装时落到 GPUI 的兜底列表；所以 app 照这台
//! 机器装了的字体用 `pick` 挑一个设进 `UiFont`。
//!
//! 窗口的根元素用 `apply` 设上，里面的界面跟着继承。提示和拖动的预览画在窗口的根上，不在窗口根元素里面，
//! 要各自再设一遍（`tooltip`、`tabs::drag_chip`）。

use gpui::{App, FontFallbacks, Global, SharedString, Styled};

/// 界面文字的字体族和缺字时依次换用的字体；只在 Linux 上设。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiFont {
    pub family: SharedString,
    pub fallbacks: Option<FontFallbacks>,
}

impl Global for UiFont {}

/// 依次挑第一个装了的作界面字体。各发行版桌面的界面字体靠前（Ubuntu、GNOME），常见的通用无衬线字体在后。
const SANS: &[&str] =
    &["Ubuntu Sans", "Ubuntu", "Adwaita Sans", "Cantarell", "Noto Sans", "DejaVu Sans", "Liberation Sans"];

/// 简体、繁体的中文字体：界面是繁体中文时繁体的在前，不然简体的在前，装了的都留作缺字时换用。
const HAN_SIMPLIFIED: &str = "Noto Sans CJK SC";
const HAN_TRADITIONAL: &str = "Noto Sans CJK TC";

/// `installed` 是装了的字体族名，`traditional` 说界面是不是繁体中文。`SANS` 一个都没装时为空，照旧用 GPUI 的
/// 兜底列表。
pub fn pick(installed: &[String], traditional: bool) -> Option<UiFont> {
    let has = |name: &str| installed.iter().any(|family| family == name);
    let family = SANS.iter().copied().find(|name| has(name))?;
    let han = if traditional { [HAN_TRADITIONAL, HAN_SIMPLIFIED] } else { [HAN_SIMPLIFIED, HAN_TRADITIONAL] };
    let fallbacks: Vec<String> = han.into_iter().filter(|name| has(name)).map(str::to_owned).collect();
    Some(UiFont {
        family: family.into(),
        fallbacks: (!fallbacks.is_empty()).then(|| FontFallbacks::from_fonts(fallbacks)),
    })
}

/// 给元素设上界面字体，它里面的文字跟着继承；没设 `UiFont` 时原样返回。
pub fn apply<E: Styled>(mut element: E, cx: &App) -> E {
    if let Some(font) = cx.try_global::<UiFont>() {
        let style = element.text_style();
        style.font_family = Some(font.family.clone());
        style.font_fallbacks = font.fallbacks.clone();
    }
    element
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn picks_the_first_installed_sans() {
        let font = pick(&names(&["DejaVu Sans", "Cantarell", "Hack Nerd Font Mono"]), false).unwrap();
        assert_eq!(font.family, "Cantarell");
        assert_eq!(font.fallbacks, None);
    }

    #[test]
    fn nothing_without_a_known_sans() {
        assert_eq!(pick(&names(&["IBM Plex Sans", "Noto Sans CJK SC"]), false), None);
    }

    #[test]
    fn han_order_follows_the_script() {
        let installed = names(&["Noto Sans", "Noto Sans CJK SC", "Noto Sans CJK TC"]);
        let simplified = pick(&installed, false).unwrap().fallbacks.unwrap();
        assert_eq!(simplified.fallback_list(), ["Noto Sans CJK SC", "Noto Sans CJK TC"]);
        let traditional = pick(&installed, true).unwrap().fallbacks.unwrap();
        assert_eq!(traditional.fallback_list(), ["Noto Sans CJK TC", "Noto Sans CJK SC"]);
    }

    #[test]
    fn han_keeps_only_installed() {
        let font = pick(&names(&["Noto Sans", "Noto Sans CJK SC"]), true).unwrap();
        assert_eq!(font.fallbacks.unwrap().fallback_list(), ["Noto Sans CJK SC"]);
    }
}
