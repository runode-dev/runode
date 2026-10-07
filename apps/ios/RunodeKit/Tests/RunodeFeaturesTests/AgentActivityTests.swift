import Foundation
import RunodeActivity
import RunodeConnection
import RunodeProtocol
import Testing

@testable import RunodeFeatures

/// 假的 Live Activity：记下开、更新、结束了几次。
@MainActor
final class FakeActivityDriver: AgentActivityDriver {
    var isAllowed = true
    var isShowing = false
    var failsToStart = false
    var starts: [AgentActivityContent] = []
    var updates: [(content: AgentActivityContent, staleDate: Date?)] = []
    var ends = 0

    /// 现在显示着的内容：最后一次开或者更新的。
    var current: AgentActivityContent? {
        guard isShowing else { return nil }
        return updates.last?.content ?? starts.last
    }

    func start(_ content: AgentActivityContent) throws {
        if failsToStart { throw CancellationError() }
        isShowing = true
        updates = []
        starts.append(content)
    }

    func update(_ content: AgentActivityContent, staleDate: Date?) async {
        updates.append((content, staleDate))
    }

    func end() async {
        isShowing = false
        ends += 1
    }
}

/// 假的后台时间：申请总能成，到期由测试来触发。
@MainActor
final class FakeBackgroundTime: BackgroundTime {
    var expiration: (@MainActor @Sendable () async -> Void)?
    var ends = 0

    func begin(expiration: @escaping @MainActor @Sendable () async -> Void) -> Bool {
        self.expiration = expiration
        return true
    }

    func end() {
        expiration = nil
        ends += 1
    }

    func expire() async {
        let expiration = self.expiration
        self.expiration = nil
        await expiration?()
    }
}

private let blocked = SessionId("11111111111111111111111111111111")!
private let working = SessionId("22222222222222222222222222222222")!
private let idle = SessionId("33333333333333333333333333333333")!
private let shell = SessionId("44444444444444444444444444444444")!

private func info(_ id: SessionId, _ state: AgentState?, title: String? = nil, exited: Bool = false) -> SessionInfo {
    SessionInfo(
        id: id, size: smallGrid,
        meta: SessionMeta(title: title ?? "\(id)", agent: state.map { Agent(kind: AgentKind("claude"), state: $0) }),
        exited: exited)
}

private func content(_ states: [AgentActivityState]) -> AgentActivityContent {
    AgentActivityContent(
        summarizing: states.enumerated().map { index, state in
            AgentActivityContent.Entry(id: "\(index)", machine: "MacBook", title: "t", agent: "Claude Code", state: state)
        })
}

@Suite struct AgentActivityContentMappingTests {
    @Test func onlyLiveSessionsWithKnownAgentStatesCount() {
        let first = UUID()
        let second = UUID()
        let content = AgentActivityContent(machines: [
            (
                first, "MacBook",
                [
                    info(working, .working, title: "构建"), info(shell, nil), info(idle, .unknown("thinking")),
                    info(sessionA, .blocked, exited: true),
                ]
            ),
            (second, "homelab", [info(blocked, .blocked, title: "部署"), info(idle, .idle, title: "")]),
        ])
        #expect(content.blocked == 1)
        #expect(content.working == 1)
        #expect(content.idle == 1)
        #expect(
            content.entries == [
                .init(id: "\(second)/\(blocked)", machine: "homelab", title: "部署", agent: "Claude Code", state: .blocked),
                // 在干活的带上 Claude 自己转圈里的中间那帧和它的颜色。
                .init(
                    id: "\(first)/\(working)", machine: "MacBook", title: "构建", agent: "Claude Code", state: .working,
                    spinner: "✽", spinnerColor: 0xD77757),
                // 没有标题的会话照会话列表那样叫「终端」。
                .init(id: "\(second)/\(idle)", machine: "homelab", title: "终端", agent: "Claude Code", state: .idle),
            ])
    }
}

@MainActor
@Suite struct AgentActivityModelTests {
    let driver = FakeActivityDriver()
    let staleAt = Date(timeIntervalSince1970: 1_800_000_000)

    func model(enabled: Bool = true) -> AgentActivityModel {
        let staleAt = self.staleAt
        return AgentActivityModel(driver: driver, enabled: enabled, now: { staleAt })
    }

    /// 模型送出去的内容：整体在干活时带上开始干活的时刻（测试里 `now` 一直是 `staleAt`）。
    func sent(_ states: [AgentActivityState]) -> AgentActivityContent {
        var sent = content(states)
        if sent.overall == .working { sent.workingSince = staleAt }
        return sent
    }

