//! 每一类文字的样子：fast-syntax-highlighting 默认主题的配色，颜色给的是调色板下标。

/// 一段文字属于哪一类，和 fast-syntax-highlighting 主题里的键一一对应。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 找不到的命令。
    UnknownToken,
    /// `if`、`then`、`for`、`{` 这类关键字，以及算术命令的 `((`、`))`。
    ReservedWord,
    /// 命令规格里有的子命令，比如 `git status` 的 `status`。
    Subcommand,
    Alias,
    Builtin,
    Function,
    Command,
    /// `sudo`、`command`、`exec` 这类后面接着另一条命令的。
    Precommand,
    /// `[` 和它的 `]`。
    SingleSqBracket,
    /// `[[` 和它的 `]]`。
    DoubleSqBracket,
    /// 存在的文件。
    Path,
    /// 存在的目录。
    PathToDir,
    /// 带 `*`、`?`、`[` 的通配。
    Globbing,
    SingleHyphenOption,
    DoubleHyphenOption,
    SingleQuotedArgument,
    DoubleQuotedArgument,
    /// `$'…'`。
    DollarQuotedArgument,
    /// `$'…'` 里的反斜杠转义。
    BackDollarQuotedArgument,
    /// `"…"` 里的变量和反斜杠转义。
    BackOrDollarDoubleQuotedArgument,
    Variable,
    /// `!!`、`!$` 这类历史展开。
    HistoryExpansion,
    Comment,
    /// `<<<`。
    HereStringTri,
    /// `a=(…)` 的括号。
    AssignArrayBracket,
    /// 子 shell 和命令替换的括号，按嵌套第几层（1 到 3 轮换）。
    BracketLevel(u8),
    /// 算术里的数字。
    MathNum,
    /// 算术里的变量名。
    MathVar,
}

/// 一类文字的样子：颜色是 256 色调色板的下标，跟着终端主题走。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub color: u8,
    pub bold: bool,
    pub underline: bool,
}

const RED: u8 = 1;
const GREEN: u8 = 2;
const YELLOW: u8 = 3;
const BLUE: u8 = 4;
const MAGENTA: u8 = 5;
const CYAN: u8 = 6;

impl Kind {
    /// fast-syntax-highlighting 默认主题里这一类的样子。只有注释不同：主题写的是黑色加粗，
    /// 在深色背景上看不见（Ghostty 默认不把粗体换成亮色），这里用亮黑，也就是灰色。
    pub fn style(self) -> Style {
        let plain = |color| Style { color, bold: false, underline: false };
        let bold = |color| Style { color, bold: true, underline: false };
        match self {
            Kind::UnknownToken => bold(RED),
            Kind::ReservedWord | Kind::Subcommand | Kind::HereStringTri => plain(YELLOW),
            Kind::Alias
            | Kind::Builtin
            | Kind::Function
            | Kind::Command
            | Kind::Precommand
            | Kind::SingleSqBracket
            | Kind::DoubleSqBracket
            | Kind::AssignArrayBracket => plain(GREEN),
            Kind::Path | Kind::MathNum => plain(MAGENTA),
            Kind::PathToDir => Style { color: MAGENTA, bold: false, underline: true },
            Kind::Globbing | Kind::HistoryExpansion | Kind::MathVar => bold(BLUE),
            Kind::SingleHyphenOption
            | Kind::DoubleHyphenOption
            | Kind::BackDollarQuotedArgument
            | Kind::BackOrDollarDoubleQuotedArgument => plain(CYAN),
            Kind::SingleQuotedArgument | Kind::DoubleQuotedArgument | Kind::DollarQuotedArgument => plain(YELLOW),
            Kind::Variable => plain(113),
            Kind::Comment => bold(8),
            Kind::BracketLevel(level) => bold([GREEN, YELLOW, CYAN][usize::from(level.saturating_sub(1) % 3)]),
        }
    }
}
