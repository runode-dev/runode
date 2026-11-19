//! 哪种文件用哪个语言服务器。服务器按程序名起，要用户自己装好、放在 PATH 里。

use std::path::Path;

/// 一个文件的语言服务器：起 `program` 加 `args`，在标准输入输出上说 LSP；`language_id` 是打开文档时报的语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Server {
    pub program: &'static str,
    pub args: &'static [&'static str],
    pub language_id: &'static str,
}

/// 按扩展名挑 `path` 的语言服务器；不认识的没有。
pub fn for_path(path: &Path) -> Option<Server> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let (program, args, language_id): (_, &[_], _) = match ext.as_str() {
        "rs" => ("rust-analyzer", &[], "rust"),
        "go" => ("gopls", &[], "go"),
        "ts" | "mts" | "cts" => ("typescript-language-server", &["--stdio"], "typescript"),
        "tsx" => ("typescript-language-server", &["--stdio"], "typescriptreact"),
        "js" | "mjs" | "cjs" => ("typescript-language-server", &["--stdio"], "javascript"),
        "jsx" => ("typescript-language-server", &["--stdio"], "javascriptreact"),
        "py" | "pyi" => ("pyright-langserver", &["--stdio"], "python"),
        "c" | "h" => ("clangd", &[], "c"),
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => ("clangd", &[], "cpp"),
        "m" => ("clangd", &[], "objective-c"),
        "mm" => ("clangd", &[], "objective-cpp"),
        "swift" => ("sourcekit-lsp", &[], "swift"),
        "zig" => ("zls", &[], "zig"),
        _ => return None,
    };
    Some(Server { program, args, language_id })
}