    /// 整体一直在干活时沿用开始的时刻；中间停下来等回答过，再干活时重新算。
    @Test func workingSinceStaysWhileStillWorking() async {
        var clock = staleAt
        let model = AgentActivityModel(driver: driver, enabled: true, now: { clock })
        model.refresh(content([.working]), settled: true)
        await model.settle()
        #expect(driver.current?.workingSince == staleAt)
        clock = staleAt.addingTimeInterval(30)
        model.refresh(content([.working, .working]), settled: true)
        await model.settle()
        #expect(driver.current?.workingSince == staleAt)
        model.refresh(content([.blocked]), settled: true)
        await model.settle()
        #expect(driver.current?.workingSince == nil)
        clock = staleAt.addingTimeInterval(60)
        model.refresh(content([.working]), settled: true)
        await model.settle()
        #expect(driver.current?.workingSince == clock)
    }

    @Test func startsOnceAndOnlySendsChanges() async {
        let model = model()
        model.refresh(content([.working]), settled: true)
        await model.settle()
        #expect(driver.starts == [sent([.working])])
        // 内容没变不发。
        model.refresh(content([.working]), settled: true)
        await model.settle()
        #expect(driver.updates.isEmpty)
        model.refresh(content([.blocked]), settled: true)
        await model.settle()
        #expect(driver.starts.count == 1)
        #expect(driver.updates.map(\.content) == [content([.blocked])])
        #expect(driver.updates.last?.staleDate == nil)
    }

    @Test func endsWhenNoAgentIsLeft() async {
        let model = model()
        model.refresh(content([.working]), settled: true)
        await model.settle()
        // 还在连、不知道有没有 agent：先不结束。
        model.refresh(content([]), settled: false)
        await model.settle()
        #expect(driver.ends == 0)
        model.refresh(content([]), settled: true)
        await model.settle()
        #expect(driver.ends == 1)
        #expect(!driver.isShowing)
        #expect(model.shown == nil)
        // 已经结束了的不再结束一次。
        model.refresh(content([]), settled: true)
        await model.settle()
        #expect(driver.ends == 1)
    }

    @Test func respectsTheSettingAndTheSystem() async {
        let model = model(enabled: false)
        model.refresh(content([.working]), settled: true)
        await model.settle()
        #expect(driver.starts.isEmpty)
        model.enabled = true
        await model.settle()
        #expect(driver.starts.count == 1)
        // 在设置里关掉：马上结束。
        model.enabled = false
        await model.settle()
        #expect(driver.ends == 1)
        // 系统不允许时不开。
        driver.isAllowed = false
        model.enabled = true
        await model.settle()
        #expect(driver.starts.count == 1)
    }

    @Test func onlyStartsInTheForeground() async {
        let model = model()
        model.setForeground(false)
        model.refresh(content([.working]), settled: true)
        await model.settle()
        #expect(driver.starts.isEmpty)
        model.setForeground(true)
        await model.settle()
        #expect(driver.starts.count == 1)
        // 开着的在后台照样更新。
        model.setForeground(false)
        model.refresh(content([.blocked]), settled: true)
        await model.settle()
        #expect(driver.current == content([.blocked]))
    }

    /// 进了后台：内容不变也补发一次带过时时刻的，App 被杀掉、来不及改成暂停时系统到点也会把它当成过时的；
    /// 回到前台去掉过时时刻。
    @Test func backgroundUpdatesCarryAStaleDate() async {
        let model = model()
        model.refresh(content([.working]), settled: true)
        await model.settle()
        model.setForeground(false)
        await model.settle()
        let staleDate = staleAt.addingTimeInterval(AgentActivityModel.backgroundStaleAfter)
        #expect(driver.updates.map(\.content) == [sent([.working])])
        #expect(driver.updates.last?.staleDate == staleDate)
        model.refresh(content([.blocked]), settled: true)
        await model.settle()
        #expect(driver.updates.last?.staleDate == staleDate)
        model.setForeground(true)
        await model.settle()
        #expect(driver.updates.last?.staleDate == nil)
        #expect(driver.current == content([.blocked]))
    }

    @Test func aFailedStartIsRetriedOnTheNextChange() async {
        let model = model()
        driver.failsToStart = true
        model.refresh(content([.working]), settled: true)
        await model.settle()
        #expect(model.shown == nil)
        driver.failsToStart = false
        model.refresh(content([.working, .idle]), settled: true)
        await model.settle()
        #expect(driver.starts == [sent([.working, .idle])])
    }

