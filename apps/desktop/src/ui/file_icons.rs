//! 文件树里按文件类型区分的彩色图标。颜色写死在 SVG 里，界面用 `img()` 按原色画，
//! 不像 `svg()` 那样整个染成一种颜色；所以每个图标都避开纯黑、纯白的主体，深浅两种
//! 背景上都看得清。
//!
//! 文件先按整个文件名认，认不出再按扩展名，都不分大小写：`Cargo.lock` 这类名字的扩展名
//! 会把它认成别的类型，整名要排在前面。目录只按名字认，认不出的用普通文件夹。

/// 列出一组编进二进制的图标：给每个图标一个资源路径常量（`$dir` 加名字），并把它们连同文件内容
/// 收进 `FILES`，路径和文件名只写一遍，免得两边对不上。`$src` 是从调用它的文件到同一目录的相对
/// 路径；`$vis` 是 `FILES` 的可见性，每个常量前面可以带文档和自己的可见性。这里的类型图标和
/// `crate::assets` 里界面上的图标都用它。
macro_rules! icons {
    (
        $vis:vis FILES, $dir:literal, $src:literal;
        $($(#[$meta:meta])* $konst_vis:vis $konst:ident = $name:literal,)*
    ) => {
        $($(#[$meta])* $konst_vis const $konst: &str = concat!($dir, $name, ".svg");)*

        /// 这一组图标的资源路径和内容，`Assets` 按路径从这里取。
        $vis const FILES: &[(&str, &[u8])] = &[
            $(($konst, include_bytes!(concat!($src, $name, ".svg"))),)*
        ];
    };
}
pub(crate) use icons;

icons! {
    pub FILES, "icons/types/", "../../assets/icons/types/";
    FOLDER = "folder",
    FOLDER_OPEN = "folder-open",
    FOLDER_SRC = "folder-src",
    FOLDER_SRC_OPEN = "folder-src-open",
    FOLDER_ASSETS = "folder-assets",
    FOLDER_ASSETS_OPEN = "folder-assets-open",
    FOLDER_LOCALES = "folder-locales",
    FOLDER_LOCALES_OPEN = "folder-locales-open",
    FOLDER_SCRIPTS = "folder-scripts",
    FOLDER_SCRIPTS_OPEN = "folder-scripts-open",
    FOLDER_VENDOR = "folder-vendor",
    FOLDER_VENDOR_OPEN = "folder-vendor-open",
    FOLDER_BUILD = "folder-build",
    FOLDER_BUILD_OPEN = "folder-build-open",
    FOLDER_THEMES = "folder-themes",
    FOLDER_THEMES_OPEN = "folder-themes-open",
    FOLDER_TESTS = "folder-tests",
    FOLDER_TESTS_OPEN = "folder-tests-open",
    FOLDER_DOCS = "folder-docs",
    FOLDER_DOCS_OPEN = "folder-docs-open",
    FOLDER_CONFIG = "folder-config",
    FOLDER_CONFIG_OPEN = "folder-config-open",
    FOLDER_GITHUB = "folder-github",
    FOLDER_GITHUB_OPEN = "folder-github-open",
    FOLDER_CARGO = "folder-cargo",
    FOLDER_CARGO_OPEN = "folder-cargo-open",
    FOLDER_CLAUDE = "folder-claude",
    FOLDER_CLAUDE_OPEN = "folder-claude-open",
    FOLDER_NODE = "folder-node",
    FOLDER_NODE_OPEN = "folder-node-open",
    FOLDER_PACKAGES = "folder-packages",
    FOLDER_PACKAGES_OPEN = "folder-packages-open",
    RUST = "rust",
    TOML = "toml",
    JSON = "json",
    YAML = "yaml",
    MARKDOWN = "markdown",
    MARKDOWN_GUIDE = "markdown-guide",
    LOCK = "lock",
    GIT = "git",
    SHELL = "shell",
    MAKEFILE = "makefile",
    JAVASCRIPT = "javascript",
    TYPESCRIPT = "typescript",
    REACT = "react",
    HTML = "html",
    CSS = "css",
    PYTHON = "python",
    GO = "go",
    C = "c",
    H = "h",
    SWIFT = "swift",
    XML = "xml",
    IMAGE = "image",
    SVG = "svg",
    TEXT = "text",
    LOG = "log",
    LICENSE = "license",
    DOCKER = "docker",
    ENV = "env",
    FILE = "file",
}

/// 文件名对应的图标资源路径。
pub fn file_icon(name: &str) -> &'static str {
    let name = name.to_ascii_lowercase();
    if let Some(icon) = icon_by_name(&name) {
        return icon;
    }
    // 以点开头、后面再没有点的名字（`.bashrc`）整个是名字，没有扩展名。
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => icon_by_extension(ext).unwrap_or(FILE),
        _ => FILE,
    }
}

/// 目录名对应的图标资源路径，`expanded` 为真时取展开的样子。
pub fn folder_icon(name: &str, expanded: bool) -> &'static str {
    let (closed, open) = match name.to_ascii_lowercase().as_str() {
        "src" | "lib" | "source" | "sources" => (FOLDER_SRC, FOLDER_SRC_OPEN),
        "assets" | "asset" | "images" | "image" | "img" | "icons" | "media" => (FOLDER_ASSETS, FOLDER_ASSETS_OPEN),
        "locales" | "locale" | "i18n" | "l10n" | "lang" | "translations" => (FOLDER_LOCALES, FOLDER_LOCALES_OPEN),
        "scripts" | "script" | "bin" => (FOLDER_SCRIPTS, FOLDER_SCRIPTS_OPEN),
        "vendor" | "vendors" | "third_party" | "third-party" | "thirdparty" => (FOLDER_VENDOR, FOLDER_VENDOR_OPEN),
        "target" | "build" | "dist" | "out" | "output" => (FOLDER_BUILD, FOLDER_BUILD_OPEN),
        "themes" | "theme" | "styles" | "style" => (FOLDER_THEMES, FOLDER_THEMES_OPEN),
        "tests" | "test" | "__tests__" | "spec" | "specs" | "e2e" => (FOLDER_TESTS, FOLDER_TESTS_OPEN),
        "docs" | "doc" | "documentation" => (FOLDER_DOCS, FOLDER_DOCS_OPEN),
        "config" | "configs" | ".config" | "conf" | "configuration" => (FOLDER_CONFIG, FOLDER_CONFIG_OPEN),
        ".github" => (FOLDER_GITHUB, FOLDER_GITHUB_OPEN),
        ".cargo" => (FOLDER_CARGO, FOLDER_CARGO_OPEN),
        ".claude" => (FOLDER_CLAUDE, FOLDER_CLAUDE_OPEN),
        "node_modules" => (FOLDER_NODE, FOLDER_NODE_OPEN),
        "crates" | "packages" => (FOLDER_PACKAGES, FOLDER_PACKAGES_OPEN),
        _ => (FOLDER, FOLDER_OPEN),
    };
    if expanded { open } else { closed }
}

/// 按整个文件名（已转小写）认出的类型。
fn icon_by_name(name: &str) -> Option<&'static str> {
    let icon = match name {
        // 锁文件的扩展名有 `.yaml`、`.json`，得在扩展名之前认出来。
        "cargo.lock" | "pnpm-lock.yaml" | "package-lock.json" | "npm-shrinkwrap.json" | "bun.lockb" => LOCK,
        ".gitignore" | ".gitmodules" | ".gitattributes" | ".gitkeep" | ".git-blame-ignore-revs" | ".mailmap" => GIT,
        "agents.md" | "agent.md" | "claude.md" | "claude.local.md" | "readme.md" | "readme" => MARKDOWN_GUIDE,
        "makefile" | "gnumakefile" | "justfile" => MAKEFILE,
        ".bashrc" | ".bash_profile" | ".bash_logout" | ".zshrc" | ".zshenv" | ".zprofile" | ".profile" => SHELL,
        ".envrc" | ".env" => ENV,
        ".dockerignore" | "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml" => DOCKER,
        "rust-toolchain" => TOML,
        "go.mod" | "go.sum" => GO,
        _ => {
            // 这几类常带后缀写成一族名字：`LICENSE-MIT`、`Dockerfile.dev`、`.env.local`，
            // 按名字的开头认。
            let stem = name.split('.').next().unwrap_or(name);
            if matches!(stem, "license" | "licence" | "copying" | "unlicense")
                || stem.starts_with("license-")
                || stem.starts_with("licence-")
            {
                LICENSE
            } else if matches!(stem, "dockerfile" | "containerfile") {
                DOCKER
            } else if name.starts_with(".env.") {
                ENV
            } else {
                return None;
            }
        }
    };
    Some(icon)
}

