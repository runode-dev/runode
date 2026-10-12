//! 按钮（`Button`）和开关按钮（`Toggle`），照 shadcn 的写法：`ButtonVariant` 选样子、`ButtonSize` 选
//! 尺寸，颜色在 render 里从主题读。两种都自带 `Role::Button` 和报给辅助工具的名字，点击照旧用 `a11y` 的
//! `Press`、`PressDown`，悬停提示由调用方加。
//!
//! 调用方接着写的样式盖在 variant 和 size 的样式上面：render 时先取下调用方写的那份，铺上组件自己的，
//! 再把调用方的那份叠回去；`hover` 也一样，和组件自己的悬停样式叠在一起。

use gpui::{
    AnyElement, App, Div, ElementId, Hsla, InteractiveElement, Interactivity, IntoElement, ParentElement, Rems,
    RenderOnce, SharedString, Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Svg, Toggled, Window, div,
    prelude::*, px, svg,
};

use crate::{
    a11y::Disable as _,
    font_size::{FontSize, scaled},
    theme::ActiveTheme as _,
};

/// 按钮的样子。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonVariant {
    /// 主按钮：反色，前景色的底、背景色的字。
    Default,
    /// 次要按钮：淡淡一层底色。
    Secondary,
    /// 描一圈边，没有底色。
    Outline,
    /// 平时只有字或图标，悬停时才有底色。只有图标时（这一种和 `Outline`）图标淡一些。
    #[default]
    Ghost,
    /// 带下划线的文字，没有底色和留白，不管 size 的高度和留白、只取它的字号。
    Link,
}

/// 按钮的尺寸。文字档的高度跟着界面字号（`scaled`），图标档是定死的方块。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonSize {
    /// 高 26 点，正文字号。
    #[default]
    Default,
    /// 高 22 点，小一号的字。
    Sm,
    /// 高 36 点，大一号的字。
    Lg,
    /// 24px 见方，图标 14px。
    Icon,
    /// 20px 见方，图标 14px。
    IconSm,
    /// 16px 见方，图标 12px。
    IconXs,
}

impl ButtonSize {
    /// 方块的边长和图标的边长；文字档为空。
    fn icon_box(self) -> Option<(f32, f32)> {
        match self {
            Self::Icon => Some((24., 14.)),
            Self::IconSm => Some((20., 14.)),
            Self::IconXs => Some((16., 12.)),
            Self::Default | Self::Sm | Self::Lg => None,
        }
    }

    /// 文字档的高度、左右留白和字号。
    fn text_box(self) -> (Rems, f32, FontSize) {
        match self {
            Self::Sm => (scaled(22.), 10., FontSize::Small),
            Self::Lg => (scaled(36.), 20., FontSize::Medium),
            _ => (scaled(26.), 12., FontSize::Body),
        }
    }
}

/// `Button` 和 `Toggle` 共用的那一层：外面的元素，以及调用方写的悬停样式、组名和是否自己报了状态。
struct Base {
    div: Stateful<Div>,
    hover: Option<StyleRefinement>,
    group: Option<SharedString>,
    /// 调用方自己报了 `aria_toggled`、`aria_expanded` 或 `aria_selected`。
    caller_state: bool,
    children: Vec<AnyElement>,
}

/// 图标悬停时变亮靠组名：没另起组名时用这个。同名的组取最近的祖先，按钮套在按钮里也不串。
const GROUP: &str = "ui-button";

impl Base {
    fn new(id: ElementId, label: SharedString) -> Self {
        Self {
            div: div().id(id).role(gpui::Role::Button).aria_label(label),
            hover: None,
            group: None,
            caller_state: false,
            children: Vec::new(),
        }
    }

    /// 取下调用方写的样式，`f` 铺上组件自己的（`own_hover` 是组件的悬停样式，空则不悬停），再把调用方的
    /// 叠回去。返回外面的元素、组名、调用方写的字色和悬停时的字色，给里面的图标用。
    fn finish(
        self,
        own_hover: Option<StyleRefinement>,
        f: impl FnOnce(Stateful<Div>) -> Stateful<Div>,
    ) -> (Stateful<Div>, SharedString, Option<Hsla>, Option<Hsla>, Vec<AnyElement>) {
        let Self { mut div, hover, group, children, .. } = self;
        let user = std::mem::take(div.style());
        let mut div = f(div);
        div.style().refine(&user);
        let hover = layer(own_hover, hover);
        let hover_color = hover.as_ref().and_then(|hover| hover.text.color);
        if let Some(hover) = hover {
            div = div.hover(|_| hover);
        }
        let group = group.unwrap_or_else(|| GROUP.into());
        let div = div.group(group.clone());
        (div, group, user.text.color, hover_color, children)
    }
}

