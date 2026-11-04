import Foundation
import RunodeProtocol
import Testing

@testable import RunodeFeatures

@Suite struct AgentLauncherTests {
    /// 菜单里的各家和宿主那边一样：取自 `runode_agent` 的测试 `launcher_fixture` 生成的样例。
    @Test func matchesTheHostLaunchers() throws {
        struct Root: Decodable {
            struct Launcher: Decodable {
                let label: String
                let programs: [String]
                let kind: String
            }
            let launchers: [Launcher]
        }
        let fixture = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appending(path: "crates/agent/tests/fixtures/launchers.json")
        let launchers = try JSONDecoder().decode(Root.self, from: Data(contentsOf: fixture)).launchers
        try #require(!launchers.isEmpty)
        #expect(AgentLauncher.all.map(\.label) == launchers.map(\.label))
        #expect(AgentLauncher.all.map(\.programs) == launchers.map(\.programs))
        #expect(AgentLauncher.all.map(\.kind.label) == launchers.map(\.kind))
    }

    /// 每家列一个：先试的程序找不到时换同一家的下一个；起它时打宿主找到的文件名。
    @Test func listsTheFirstProgramFoundForEachAgent() {
        let programs = AgentLauncher.programs
        #expect(Set(programs).count == programs.count)
        var found: [String?] = programs.map { _ in nil }
        found[programs.firstIndex(of: "codex")!] = "codex.cmd"
        found[programs.firstIndex(of: "opencode")!] = "opencode"
        #expect(
            AgentLauncher.installed(found: found) == [
                InstalledAgent(label: "Codex", kind: AgentKind("codex"), command: "codex.cmd"),
                InstalledAgent(label: "OpenCode", kind: AgentKind("open_code"), command: "opencode"),
            ])
        found[programs.firstIndex(of: "opencode2")!] = "opencode2"
        #expect(AgentLauncher.installed(found: found).map(\.command) == ["codex.cmd", "opencode2"])
        #expect(AgentLauncher.installed(found: []).isEmpty)
    }
}