/// 按扩展名（已转小写，不带点）认出的类型。
fn icon_by_extension(ext: &str) -> Option<&'static str> {
    let icon = match ext {
        "rs" => RUST,
        "toml" => TOML,
        "json" | "jsonc" | "json5" => JSON,
        "yaml" | "yml" => YAML,
        "md" | "markdown" | "mdx" => MARKDOWN,
        "lock" | "lockb" => LOCK,
        "sh" | "bash" | "zsh" | "fish" | "ksh" => SHELL,
        "mk" | "mak" => MAKEFILE,
        "js" | "mjs" | "cjs" => JAVASCRIPT,
        "ts" | "mts" | "cts" => TYPESCRIPT,
        "tsx" | "jsx" => REACT,
        "html" | "htm" => HTML,
        "css" | "scss" | "sass" | "less" => CSS,
        "py" | "pyi" | "pyw" => PYTHON,
        "go" => GO,
        "c" | "cc" | "cpp" | "cxx" | "m" | "mm" => C,
        "h" | "hh" | "hpp" | "hxx" => H,
        "swift" => SWIFT,
        "xml" | "plist" | "xib" | "storyboard" | "entitlements" => XML,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "icns" | "tif" | "tiff" | "avif" | "heic" => IMAGE,
        "svg" => SVG,
        "txt" => TEXT,
        "log" => LOG,
        "dockerfile" => DOCKER,
        "env" => ENV,
        _ => return None,
    };
    Some(icon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_name_wins_over_extension() {
        assert_eq!(file_icon("Cargo.lock"), LOCK);
        assert_eq!(file_icon("Cargo.toml"), TOML);
        assert_eq!(file_icon("pnpm-lock.yaml"), LOCK);
        assert_eq!(file_icon("ci.yaml"), YAML);
        assert_eq!(file_icon("package-lock.json"), LOCK);
        assert_eq!(file_icon("package.json"), JSON);
        assert_eq!(file_icon("CLAUDE.md"), MARKDOWN_GUIDE);
        assert_eq!(file_icon("notes.md"), MARKDOWN);
        assert_eq!(file_icon("yarn.lock"), LOCK);
    }

    #[test]
    fn ignores_case() {
        assert_eq!(file_icon("MAIN.RS"), RUST);
        assert_eq!(file_icon("Makefile"), MAKEFILE);
        assert_eq!(file_icon("AGENTS.md"), MARKDOWN_GUIDE);
        assert_eq!(file_icon("Photo.JPG"), IMAGE);
        assert_eq!(folder_icon("SRC", false), FOLDER_SRC);
        assert_eq!(folder_icon("Assets", true), FOLDER_ASSETS_OPEN);
    }

    #[test]
    fn names_without_extension() {
        assert_eq!(file_icon("LICENSE"), LICENSE);
        assert_eq!(file_icon("LICENSE-MIT"), LICENSE);
        assert_eq!(file_icon("Dockerfile"), DOCKER);
        assert_eq!(file_icon("Dockerfile.dev"), DOCKER);
        assert_eq!(file_icon(".gitignore"), GIT);
        assert_eq!(file_icon(".zshrc"), SHELL);
        assert_eq!(file_icon(".env.local"), ENV);
        // 以点开头的名字不把点后面的部分当扩展名。
        assert_eq!(file_icon(".rs"), FILE);
        assert_eq!(file_icon("build"), FILE);
    }

    #[test]
    fn falls_back_to_generic() {
        assert_eq!(file_icon("data.qwerty"), FILE);
        assert_eq!(file_icon("trailing."), FILE);
        assert_eq!(file_icon(""), FILE);
        assert_eq!(folder_icon("whatever", false), FOLDER);
        assert_eq!(folder_icon("whatever", true), FOLDER_OPEN);
    }

    #[test]
    fn special_folders() {
        assert_eq!(folder_icon("src", false), FOLDER_SRC);
        assert_eq!(folder_icon("src", true), FOLDER_SRC_OPEN);
        assert_eq!(folder_icon(".github", false), FOLDER_GITHUB);
        assert_eq!(folder_icon("node_modules", true), FOLDER_NODE_OPEN);
        assert_eq!(folder_icon("__tests__", false), FOLDER_TESTS);
        assert_eq!(folder_icon("third_party", false), FOLDER_VENDOR);
    }

    /// 目录里的每个 SVG 都收进了 `FILES`，`FILES` 里的路径也没有重复。
    #[test]
    fn every_icon_file_is_listed() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons/types");
        let mut on_disk: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| format!("icons/types/{}", entry.unwrap().file_name().to_string_lossy()))
            .filter(|path| path.ends_with(".svg"))
            .collect();
        on_disk.sort();
        let mut listed: Vec<String> = FILES.iter().map(|(path, _)| path.to_string()).collect();
        listed.sort();
        listed.dedup();
        assert_eq!(listed.len(), FILES.len());
        assert_eq!(on_disk, listed);
    }
}