/// 调用方的悬停样式 `user` 叠在组件自己的 `own` 上面。
fn layer(own: Option<StyleRefinement>, user: Option<StyleRefinement>) -> Option<StyleRefinement> {
    match (own, user) {
        (Some(mut own), Some(user)) => {
            own.refine(&user);
            Some(own)
        }
        (own, user) => own.or(user),
    }
}

fn icon(path: &'static str, size: f32, color: Hsla, hover: Option<Hsla>, group: SharedString) -> Svg {
    svg()
        .flex_none()
        .path(path)
        .size(px(size))
        .text_color(color)
        .when_some(hover, |icon, hover| icon.group_hover(group, |icon| icon.text_color(hover)))
}

/// 按钮。`label` 是报给辅助工具的名字，文字档没有另给子元素时也是画出来的字；图标档只画图标。
#[derive(IntoElement)]
pub struct Button {
    base: Base,
    label: SharedString,
    icon: Option<&'static str>,
    icon_size: Option<f32>,
    variant: ButtonVariant,
    size: ButtonSize,
    disabled: bool,
    toggled: Option<bool>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        Self {
            base: Base::new(id.into(), label.clone()),
            label,
            icon: None,
            icon_size: None,
            variant: ButtonVariant::default(),
            size: ButtonSize::default(),
            disabled: false,
            toggled: None,
        }
    }

    /// 画在字前面的图标；图标档只画它。
    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// 图标的边长，不写时按 size 定。
    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = Some(size);
        self
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// 尺寸。不叫 `size`：`Styled::size` 已经是设边长的。
    pub fn with_size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// 灰着、不画悬停样式（调用方写的 `hover` 也不画），报给辅助工具不可用。按下的处理调用方别挂上：
    /// 组件拿不掉已经挂上的处理。
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 按下去是切换的：报给辅助工具切换着没有，切换着时字和图标亮一些。普通按钮为空。
    pub fn toggled(mut self, toggled: Option<bool>) -> Self {
        if let Some(toggled) = toggled {
            self.base.div = self.base.div.aria_toggled(Toggled::from(toggled));
            self.base.caller_state = true;
        }
        self.toggled = toggled;
        self
    }
}

impl RenderOnce for Button {
    fn render(mut self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let on = self.toggled == Some(true);
        if self.disabled {
            self.base.hover = None;
        }
        let (color, bg, hover_bg, hover_color) = match self.variant {
            ButtonVariant::Default => {
                (theme.primary_foreground, Some(theme.primary), Some(theme.primary.opacity(0.9)), None)
            }
            ButtonVariant::Secondary => {
                (theme.secondary_foreground, Some(theme.secondary), Some(theme.mix(0.22)), None)
            }
            // 只有图标时图标淡一些，悬停或切换着才全亮；有字时字照常。
            ButtonVariant::Outline | ButtonVariant::Ghost => {
                let dim = !on && self.size.icon_box().is_some();
                (if dim { fg.opacity(0.6) } else { fg }, None, Some(theme.mix(0.10)), Some(fg))
            }
            ButtonVariant::Link => (if on { fg } else { fg.opacity(0.75) }, None, None, Some(fg)),
        };
        let border = (self.variant == ButtonVariant::Outline).then_some(theme.input);
        let own_hover = (!self.disabled && (hover_bg.is_some() || hover_color.is_some())).then(|| {
            let mut hover = StyleRefinement::default();
            if let Some(bg) = hover_bg {
                hover = hover.bg(bg);
            }
            if let Some(color) = hover_color {
                hover = hover.text_color(color);
            }
            hover
        });
        let (size, variant, disabled) = (self.size, self.variant, self.disabled);
        let (div, group, user_color, user_hover_color, children) = self.base.finish(own_hover, |div| {
            let div = match (variant, size.icon_box()) {
                (ButtonVariant::Link, _) => div.text_size(size.text_box().2),
                (_, Some((side, _))) => div.size(px(side)).rounded(px(if side < 20. { 3. } else { 4. })),
                (_, None) => {
                    let (height, padding, font) = size.text_box();
                    div.h(height).px(px(padding)).gap(px(6.)).rounded(px(6.)).text_size(font)
                }
            };
            div.flex_none()
                .flex()
                .items_center()
                .justify_center()
                .text_color(color)
                .when_some(bg, |div, bg| div.bg(bg))
                .when_some(border, |div, border| div.border_1().border_color(border))
                .when(variant == ButtonVariant::Link, |div| div.underline())
                .when(disabled, |div| div.opacity(0.5).aria_disabled(true))
        });
        let icon_side = self.icon_size.unwrap_or_else(|| size.icon_box().map_or(14., |(_, icon)| icon));
        let icon_hover = user_hover_color.or(if disabled { None } else { hover_color });
        let icon = self.icon.map(|path| icon(path, icon_side, user_color.unwrap_or(color), icon_hover, group));
        let label = (children.is_empty() && size.icon_box().is_none()).then_some(self.label);
        div.children(icon).children(children).children(label)
    }
}

