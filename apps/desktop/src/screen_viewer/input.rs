//! 本机的键鼠换成发给远端的 `ScreenInput`：按键按键位交过去，由远端的键盘布局和输入法去解释；坐标按
//! 画面在视图里实际占的那块（等比缩放、留黑边）换成远端屏幕上的比例。

use gpui::{Bounds, Keystroke, Modifiers, Pixels, Point, ScrollDelta, point, px, size};
use runode_types::{
    input::Key,
    screen::{ScreenInput, ScreenMods},
};

/// 按行给的滚动换成多少点。
const LINE_POINTS: f32 = 16.;

pub fn mods(modifiers: &Modifiers) -> ScreenMods {
    ScreenMods { shift: modifiers.shift, ctrl: modifiers.control, alt: modifiers.alt, cmd: modifiers.platform }
}

/// 一次按键要发的输入；只按了修饰键、认不出的键为空。认得出键位的照键位发，远端的输入法照常能用；
/// 美式键盘上没有的字符（键名不是美式键位）按文字发。
pub fn key(keystroke: &Keystroke) -> Option<ScreenInput> {
    let mods = mods(&keystroke.modifiers);
    match crate::terminal_view::keys::key_code(&keystroke.key)? {
        (Key::Unidentified, c) => {
            let text = keystroke.key_char.clone().filter(|text| !text.is_empty()).unwrap_or_else(|| c.to_string());
            Some(ScreenInput::Text { text })
        }
        (key, _) => Some(ScreenInput::Key { key, mods }),
    }
}

/// 一次滚轮滚了多少点，累加不足一点的余量 `remainder`；方向和 `ScreenInput::Scroll` 一样，正的是内容
/// 往下、往右走。
pub fn scroll(delta: ScrollDelta, remainder: &mut Point<f32>) -> Option<ScreenInput> {
    let (x, y) = match delta {
        ScrollDelta::Pixels(delta) => (f32::from(delta.x), f32::from(delta.y)),
        ScrollDelta::Lines(delta) => (delta.x * LINE_POINTS, delta.y * LINE_POINTS),
    };
    remainder.x += x;
    remainder.y += y;
    let (dx, dy) = (remainder.x.trunc(), remainder.y.trunc());
    remainder.x -= dx;
    remainder.y -= dy;
    (dx != 0. || dy != 0.).then_some(ScreenInput::Scroll { dx: dx as i32, dy: dy as i32 })
}

/// 宽高比为 `width`:`height` 的画面等比缩放、居中放进 `bounds` 时占的那块。
pub fn fitted(bounds: Bounds<Pixels>, width: u32, height: u32) -> Bounds<Pixels> {
    if width == 0 || height == 0 {
        return bounds;
    }
    let scale = (f32::from(bounds.size.width) / width as f32).min(f32::from(bounds.size.height) / height as f32);
    let fit = size(px(width as f32 * scale), px(height as f32 * scale));
    let origin = point(
        bounds.origin.x + (bounds.size.width - fit.width) / 2.,
        bounds.origin.y + (bounds.size.height - fit.height) / 2.,
    );
    Bounds { origin, size: fit }
}

/// 窗口里的 `position` 落在画面 `picture` 上的比例，出了画面的贴着边算。
pub fn ratio(picture: Bounds<Pixels>, position: Point<Pixels>) -> (f64, f64) {
    let along = |offset: Pixels, length: Pixels| {
        if length <= px(0.) { 0. } else { f64::from(f32::from(offset) / f32::from(length)).clamp(0., 1.) }
    };
    (
        along(position.x - picture.origin.x, picture.size.width),
        along(position.y - picture.origin.y, picture.size.height),
    )
}

/// 视图有 `available` 个物理像素可用时，向远端要多大的画面：照屏幕的宽高比缩进去，不放大，长边不超过
/// `max_side`，宽高取偶数（编码器的要求）。
pub fn capture_size(available: (f32, f32), display: (u32, u32), max_side: u32) -> (u32, u32) {
    let (pw, ph) = (display.0.max(2) as f32, display.1.max(2) as f32);
    let scale = (available.0 / pw).min(available.1 / ph).min(max_side as f32 / pw.max(ph)).min(1.);
    let even = |value: f32| ((value as u32).max(16) / 2) * 2;
    (even(pw * scale), even(ph * scale))
}

#[cfg(test)]
mod tests {
    use gpui::{Keystroke, Modifiers};

    use super::*;

    fn stroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke { modifiers, key: key.into(), key_char: key_char.map(Into::into) }
    }

    /// 字母、快捷键和打字靠 Shift 的符号都按美式键位发，修饰键照带；美式键盘上没有的字符按文字发。
    #[test]
    fn keys_go_by_key_position() {
        assert_eq!(
            key(&stroke("a", Some("a"), Modifiers::none())),
            Some(ScreenInput::Key { key: Key::A, mods: ScreenMods::default() })
        );
        assert_eq!(
            key(&stroke("w", None, Modifiers::command())),
            Some(ScreenInput::Key { key: Key::W, mods: ScreenMods { cmd: true, ..ScreenMods::default() } })
        );
        assert_eq!(
            key(&stroke("!", Some("!"), Modifiers::shift())),
            Some(ScreenInput::Key { key: Key::Digit1, mods: ScreenMods { shift: true, ..ScreenMods::default() } })
        );
        assert_eq!(
            key(&stroke("enter", None, Modifiers::none())),
            Some(ScreenInput::Key { key: Key::Enter, mods: ScreenMods::default() })
        );
        assert_eq!(key(&stroke("é", Some("é"), Modifiers::none())), Some(ScreenInput::Text { text: "é".into() }));
    }

    #[test]
    fn scroll_keeps_fractions() {
        let mut remainder = Point::default();
        assert_eq!(scroll(ScrollDelta::Pixels(point(px(0.), px(0.6))), &mut remainder), None);
        assert_eq!(
            scroll(ScrollDelta::Pixels(point(px(0.), px(0.6))), &mut remainder),
            Some(ScreenInput::Scroll { dx: 0, dy: 1 })
        );
        assert_eq!(
            scroll(ScrollDelta::Lines(point(0., -1.)), &mut remainder),
            Some(ScreenInput::Scroll { dx: 0, dy: -15 })
        );
    }

    /// 宽的画面放进高的视图上下留黑边，点在黑边上贴着画面的边算。
    #[test]
    fn maps_positions_onto_the_letterboxed_picture() {
        let view = Bounds { origin: point(px(100.), px(50.)), size: size(px(400.), px(400.)) };
        let picture = fitted(view, 1600, 900);
        assert_eq!(picture.size, size(px(400.), px(225.)));
        assert_eq!(picture.origin, point(px(100.), px(50.) + px(87.5)));
        assert_eq!(ratio(picture, point(px(300.), px(250.))), (0.5, 0.5));
        assert_eq!(ratio(picture, point(px(90.), px(60.))), (0., 0.));
    }

    #[test]
    fn asks_for_a_picture_that_fits_without_upscaling() {
        assert_eq!(capture_size((1600., 1000.), (3024, 1964), 2560), (1538, 1000));
        assert_eq!(capture_size((8000., 6000.), (1920, 1080), 2560), (1920, 1080));
        assert_eq!(capture_size((8000., 6000.), (5120, 2880), 2560), (2560, 1440));
    }
}
