//! 读 `~/.ssh/config` 里写着的主机别名，给用户挑。连接时怎么解析交给 OpenSSH 自己（`ssh -G`）。

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

/// `Include` 最多嵌套这么多层，和 OpenSSH 一样，防着互相包含绕圈。
const MAX_INCLUDE_DEPTH: usize = 16;

/// `home` 下的 `~/.ssh/config`（连同它 `Include` 的文件）里写着的主机别名，按出现的先后，去重。带通配
/// （`*`、`?`）和取反（`!`）的模式不是具体的主机，不列；`Match` 块里的不列。
pub fn list_hosts(home: &Path) -> Vec<String> {
    let ssh_dir = home.join(".ssh");
    let mut hosts = Vec::new();
    let mut seen = HashSet::new();
    read_file(&ssh_dir.join("config"), home, &ssh_dir, 0, &mut |alias| {
        if seen.insert(alias.to_owned()) {
            hosts.push(alias.to_owned());
        }
    });
    hosts
}

fn read_file(path: &Path, home: &Path, ssh_dir: &Path, depth: usize, found: &mut dyn FnMut(&str)) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    parse(&text, home, ssh_dir, depth, found);
}

/// 一份配置的内容里的主机别名交给 `found`，`Include` 的文件就地展开。
fn parse(text: &str, home: &Path, ssh_dir: &Path, depth: usize, found: &mut dyn FnMut(&str)) {
    for line in text.lines() {
        let Some((key, value)) = split_line(line) else {
            continue;
        };
        let words = split_words(value);
        if key.eq_ignore_ascii_case("host") {
            for word in &words {
                if !word.is_empty() && !word.contains(['*', '?', '!']) {
                    found(word);
                }
            }
        } else if key.eq_ignore_ascii_case("include") && depth < MAX_INCLUDE_DEPTH {
            for word in &words {
                for file in include_paths(word, home, ssh_dir) {
                    read_file(&file, home, ssh_dir, depth + 1, found);
                }
            }
        }
    }
}

/// 一行拆成关键字和值：关键字和值之间是空白或 `=`，`#` 开头的和空行没有。
fn split_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let (key, rest) = line.split_at(end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    Some((key, rest))
}

/// 按空白拆开值，双引号括起来的算一个词。
fn split_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in value.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    words.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        words.push(word);
    }
    words
}

/// `Include` 的一个参数指的文件：`~` 换成家目录，相对路径按 `~/.ssh` 算；最后一段可以带 `*` 和 `?`，
/// 匹配到的文件按名字排。
fn include_paths(pattern: &str, home: &Path, ssh_dir: &Path) -> Vec<PathBuf> {
    let path = match pattern.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if Path::new(pattern).is_absolute() => PathBuf::from(pattern),
        None => ssh_dir.join(pattern),
    };
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return vec![path];
    };
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_str().is_some_and(|file| glob(name, file)))
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files
}

/// `*` 配任意多个字符、`?` 配一个字符的通配。
fn glob(pattern: &str, text: &str) -> bool {
    let (pattern, text): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_keywords_and_quoted_words() {
        assert_eq!(split_line("  Host = a b "), Some(("Host", "a b")));
        assert_eq!(split_line("HostName\tx"), Some(("HostName", "x")));
        assert_eq!(split_line("# Host x"), None);
        assert_eq!(split_words(r#"a "b c" d"#), ["a", "b c", "d"]);
    }

    #[test]
    fn globs() {
        assert!(glob("*.conf", "a.conf"));
        assert!(glob("h?st", "host"));
        assert!(!glob("*.conf", "a.conf.bak"));
        assert!(glob("*", ""));
    }
}
