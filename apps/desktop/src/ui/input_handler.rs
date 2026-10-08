//! 输入框和终端挂给窗口的输入处理：照搬 GPUI 的 `ElementInputHandler`，只改一处——按着 Option 时
//! 按键先走快捷键。
//!
//! 中文这类输入法开着时，GPUI 在 macOS 上把不带 Control、Command 的可打印键先交给输入法，输入法把
//! option+1 打成「¡」或者干脆吞掉，`alt+digit=goto_workspace` 这类绑定就永远轮不到。Option 组合键
//! 本来就不是拿来组字的，让它先匹配快捷键，没匹配上再照常交给输入法，和英文键盘布局下一样。

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ElementInputHandler, Entity, EntityInputHandler, InputHandler, Pixels, Point,
    TextInputConfiguration, UTF16Selection, Window,
};

pub struct ElementInput<V>(ElementInputHandler<V>);

impl<V: EntityInputHandler> ElementInput<V> {
    pub fn new(bounds: Bounds<Pixels>, view: Entity<V>) -> Self {
        Self(ElementInputHandler::new(bounds, view))
    }
}

impl<V: EntityInputHandler> InputHandler for ElementInput<V> {
    fn prefers_ime_for_printable_keys(&mut self, window: &mut Window, cx: &mut App) -> bool {
        !window.modifiers().alt && self.0.prefers_ime_for_printable_keys(window, cx)
    }

    fn selected_text_range(&mut self, ignore: bool, window: &mut Window, cx: &mut App) -> Option<UTF16Selection> {
        self.0.selected_text_range(ignore, window, cx)
    }

    fn marked_text_range(&mut self, window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.0.marked_text_range(window, cx)
    }

    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        self.0.text_for_range(range, adjusted, window, cx)
    }

    fn replace_text_in_range(&mut self, range: Option<Range<usize>>, text: &str, window: &mut Window, cx: &mut App) {
        self.0.replace_text_in_range(range, text, window, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.replace_and_mark_text_in_range(range, text, selected, window, cx);
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut App) {
        self.0.unmark_text(window, cx);
    }

    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut App) {
        self.0.paste(item, window, cx);
    }

    fn bounds_for_range(&mut self, range: Range<usize>, window: &mut Window, cx: &mut App) -> Option<Bounds<Pixels>> {
        self.0.bounds_for_range(range, window, cx)
    }

    fn character_index_for_point(&mut self, point: Point<Pixels>, window: &mut Window, cx: &mut App) -> Option<usize> {
        self.0.character_index_for_point(point, window, cx)
    }

    fn set_selected_text_range(&mut self, range: Range<usize>, window: &mut Window, cx: &mut App) {
        self.0.set_selected_text_range(range, window, cx);
    }

    fn element_bounds(&mut self, window: &mut Window, cx: &mut App) -> Option<Bounds<Pixels>> {
        self.0.element_bounds(window, cx)
    }

    fn text_length_utf16(&mut self, window: &mut Window, cx: &mut App) -> Option<usize> {
        self.0.text_length_utf16(window, cx)
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        self.0.apple_press_and_hold_enabled()
    }

    fn accepts_text_input(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.0.accepts_text_input(window, cx)
    }

    fn text_input_editable_range(&mut self, window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.0.text_input_editable_range(window, cx)
    }

    fn text_input_configuration(&mut self, window: &mut Window, cx: &mut App) -> TextInputConfiguration {
        self.0.text_input_configuration(window, cx)
    }
}
