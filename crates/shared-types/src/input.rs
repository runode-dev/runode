//! 交给终端的键盘和鼠标输入，与界面框架无关。

/// 接近物理键位的按键。字母、数字和符号键按美式布局里产生它的那个键命名；别的布局上没有
/// 对应键位的字符用 `Unidentified`，字符本身由 `KeyInput::text` 带出。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Unidentified,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Minus,
    Equal,
    BracketLeft,
    BracketRight,
    Backslash,
    Semicolon,
    Quote,
    Comma,
    Period,
    Slash,
    Backquote,
    Space,
    Enter,
    Tab,
    Backspace,
    Escape,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// 按着的修饰键。Command 键留给应用自己的快捷键，不交给终端，所以这里没有它。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// 按着的 Alt（Option）是右边那个，供 `macos-option-as-alt` 区分左右键。
    pub right_alt: bool,
}

/// 一次按键。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub key: Key,
    pub mods: Mods,
    /// 平台生成 `text` 时已经用掉的修饰键。
    pub consumed_mods: Mods,
    /// 不带修饰键时该键产生的字符（没有则为 '\0'）。
    pub unshifted: char,
    pub text: Option<String>,
}

/// 上报给程序的鼠标按键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// 上报给程序的鼠标动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    /// 指针移动；按着键时是拖动。
    Motion,
}

/// 用键盘调整选区时，选区活动的一端往哪里挪。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionAdjust {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}
