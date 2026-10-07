//! 改 runode 自己的配置文件：设置界面改一项时，把这个键的设置行换成新值，文件里别的行（说明、
//! 注释掉的默认值、别的键）原样留着，用户手写的东西不丢。

use std::path::Path;

use crate::{Config, parse::KEYS, template::setting_key};

/// 读进来的一份配置文件，按行编辑后写回。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigFile {
    lines: Vec<String>,
    trailing_newline: bool,
}

/// 没注释掉的设置行 `key = value` 的键名和值（去掉两边的引号），`parse_entries` 读配置时也按它。
pub(crate) fn active(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=').unwrap_or((line, ""));
    let value = value.trim();
    Some((key.trim(), value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value)))
}

/// 注释掉的设置行 `# key = value` 的键名；说明行（`##`）和别的行为 `None`。
fn commented(line: &str) -> Option<&str> {
    line.trim_start().starts_with('#').then(|| setting_key(line)).flatten()
}

impl ConfigFile {
    pub fn parse(text: &str) -> Self {
        Self { lines: text.lines().map(str::to_owned).collect(), trailing_newline: text.ends_with('\n') }
    }

    /// 读 `path`；文件还不存在时是一份空的。
    pub fn read(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.text())
    }

    pub fn text(&self) -> String {
        let mut text = self.lines.join("\n");
        if self.trailing_newline && !text.is_empty() {
            text.push('\n');
        }
        text
    }

    /// 文件里给 `key` 写的值，按出现顺序；注释掉的不算。
    pub fn values(&self, key: &str) -> Vec<String> {
        self.lines
            .iter()
            .filter_map(|line| active(line))
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v.to_owned())
            .collect()
    }

    /// 文件里有没有写 `key`。
    pub fn has(&self, key: &str) -> bool {
        self.lines.iter().any(|line| active(line).is_some_and(|(k, _)| k == key))
    }

    /// 把 `key` 的设置行换成 `values`，每项一行；`values` 为空就是删掉这些行，回到 Ghostty 配置、
    /// 主题或内置的默认值。原来写过的，新行放在第一处；没写过的放在模板里这个键注释掉的默认值
    /// 后面，再没有就放在 `KEYS` 里排在它前面、文件里写到了的键后面，都没有就接在文件末尾。
    pub fn set(&mut self, key: &str, values: &[String]) {
        let new: Vec<String> = values.iter().map(|value| line(key, value)).collect();
        let first = self.lines.iter().position(|line| active(line).is_some_and(|(k, _)| k == key));
        if let Some(first) = first {
            self.lines.retain(|line| !active(line).is_some_and(|(k, _)| k == key));
            self.lines.splice(first..first, new);
            return;
        }
        if new.is_empty() {
            return;
        }
        match self.insert_at(key) {
            Some(at) => {
                self.lines.splice(at..at, new);
            }
            None => {
                if self.lines.last().is_some_and(|line| !line.trim().is_empty()) {
                    self.lines.push(String::new());
                }
                self.lines.extend(new);
                self.trailing_newline = true;
            }
        }
    }

    /// 还没写过 `key` 时新行插在哪：见 `set`。
    fn insert_at(&self, key: &str) -> Option<usize> {
        let mentions = |key: &str| {
            self.lines
                .iter()
                .rposition(|line| commented(line) == Some(key) || active(line).is_some_and(|(k, _)| k == key))
        };
        if let Some(last) = self.lines.iter().rposition(|line| commented(line) == Some(key)) {
            return Some(last + 1);
        }
        let keys: Vec<&str> = KEYS.iter().flat_map(|group| group.iter().copied()).collect();
        let ix = keys.iter().position(|k| *k == key)?;
        keys[..ix].iter().rev().find_map(|prev| mentions(prev)).map(|last| last + 1)
    }
}

/// 一行设置。值两头有空格时加引号，免得读的时候被去掉。
fn line(key: &str, value: &str) -> String {
    if value.is_empty() {
        format!("{key} =")
    } else if value.trim() != value {
        format!("{key} = \"{value}\"")
    } else {
        format!("{key} = {value}")
    }
}

/// 检查 `value` 能不能写给 `key`：和读配置时的解析一样，写错了返回原因。不认识的键也算错。
pub fn check_value(key: &str, value: &str) -> Result<(), String> {
    if !KEYS.iter().any(|group| group.contains(&key)) {
        return Err(format!("unknown key: {key}"));
    }
    Config::default().apply(key, value, true)
}