    @Test func suspendingShowsTheLastStateAsPaused() async {
        let model = model()
        model.refresh(content([.blocked, .working]), settled: true)
        await model.settle()
        model.setForeground(false)
        model.suspend()
        await model.settle()
        #expect(driver.current == content([.blocked, .working]).asPaused)
        #expect(driver.updates.last?.staleDate == staleAt)
        // 停着的时候列表上的变化不算（连接停了，那些是旧的）。
        model.refresh(content([]), settled: true)
        await model.settle()
        #expect(driver.ends == 0)
        // 回到前台、还在重连：先留着暂停的样子；连上以后跟着实时的走。
        model.setForeground(true)
        model.resume()
        model.refresh(content([]), settled: false)
        await model.settle()
        #expect(driver.current?.paused == true)
        model.refresh(content([.working]), settled: true)
        await model.settle()
        #expect(driver.current == sent([.working]))
        #expect(driver.updates.last?.staleDate == nil)
    }

    @Test func leftoversAreReusedOrEnded() async {
        // 上次运行留下了一个：有 agent 时接着用，不再开新的。
        driver.isShowing = true
        let reused = model()
        reused.refresh(content([.working]), settled: true)
        await reused.settle()
        #expect(driver.starts.isEmpty)
        #expect(driver.updates.map(\.content) == [sent([.working])])
        // 没有 agent 时结束掉。
        let other = FakeActivityDriver()
        other.isShowing = true
        let ended = AgentActivityModel(driver: other, enabled: true)
        ended.refresh(content([]), settled: true)
        await ended.settle()
        #expect(other.ends == 1)
    }
}

@MainActor
@Suite struct AppAgentActivityTests {
    let machine = machineRecord()
    let link = FakeLink()
    let driver = FakeActivityDriver()
    let background = FakeBackgroundTime()
    let preferences = DefaultsStore<AppPreferences>("preferences", defaults: .temporary())

    func app() async throws -> (AppModel, SessionListModel) {
        let store = MemoryMachineStore()
        await store.upsert(machine)
        let link = self.link
        var dependencies = AppDependencies(
            store: store, keyStore: MemoryDeviceKeyStore(), pairing: FakePairing { _ in machineRecord() },
            makeLink: { _ in link }, deviceName: "测试 iPhone", preferences: preferences)
        dependencies.agentActivity = driver
        dependencies.backgroundTime = background
        let app = AppModel(dependencies: dependencies)
        await app.machineList.load()
        let list = try #require(app.sessionList(for: machine.id))
        list.handle(.state(.connected(hostName: "MacBook", address: nil)))
        list.handle(.ready(generation: 1))
        list.handle(.message(.sessionList([info(blocked, .blocked, title: "部署"), info(shell, nil)])))
        return (app, list)
    }

    @Test func agentsOnConnectedMachinesAreShown() async throws {
        // `AppModel` 要留着：会话列表经弱引用回调它。
        let (app, list) = try await app()
        #expect(await eventually { driver.current?.blocked == 1 })
        #expect(driver.current?.entries.map(\.title) == ["部署"])
        #expect(driver.current?.entries.map(\.machine) == [machine.name])
        // agent 都没了就结束。
        list.handle(.message(.sessionList([info(shell, nil)])))
        #expect(await eventually { driver.ends == 1 })
        withExtendedLifetime(app) {}
    }

    @Test func backgroundKeepsTheConnectionUntilTimeRunsOut() async throws {
        let (app, list) = try await app()
        #expect(await eventually { driver.current != nil })
        #expect(await eventually { link.starts == 1 })
        app.setActive(false)
        try await Task.sleep(for: .milliseconds(20))
        #expect(link.stops == 0)
        // 多要的时间里照常更新。
        list.handle(.message(.sessionList([info(blocked, .working, title: "部署")])))
        #expect(await eventually { driver.current?.working == 1 })
        // 时间到了：断开，显示停之前的样子并标成已暂停。
        await background.expire()
        #expect(await eventually { link.stops == 1 })
        #expect(driver.current?.paused == true)
        #expect(driver.current?.working == 1)
        #expect(driver.updates.last?.staleDate != nil)
        // 回到前台照原来那样重连。
        app.setActive(true)
        #expect(await eventually { link.starts == 2 })
        #expect(background.ends == 1)
    }

    @Test func comingBackBeforeTimeRunsOutKeepsTheConnection() async throws {
        let (app, _) = try await app()
        #expect(await eventually { link.starts == 1 })
        app.setActive(false)
        app.setActive(true)
        #expect(background.ends == 1)
        try await Task.sleep(for: .milliseconds(20))
        #expect(link.stops == 0)
        #expect(link.starts == 1)
    }

    @Test func turningTheSettingOffEndsIt() async throws {
        let (app, _) = try await app()
        #expect(await eventually { driver.current != nil })
        app.settings.preferences.showsAgentActivity = false
        #expect(await eventually { driver.ends == 1 })
        #expect(preferences.load()?.showsAgentActivity == false)
    }
}
