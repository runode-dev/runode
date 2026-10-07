//! 嵌进二进制的命令规格：构建脚本把每个命令的 JSON 压缩后接成一整块，另有一张按命令名排序的
//! 索引。用到某个命令时才解压、解析，结果连同「没有这个命令的规格」一起按命令名缓存。

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock, PoisonError},
};

use warp_command_signatures::{DynamicCompletionData, Signature, fig_types};

static BLOB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/command_specs.bin"));
/// `(命令名, 在 BLOB 里的偏移, 长度, 命令的说明)`，按命令名排序。
static INDEX: &[(&str, usize, usize, &str)] = include!(concat!(env!("OUT_DIR"), "/command_specs.rs"));

/// 一个命令的规格。
pub struct Spec {
    pub signature: Signature,
    pub flags: Flags,
}

/// 和别的命令同一个可执行文件的短名字，按它指的那个命令补全：`rn` 是 runode 放在自己旁边的
/// 符号链接。
const ALIASES: &[(&str, &str)] = &[("rn", "runode")];

/// `name` 是短名字时换成它指的命令。
fn canonical(name: &str) -> &str {
    ALIASES.iter().find(|(alias, _)| *alias == name).map_or(name, |(_, command)| command)
}

/// 名为 `name` 的命令的规格；没有时为 `None`。
pub fn lookup(name: &str) -> Option<Arc<Spec>> {
    let name = canonical(name);
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<Spec>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(spec) = cache.lock().unwrap_or_else(PoisonError::into_inner).get(name) {
        return spec.clone();
    }
    let spec = load(name).map(Arc::new);
    cache.lock().unwrap_or_else(PoisonError::into_inner).insert(name.to_owned(), spec.clone());
    spec
}

/// 规格里命令本身的说明；没有规格或者规格里没写时为 `None`。不用解压规格。
pub fn description(name: &str) -> Option<&'static str> {
    let name = canonical(name);
    let i = INDEX.binary_search_by(|(n, ..)| (*n).cmp(name)).ok()?;
    Some(INDEX[i].3).filter(|description| !description.is_empty())
}

/// 动态补全的数据（生成器和过滤函数），按命令名查；第一次用到时建好。runode 自己的命令行的
/// 生成器在 `runode_cli` 里。
pub fn dynamic(command: &str) -> Option<&'static DynamicCompletionData> {
    static DATA: OnceLock<HashMap<String, DynamicCompletionData>> = OnceLock::new();
    DATA.get_or_init(|| {
        let mut data = warp_command_signatures::dynamic_command_signature_data();
        data.extend([crate::runode_cli::generators().into()]);
        data
    })
    .get(canonical(command))
}

/// 嵌进来的所有命令名。
#[cfg(test)]
pub fn names() -> impl Iterator<Item = &'static str> {
    INDEX.iter().map(|(name, ..)| *name)
}

fn load(name: &str) -> Option<Spec> {
    let i = INDEX.binary_search_by(|(n, ..)| (*n).cmp(name)).ok()?;
    let (_, offset, len, _) = INDEX[i];
    let json = match miniz_oxide::inflate::decompress_to_vec(&BLOB[offset..offset + len]) {
        Ok(json) => json,
        Err(err) => {
            tracing::warn!("failed to inflate the command spec for {name}: {err:?}");
            return None;
        }
    };
    let command: fig_types::Command = match serde_json::from_slice(&json) {
        Ok(command) => command,
        Err(err) => {
            tracing::warn!("failed to parse the command spec for {name}: {err}");
            return None;
        }
    };
    // 转成 `Signature` 时这几项会丢掉，先记下来。
    let mut flags = Flags::default();
    flags.collect(&command);
    let signatures: Vec<Signature> = command.into();
    let signature = signatures.into_iter().find(|s| s.name == name)?;
    Some(Spec { signature, flags })
}

/// 规格里转成 `Signature` 时会丢掉的几项。
#[derive(Default)]
pub struct Flags {
    /// 值要写成 `--name=value` 的选项名。
    pub requires_equals: HashSet<String>,
    /// 一条命令里可以出现多次的选项名。
    pub repeatable: HashSet<String>,
    /// 规格里标为隐藏的选项和子命令名，只在完整输入时才列出来。
    pub hidden: HashSet<String>,
}

impl Flags {
    /// 整棵命令树里的选项和子命令都算进来，不分在哪一层。
    fn collect(&mut self, command: &fig_types::Command) {
        for option in &command.options {
            let repeatable = match option.is_repeatable {
                Some(fig_types::NumberOrBool::Bool(repeatable)) => repeatable,
                Some(fig_types::NumberOrBool::Number(times)) => times > 1,
                None => false,
            };
            for name in &option.name {
                if option.requires_equals {
                    self.requires_equals.insert(name.clone());
                }
                if repeatable {
                    self.repeatable.insert(name.clone());
                }
                if option.hidden {
                    self.hidden.insert(name.clone());
                }
            }
        }
        for subcommand in &command.subcommands {
            if subcommand.hidden {
                self.hidden.extend(subcommand.name.iter().cloned());
            }
            self.collect(subcommand);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_spec_parses() {
        let names: Vec<&str> = names().collect();
        assert!(names.len() > 400, "only {} specs embedded", names.len());
        for name in names {
            assert!(lookup(name).is_some(), "the spec for {name} does not parse");
        }
    }

    #[test]
    fn looks_up_git_and_caches_misses() {
        let git = lookup("git").unwrap();
        assert!(git.signature.subcommands().iter().any(|s| s.name == "checkout"));
        assert!(lookup("no-such-command-here").is_none());
        assert!(lookup("no-such-command-here").is_none());
        assert_eq!(description("git"), Some("The stupid content tracker"));
        assert_eq!(description("no-such-command-here"), None);
        assert!(dynamic("git").is_some_and(|data| !data.generators().is_empty()));
    }

    #[test]
    fn rn_completes_as_runode() {
        assert!(Arc::ptr_eq(&lookup("rn").unwrap(), &lookup("runode").unwrap()));
        assert!(dynamic("rn").is_some());
        assert_eq!(description("rn"), description("runode"));
    }

    #[test]
    fn runode_has_its_own_spec_and_generators() {
        let runode = lookup("runode").unwrap();
        let subcommands: Vec<&str> = runode.signature.subcommands().iter().map(|s| s.name.as_str()).collect();
        assert!(
            ["list", "read", "send", "wait", "open", "kill", "focus", "setup", "remote"]
                .iter()
                .all(|name| subcommands.contains(name))
        );
        assert!(runode.flags.repeatable.contains("--key"));
        let generators = dynamic("runode").unwrap().generators();
        assert!(generators.contains_key(&"sessions".into()) && generators.contains_key(&"devices".into()));
    }
}
