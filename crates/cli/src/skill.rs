//! `runode skill install`：把编进这一版的 skill 装给这台电脑上的 agent（`runode_agent_skill`），没有桌面
//! 界面的机器（SSH 远端）上也能装。不经宿主。

use std::io::Write;

use crate::{Env, output::tilde};

pub(crate) fn install(env: &Env, out: &mut dyn Write) -> anyhow::Result<()> {
    let skill_env = runode_agent_skill::Env::from_dirs(&env.dirs, |key| std::env::var_os(key));
    let home = env.dirs.home.as_deref();
    for report in runode_agent_skill::install_all(&skill_env)? {
        let canonical = tilde(&report.canonical, home);
        if report.left_as_is {
            writeln!(out, "Left the {} skill as it is: {canonical} is a link you manage.", report.skill)?;
        } else {
            writeln!(out, "Installed the {} skill in {canonical}", report.skill)?;
        }
        for (agent, dir) in &report.linked {
            writeln!(out, "{agent} reads it from {}", tilde(dir, home))?;
        }
        for path in &report.skipped {
            writeln!(out, "Skipped {}: something of yours is there, left untouched.", tilde(path, home))?;
        }
    }
    Ok(())
}