/// 开关按钮：只有图标，按下着（`pressed`）时底色一直亮着、图标也亮一些，悬停时底色和图标变亮。
/// 标题栏和面板上那几个用它。报给辅助工具切换着没有；调用方自己报了 `aria_expanded`、`aria_selected`
/// 时以那个为准，不再报切换（展开菜单、面板的按钮，标签）。尺寸由调用方写。
#[derive(IntoElement)]
pub struct Toggle {
    base: Base,
    icon: &'static str,
    icon_size: f32,
    pressed: bool,
}

impl Toggle {
    pub fn new(id: impl Into<ElementId>, icon: &'static str, label: impl Into<SharedString>) -> Self {
        Self { base: Base::new(id.into(), label.into()), icon, icon_size: 16., pressed: false }
    }

    pub fn pressed(mut self, pressed: bool) -> Self {
        self.pressed = pressed;
        self
    }

    /// 图标的边长，默认 16px。
    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = size;
        self
    }
}

impl RenderOnce for Toggle {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let (pressed, pressed_bg) = (self.pressed, theme.mix(0.07));
        let hover = StyleRefinement::default().bg(theme.mix(0.10));
        let report = !self.base.caller_state;
        let (div, group, user_color, user_hover_color, children) = self.base.finish(Some(hover), |div| {
            div.rounded(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .when(pressed, |div| div.bg(pressed_bg))
                .when(report, |div| div.aria_toggled(Toggled::from(pressed)))
        });
        let color = user_color.unwrap_or(fg.opacity(if pressed { 0.9 } else { 0.55 }));
        div.child(icon(self.icon, self.icon_size, color, Some(user_hover_color.unwrap_or(fg)), group))
            .children(children)
    }
}

/// `Button`、`Toggle` 把样式、交互和子元素转给外面的元素；`hover` 和 `group` 记下来到 render 时和组件
/// 自己的合起来，报状态的几个记下调用方报过。
macro_rules! forward {
    ($ty:ty) => {
        impl Styled for $ty {
            fn style(&mut self) -> &mut StyleRefinement {
                self.base.div.style()
            }
        }

        impl InteractiveElement for $ty {
            fn interactivity(&mut self) -> &mut Interactivity {
                self.base.div.interactivity()
            }

            fn hover(mut self, f: impl FnOnce(StyleRefinement) -> StyleRefinement) -> Self {
                self.base.hover = Some(f(StyleRefinement::default()));
                self
            }

            fn group(mut self, group: impl Into<SharedString>) -> Self {
                self.base.group = Some(group.into());
                self
            }
        }

        impl StatefulInteractiveElement for $ty {
            fn aria_toggled(mut self, toggled: Toggled) -> Self {
                self.base.caller_state = true;
                self.base.div = self.base.div.aria_toggled(toggled);
                self
            }

            fn aria_expanded(mut self, expanded: bool) -> Self {
                self.base.caller_state = true;
                self.base.div = self.base.div.aria_expanded(expanded);
                self
            }

            fn aria_selected(mut self, selected: bool) -> Self {
                self.base.caller_state = true;
                self.base.div = self.base.div.aria_selected(selected);
                self
            }
        }

        impl ParentElement for $ty {
            fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
                self.base.children.extend(elements);
            }
        }
    };
}

forward!(Button);
forward!(Toggle);

#[cfg(test)]
mod tests {
    use gpui::{Length, blue, green, red};

    use super::*;

    /// 调用方写的样式盖在组件自己的上面，没写的那几项照组件的。
    #[test]
    fn caller_styles_win_over_the_component() {
        let button = Button::new("b", "b").h(px(50.)).bg(red()).hover(|hover| hover.text_color(green()));
        let own_hover = StyleRefinement::default().bg(blue()).text_color(blue());
        let (mut div, group, color, hover_color, _) =
            button.base.finish(Some(own_hover), |div| div.w(px(10.)).h(px(10.)).bg(blue()));
        let style = div.style();
        assert_eq!(style.size.height, Some(Length::from(px(50.))));
        assert_eq!(style.size.width, Some(Length::from(px(10.))));
        assert_eq!(style.background, Some(red().into()));
        assert_eq!((group.as_ref(), color, hover_color), (GROUP, None, Some(green())));
        let hover = layer(
            Some(StyleRefinement::default().bg(blue()).text_color(blue())),
            Some(StyleRefinement::default().text_color(green())),
        )
        .unwrap();
        assert_eq!((hover.background, hover.text.color), (Some(blue().into()), Some(green())));
    }
}
