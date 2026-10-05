//! 命令历史里各个命令名、各个「命令 + 子命令」的常用程度，补全菜单按它把常用的排在前面。
//!
//! 常用程度是去重后的历史（同一条命令只算一条）里有几条以它开头：`git status` 和
//! `git status -s` 各算 `git` 一次、`git status` 一次。历史一变才重新统计，统计一遍要把全部
//! 历史（最多 `history::LIMIT` 条）切一次词，平时按键直接用上次的结果。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, PoisonError},
};

use runode_terminal::history;

use super::line;

/// 各个命令名和「命令 + 子命令」在历史里出现了几条。
#[derive(Debug, Default)]
pub struct Usage {
    commands: HashMap<String, u32>,
    subcommands: HashMap<(String, String), u32>,
}

impl Usage {
    /// 从一条条命令统计。命令名跳过开头的变量赋值，写成路径时取最后一段；子命令是命令名后面
    /// 第一个不以 `-` 开头的词。
    pub fn from_commands<'a>(commands: impl IntoIterator<Item = &'a str>) -> Self {
        let mut usage = Self::default();
        for command in commands {
            let mut words = command.split_whitespace().skip_while(|word| line::is_assignment(word));
            let Some(name) = words.next() else {
                continue;
            };
            let name = name.rsplit('/').next().unwrap_or(name).to_owned();
            if let Some(sub) = words.find(|word| !word.starts_with('-')) {
                *usage.subcommands.entry((name.clone(), sub.to_owned())).or_default() += 1;
            }
            *usage.commands.entry(name).or_default() += 1;
        }
        usage
    }

    pub fn command(&self, name: &str) -> u32 {
        self.commands.get(name).copied().unwrap_or(0)
    }

    pub fn subcommand(&self, command: &str, subcommand: &str) -> u32 {
        self.subcommands.get(&(command.to_owned(), subcommand.to_owned())).copied().unwrap_or(0)
    }
}

/// 按现在的命令历史统计的常用程度；历史没变时直接用上次的结果。
pub fn current() -> Arc<Usage> {
    static CACHE: OnceLock<Mutex<(u64, Arc<Usage>)>> = OnceLock::new();
    let history = history::shared();
    let generation = history.generation();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(PoisonError::into_inner);
    if cache.0 != generation || generation == 0 {
        *cache = (generation, Arc::new(Usage::from_commands(history.commands())));
    }
    cache.1.clone()
}
