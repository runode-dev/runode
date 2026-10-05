//! 沿命令规格往下走，弄清光标所在的词该补什么：子命令、选项、选项的值还是位置参数，再给出
//! 候选和还要去取的动态来源；以及候选的匹配排序、公共前缀和插入时要发给 shell 的按键。
//!
//! 这里是候选、来源和补全计划这几样数据，以及从生成器结果、文件和命令名构造候选。沿规格
//! 走出计划在 `plan`，匹配排序和公共前缀在 `rank`，插入时的按键在 `edit`。

mod edit;
mod plan;
mod rank;

use warp_command_signatures::{FilterTemplateSuggestion, GeneratorName, Priority, TemplateType};

pub use edit::{Edit, edit};
pub use plan::plan;
pub use rank::{common_prefix, decisive, highlight, rank};

/// 候选属于哪一类，菜单里按它分组标出、着色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// PATH 里的可执行文件。
    Command,
    Alias,
    Function,
    Builtin,
    Keyword,
    Subcommand,
    Option,
    /// 规格里写好的参数取值。
    Value,
    /// 跑命令生成的参数取值。
    Generated,
    Folder,
    File,
}

/// 菜单里的一项。
#[derive(Clone, Debug)]
pub struct Candidate {
    /// 菜单里显示的名字。
    pub label: String,
    pub kind: Kind,
    /// 接受时写进去的文字，还没转义。
    pub value: String,
    pub description: Option<String>,
    pub priority: Priority,
    /// 只在输入和它完全一样时才列出来。
    pub hidden: bool,
    /// 从当前词（去掉引号后）的第几个字符起换成 `value`：文件只换最后一段，`--opt=value`
    /// 只换 `=` 后面的部分。
    pub from: usize,
    /// 接受后这个词就写完了：闭合引号、补上空格。目录和要接 `=` 值的选项为假。
    pub finish: bool,
    /// 在 runode 的命令历史里用过几次，见 `usage`；匹配程度和优先级都一样时常用的在前。
    pub usage: u32,
    /// 生成器要求保持原样顺序的结果在其中的位置；这些排在按名字排的候选前面。
    pub fixed: Option<u32>,
    /// 分组标签的补充说明：生成器结果所属参数在规格里的名字。
    pub detail: Option<String>,
}

impl Candidate {
    /// 一个显示名和插入文字相同、其余取默认值的候选。
    pub fn new(value: impl Into<String>, kind: Kind) -> Self {
        let value = value.into();
        Self {
            label: value.clone(),
            kind,
            value,
            description: None,
            priority: Priority::Default,
            hidden: false,
            from: 0,
            finish: true,
            usage: 0,
            fixed: None,
            detail: None,
        }
    }
}

/// 还要去取候选的地方。
#[derive(Clone, Debug)]
pub enum Source {
    /// 列出文件或目录，候选从当前词里第 `from` 个字符起（去掉目录部分之前）。
    Template { kind: TemplateType, filter: Option<FilterTemplateSuggestion>, from: usize },
    /// 跑命令生成，候选从当前词里第 `from` 个字符起；`arg` 是参数在规格里的名字。
    Generator { name: GeneratorName, from: usize, group: u32, arg: Option<String> },
    /// 命令名：shell 报告的别名、函数、内建命令、关键字，以及 PATH 里的可执行文件。
    Commands,
    /// 写成路径的命令名：列出可执行文件和目录，候选从当前词里第 `from` 个字符起。
    CommandPaths { from: usize },
}

/// 光标所在的词要补的东西。
#[derive(Clone, Debug)]
pub struct Plan {
    /// 命令名，查动态补全数据用。
    pub command: String,
    /// 不用再去取的候选：子命令、选项、参数的固定取值。
    pub candidates: Vec<Candidate>,
    pub sources: Vec<Source>,
    /// 光标所在的词在光标之前的部分，去掉了引号和转义。
    pub typed: String,
    /// 生成器要的：从命令名起到当前词的各个词，当前词只取光标前的部分，空的不算。
    pub tokens: Vec<String>,
    /// 写在命令前面的变量赋值。
    pub env: Vec<String>,
    /// 候选里的子命令是命令本身的子命令（不是子命令的子命令），按历史统计常用程度时用。
    pub top_level: bool,
}

/// 生成器给出的一个结果，是这个生成器的第 `index` 个；标明了是文件或目录的归到文件、目录两组。
/// `ordered` 时保持生成器给的顺序，`arg` 是参数在规格里的名字。
pub fn generated(
    suggestion: warp_command_signatures::Suggestion,
    from: usize,
    index: usize,
    ordered: bool,
    arg: Option<String>,
) -> Candidate {
    use warp_command_signatures::IconType;
    let kind = match suggestion.icon {
        Some(IconType::Folder) => Kind::Folder,
        Some(IconType::File) => Kind::File,
        _ if suggestion.exact_string.ends_with('/') => Kind::Folder,
        _ => Kind::Generated,
    };
    Candidate {
        label: suggestion.display_name.unwrap_or_else(|| suggestion.exact_string.clone()),
        finish: !suggestion.exact_string.ends_with('/'),
        description: suggestion.description,
        priority: suggestion.priority,
        hidden: suggestion.is_hidden,
        from,
        fixed: ordered.then_some(index as u32),
        detail: (kind == Kind::Generated).then_some(arg).flatten(),
        ..Candidate::new(suggestion.exact_string, kind)
    }
}

/// 列出的一个文件或目录；`from` 是目录部分之后。
pub fn path(name: String, is_dir: bool, description: Option<String>, from: usize) -> Candidate {
    let value = if is_dir { format!("{name}/") } else { name };
    Candidate {
        description,
        from,
        finish: !is_dir,
        ..Candidate::new(value, if is_dir { Kind::Folder } else { Kind::File })
    }
}

/// 命令名候选：别名、函数、内建命令、关键字或者 PATH 里的可执行文件。
pub fn command(name: String, kind: Kind) -> Candidate {
    Candidate::new(name, kind)
}
