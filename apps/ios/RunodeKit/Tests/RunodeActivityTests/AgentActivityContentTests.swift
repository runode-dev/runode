import Foundation
import RunodeActivity
import Testing

private func entry(_ id: String, _ state: AgentActivityState, machine: String = "MacBook", title: String? = nil)
    -> AgentActivityContent.Entry
{
    AgentActivityContent.Entry(id: id, machine: machine, title: title ?? id, agent: "Claude Code", state: state)
}

@Suite struct AgentActivityContentTests {
    @Test func countsEveryStateAndKeepsTheMostUrgentEntries() {
        let content = AgentActivityContent(summarizing: [
            entry("a", .idle), entry("b", .working), entry("c", .blocked), entry("d", .working), entry("e", .idle),
            entry("f", .blocked),
        ])
        #expect(content.blocked == 2)
        #expect(content.working == 2)
        #expect(content.idle == 2)
        // 等回答的在前，再是在干活的，同一种保持原来的先后；只留四条。
        #expect(content.entries.map(\.id) == ["c", "f", "b", "d"])
        #expect(!content.paused)
        #expect(!content.isEmpty)
    }

    @Test func idleEntriesFillTheRemainingRows() {
        let content = AgentActivityContent(summarizing: [entry("a", .idle), entry("b", .working)])
        #expect(content.entries.map(\.id) == ["b", "a"])
    }

    @Test func noAgentsIsEmpty() {
        let content = AgentActivityContent(summarizing: [])
        #expect(content.isEmpty)
        #expect(content == AgentActivityContent())
    }

    @Test func theHeadlinePrefersWaitingThenWorking() {
        let waiting = AgentActivityContent(summarizing: [entry("a", .working), entry("b", .blocked)])
        #expect(waiting.overall == .blocked)
        #expect(waiting.headline == 1)
        let working = AgentActivityContent(summarizing: [entry("a", .working), entry("b", .working), entry("c", .idle)])
        #expect(working.overall == .working)
        #expect(working.headline == 2)
        let idle = AgentActivityContent(summarizing: [entry("a", .idle)])
        #expect(idle.overall == .idle)
        #expect(idle.headline == 1)
    }

    @Test func longTextIsClipped() throws {
        let long = String(repeating: "长", count: 100)
        let content = AgentActivityContent(summarizing: [entry("a", .working, machine: long, title: long)])
        let clipped = try #require(content.entries.first)
        #expect(clipped.title.count == AgentActivityContent.textLimit)
        #expect(clipped.title.hasSuffix("…"))
        #expect(clipped.machine.count == AgentActivityContent.textLimit)
    }

    @Test func pausedKeepsTheContent() throws {
        let content = AgentActivityContent(summarizing: [entry("a", .blocked)])
        let paused = content.asPaused
        #expect(paused.paused)
        #expect(paused.entries == content.entries)
        // 两边经 JSON 传，存取前后一样。
        let decoded = try JSONDecoder().decode(AgentActivityContent.self, from: JSONEncoder().encode(paused))
        #expect(decoded == paused)
    }
}
