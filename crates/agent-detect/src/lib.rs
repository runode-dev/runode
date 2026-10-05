//! 认出终端前台在跑哪个 AI 编程 agent，以及它在干活、空闲还是在等用户回答。
//!
//! - `identify`：按前台进程组认 agent，包括 node、bun、python 包着跑的。
//! - `rules`、`region`：识别规则的格式和求值，规则拿屏幕底部的文字、标题和进度报告去比。
//! - `book`：各 agent 用哪份规则，内置的编进二进制，用户可以放同名文件覆盖。
//! - `title`：agent 在标题前缀里自己报告的状态。
//! - `osc`：从输出里留下 OSC 9 报告的原文给规则用。
//! - `tracker`：把这些合成对外的状态，并去抖、节流。
//!
//! 这里不碰终端仿真、PTY 和界面：屏幕文字、前台进程都由调用方读好了交进来。

mod book;
mod identify;
mod osc;
mod region;
mod rules;
pub mod title;
mod tracker;

pub use book::RuleBook;
pub use identify::{ForegroundJob, ForegroundProcess, agent_from_name, identify_job};
pub use rules::{ENGINE_VERSION, RuleSet, Signals, Verdict};
pub use tracker::{
    BURST_GAP, BURST_MIN, EVAL_INTERVAL, Foreground, IDLE_CAP, IDLE_CONFIRMATIONS, IDLE_RECHECK, STARTUP_GRACE, Tracker,
};

#[cfg(test)]
mod screens;
