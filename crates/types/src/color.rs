//! 颜色。

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn to_u32(self) -> u32 {
        (u32::from(self.0) << 16) | (u32::from(self.1) << 8) | u32::from(self.2)
    }

    /// 从自身往 `other` 混合 `amount`（0 到 1）。
    pub fn mix(self, other: Rgb, amount: f32) -> Rgb {
        let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
        Rgb(mix(self.0, other.0), mix(self.1, other.1), mix(self.2, other.2))
    }
}

/// 光标和选区的颜色：固定色，或者跟随所在单元格的前景、背景色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalColor {
    Rgb(Rgb),
    CellForeground,
    CellBackground,
}
