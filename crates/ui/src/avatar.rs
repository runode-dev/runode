//! 没有头像时用的名字首字母色块：侧栏的 workspace 行和看板卡片上的目录都用它，同一个名字在两处颜色一样。

use runode_types::color::Rgb;

/// 色块的颜色按名字从这里挑；字是白的，深浅背景上都看得清。
const LETTER_COLORS: [Rgb; 8] = [
    Rgb(0x2B, 0x9E, 0x8F),
    Rgb(0xE0, 0x7A, 0x3C),
    Rgb(0x4C, 0x7D, 0xE8),
    Rgb(0x4F, 0xA3, 0x5B),
    Rgb(0x8E, 0x63, 0xD8),
    Rgb(0xD1, 0x55, 0x8F),
    Rgb(0xC4, 0x93, 0x1F),
    Rgb(0x5B, 0x8A, 0xA6),
];

/// `name` 的首字母色块：字取名字里第一个字母或数字（没有就取第一个字），颜色按整个名字定，改名前后同名的
/// 颜色一样。
pub fn letter_avatar(name: &str) -> (String, Rgb) {
    let letter = name.chars().find(|c| c.is_alphanumeric()).or_else(|| name.chars().next()).unwrap_or(' ');
    let hash = name.bytes().fold(0x811c_9dc5_u32, |hash, byte| (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193));
    (letter.to_uppercase().collect(), LETTER_COLORS[hash as usize % LETTER_COLORS.len()])
}

#[cfg(test)]
mod tests {
    use super::letter_avatar;

    #[test]
    fn letter_avatars_use_the_first_letter_and_a_stable_color() {
        assert_eq!(letter_avatar("website").0, "W");
        assert_eq!(letter_avatar("~/.dotfiles").0, "D");
        assert_eq!(letter_avatar("~").0, "~");
        assert_eq!(letter_avatar("项目").0, "项");
        assert_eq!(letter_avatar("core").1, letter_avatar("core").1);
    }
}
