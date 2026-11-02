//! 配置里写成一个名字的枚举：名字表（`Named::NAMES`）只写一次，读配置、写回配置和设置页列出的选项都从它来。

use runode_types::{
    clipboard::{ClipboardRead, ClipboardWrite},
    settings::CursorStyle,
    speech::{DictationCloud, SpeechModel},
};

use crate::{PreventSleep, PreviewClick, TaskPlacement, WindowStyle};

/// 配置文件里写成一个名字的值。
pub trait Named: Copy + PartialEq + 'static {
    /// 配置文件里的写法和对应的值，按设置页上列出的顺序。
    const NAMES: &'static [(&'static str, Self)];

    /// 写进配置文件的名字。
    fn name(self) -> &'static str {
        Self::NAMES.iter().find(|(_, value)| *value == self).map_or("", |(name, _)| name)
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::NAMES.iter().find(|(known, _)| *known == name).map(|(_, value)| *value)
    }
}

/// 读一个名字：空值是 `default`，认不出时报能写哪些。
pub(crate) fn parse_named<T: Named>(value: &str, default: T) -> Result<T, String> {
    if value.is_empty() {
        return Ok(default);
    }
    T::from_name(value).ok_or_else(|| expected(T::NAMES.iter().map(|(name, _)| *name)))
}

/// 「expected a, b or c」。
pub(crate) fn expected<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let names: Vec<_> = names.into_iter().collect();
    match names.split_last() {
        Some((last, [])) => format!("expected {last}"),
        Some((last, rest)) => format!("expected {} or {last}", rest.join(", ")),
        None => "expected nothing".into(),
    }
}

/// 只能从几个名字里挑一个的键能写哪些名字，按设置页上列出的顺序；别的键（开关、可以不写的、
/// 一次写几个的）为空。
pub fn choices(key: &str) -> Option<Vec<&'static str>> {
    fn names<T: Named>() -> Vec<&'static str> {
        T::NAMES.iter().map(|(name, _)| *name).collect()
    }
    Some(match key {
        "window-style" => names::<WindowStyle>(),
        "prevent-sleep" => names::<PreventSleep>(),
        "file-tree-preview-click" => names::<PreviewClick>(),
        "task-placement" => names::<TaskPlacement>(),
        "cursor-style" => names::<CursorStyle>(),
        "clipboard-write" => names::<ClipboardWrite>(),
        "clipboard-read" => names::<ClipboardRead>(),
        "dictation-model" => SpeechModel::ALL.iter().map(|model| model.name()).collect(),
        "dictation-cloud" => DictationCloud::ALL.iter().map(|cloud| cloud.name()).collect(),
        _ => return None,
    })
}

impl Named for WindowStyle {
    const NAMES: &'static [(&'static str, Self)] = &[("cards", Self::Cards), ("classic", Self::Classic)];
}

impl Named for PreventSleep {
    const NAMES: &'static [(&'static str, Self)] = &[("on", Self::On), ("agent", Self::Agent), ("off", Self::Off)];
}

impl Named for PreviewClick {
    const NAMES: &'static [(&'static str, Self)] = &[("single", Self::Single), ("double", Self::Double)];
}

impl Named for TaskPlacement {
    const NAMES: &'static [(&'static str, Self)] = &[("right", Self::Right), ("down", Self::Down), ("tab", Self::Tab)];
}

impl Named for CursorStyle {
    const NAMES: &'static [(&'static str, Self)] = &[
        ("block", Self::Block),
        ("bar", Self::Bar),
        ("underline", Self::Underline),
        ("block_hollow", Self::BlockHollow),
    ];
}

impl Named for ClipboardWrite {
    const NAMES: &'static [(&'static str, Self)] = &[("allow", Self::Allow), ("deny", Self::Deny)];
}

impl Named for ClipboardRead {
    const NAMES: &'static [(&'static str, Self)] = &[("ask", Self::Ask), ("allow", Self::Allow), ("deny", Self::Deny)];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_choice_reads_back_as_itself() {
        let keys = crate::KEYS.iter().flat_map(|group| group.iter());
        let mut seen = 0;
        for key in keys {
            let Some(names) = choices(key) else { continue };
            for name in names {
                let config = crate::parse::tests::load(&[&format!("{key} = {name}")]);
                assert_eq!(config.values(key), [name], "{key} = {name}");
            }
            seen += 1;
        }
        assert_eq!(seen, 9);
        assert_eq!(crate::check_value("task-placement", "up"), Err("expected right, down or tab".into()));
    }
}
