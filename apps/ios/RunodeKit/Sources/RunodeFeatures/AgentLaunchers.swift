import RunodeProtocol

/// 新开终端时能直接起的一家 agent：菜单上的名字、按先后试的程序名（前一个在电脑上找不到才用后一个）和
/// 认出来的种类（挑 logo 用）。清单以宿主的 `runode_agent::models::LAUNCHERS` 为准，测试对着它生成的样例核对。
public struct AgentLauncher: Hashable, Sendable {
    public let label: String
    public let programs: [String]
    public let kind: AgentKind

    public static let all: [AgentLauncher] = [
        AgentLauncher(label: "Claude", programs: ["claude"], kind: AgentKind("claude")),
        AgentLauncher(label: "Codex", programs: ["codex"], kind: AgentKind("codex")),
        AgentLauncher(label: "Pi", programs: ["pi"], kind: AgentKind("pi")),
        AgentLauncher(label: "OMP", programs: ["omp"], kind: AgentKind("omp")),
        AgentLauncher(label: "OpenCode", programs: ["opencode2", "opencode"], kind: AgentKind("open_code")),
        AgentLauncher(label: "GitHub Copilot", programs: ["copilot"], kind: AgentKind("github_copilot")),
        AgentLauncher(label: "Cursor", programs: ["cursor-agent"], kind: AgentKind("cursor")),
        AgentLauncher(label: "Antigravity", programs: ["agy"], kind: AgentKind("antigravity")),
        AgentLauncher(label: "Muse", programs: ["muse"], kind: AgentKind("muse")),
    ]

    /// 要宿主找的命令：各家的程序名，每个一次。
    static let programs: [String] = all.flatMap(\.programs).reduce(into: []) { programs, program in
        if !programs.contains(program) { programs.append(program) }
    }

    /// 宿主找到了命令的各家，按 `all` 的先后。`found` 和 `programs` 一一对应，是宿主找到的命令文件名；
    /// 起 agent 时打这个文件名：Windows 上带着扩展名，PowerShell 只打程序名时先跑 npm 放的 `.ps1`，执行策略
    /// 不让跑脚本时就报错。
    static func installed(found: [String?]) -> [InstalledAgent] {
        all.compactMap { launcher in
            let command = launcher.programs.lazy.compactMap { program in
                programs.firstIndex(of: program).flatMap { found.indices.contains($0) ? found[$0] : nil }
            }.first
            return command.map { InstalledAgent(label: launcher.label, kind: launcher.kind, command: $0) }
        }
    }
}

/// 电脑上装着的一家 agent：新开终端后在 shell 里打 `command` 起它。
public struct InstalledAgent: Hashable, Sendable, Identifiable {
    public let label: String
    public let kind: AgentKind
    public let command: String

    public var id: String { label }

    public init(label: String, kind: AgentKind, command: String) {
        self.label = label
        self.kind = kind
        self.command = command
    }
}
