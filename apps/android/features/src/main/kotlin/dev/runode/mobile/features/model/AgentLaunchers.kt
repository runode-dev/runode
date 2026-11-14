package dev.runode.mobile.features.model

import dev.runode.mobile.protocol.AgentKind

/**
 * 新开终端时能直接起的一家 agent：菜单上的名字、按先后试的程序名（前一个在电脑上找不到才用后一个）和认出来的种类
 * （挑 logo 用）。清单以宿主的 `runode_agent::models::LAUNCHERS` 为准，测试对着它生成的样例核对。
 */
data class AgentLauncher(val label: String, val programs: List<String>, val kind: AgentKind) {
    companion object {
        val all: List<AgentLauncher> = listOf(
            AgentLauncher("Claude", listOf("claude"), AgentKind("claude")),
            AgentLauncher("Codex", listOf("codex"), AgentKind("codex")),
            AgentLauncher("Pi", listOf("pi"), AgentKind("pi")),
            AgentLauncher("OMP", listOf("omp"), AgentKind("omp")),
            AgentLauncher("OpenCode", listOf("opencode2", "opencode"), AgentKind("open_code")),
            AgentLauncher("GitHub Copilot", listOf("copilot"), AgentKind("github_copilot")),
            AgentLauncher("Cursor", listOf("cursor-agent"), AgentKind("cursor")),
            AgentLauncher("Antigravity", listOf("agy"), AgentKind("antigravity")),
            AgentLauncher("Muse", listOf("muse"), AgentKind("muse")),
        )

        /** 要宿主找的命令：各家的程序名，每个一次。 */
        internal val programs: List<String> = all.flatMap { it.programs }.distinct()

        /**
         * 宿主找到了命令的各家，按 [all] 的先后。[found] 和 [programs] 一一对应，是宿主找到的命令文件名；起 agent 时
         * 打这个文件名：Windows 上带着扩展名，PowerShell 只打程序名时先跑 npm 放的 `.ps1`，执行策略不让跑脚本时就报错。
         */
        internal fun installed(found: List<String?>): List<InstalledAgent> = all.mapNotNull { launcher ->
            val command = launcher.programs.firstNotNullOfOrNull { program ->
                found.getOrNull(programs.indexOf(program))
            }
            command?.let { InstalledAgent(launcher.label, launcher.kind, it) }
        }
    }
}

/** 电脑上装着的一家 agent：新开终端后在 shell 里打 [command] 起它。 */
data class InstalledAgent(val label: String, val kind: AgentKind, val command: String) {
    val id: String get() = label
}
