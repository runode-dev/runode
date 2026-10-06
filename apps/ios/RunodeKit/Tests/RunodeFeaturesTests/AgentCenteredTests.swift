import Foundation
import RunodeConnection
import RunodeProtocol
import Testing

@testable import RunodeFeatures

/// 一台假 Mac 上的三个会话：等回答的 Claude、干活的 Codex、普通 shell。
private let waiting = SessionId("11111111111111111111111111111111")!
private let working = SessionId("22222222222222222222222222222222")!
private let plain = SessionId("33333333333333333333333333333333")!

private func info(_ id: SessionId, _ state: AgentState?, exited: Bool = false, owner: String? = nil) -> SessionInfo {
    SessionInfo(
        id: id, size: smallGrid,
        meta: SessionMeta(title: "\(id)", agent: state.map { Agent(kind: AgentKind("claude"), state: $0) }),
        exited: exited, sizeOwner: owner)
}

/// 手动拨的钟，预览节流的测试用。
@MainActor
final class ManualClock {
    var now = ContinuousClock.now
    func advance(_ duration: Duration) { now += duration }
}

private func readScreens(_ sent: [ClientMsg]) -> [SessionId] {
    sent.compactMap {
        if case .readScreen(let id, _, _) = $0 { return id }
        return nil
    }
}

@MainActor
@Suite struct SessionGroupingTests {
    let link = FakeLink()

    @Test func waitingComesFirstThenWorkingThenTheRest() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(
            .message(
                .sessionList([
                    info(plain, nil), info(working, .working), info(waiting, .blocked),
                    info(SessionId("44444444444444444444444444444444")!, .idle),
                    // 退出了的不再算等回答。
                    info(SessionId("55555555555555555555555555555555")!, .blocked, exited: true),
                ])))
        let sections = model.sections
        #expect(sections.map(\.group) == [.waiting, .working, .other])
        #expect(sections[0].sessions.map(\.id) == [waiting])
        #expect(sections[1].sessions.map(\.id) == [working])
        #expect(sections[2].sessions.count == 3)
        // 组内按宿主给的先后。
        #expect(sections[2].sessions.first?.id == plain)
    }

    @Test func groupsFollowLiveAgentState() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(working, .working)])))
        #expect(model.sections.map(\.group) == [.working])
        model.handle(.message(.meta(id: working, meta: SessionMeta(agent: Agent(kind: AgentKind("codex"), state: .blocked)))))
        #expect(model.sections.map(\.group) == [.waiting])
        #expect(model.sections.isEmpty == false)
    }

    @Test func emptyGroupsAreLeftOut() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(plain, nil)])))
        #expect(model.sections.map(\.group) == [.other])
    }
}

@MainActor
@Suite struct PreviewTests {
    let link = FakeLink()
    let clock = ManualClock()

    func model() -> SessionListModel {
        let clock = self.clock
        let model = SessionListModel(
            machine: machineRecord(), link: link, previewInterval: .milliseconds(200), now: { clock.now })
        model.handle(.ready(generation: 1))
        return model
    }

    @Test func throttleAllowsOneReadPerInterval() {
        var throttle = PreviewThrottle(interval: .seconds(1))
        let start = ContinuousClock.now
        #expect(throttle.request(waiting, at: start) == .now)
        #expect(throttle.request(working, at: start) == .now)
        #expect(throttle.request(waiting, at: start + .milliseconds(300)) == .later(.milliseconds(700)))
        // 已经排着一次了：再来的合并进去。
        #expect(throttle.request(waiting, at: start + .milliseconds(400)) == .skip)
        throttle.fire(waiting, at: start + .seconds(1))
        #expect(throttle.request(waiting, at: start + .milliseconds(1500)) == .later(.milliseconds(500)))
        #expect(throttle.request(waiting, at: start + .seconds(3)) == .skip)
    }

    @Test func enteringTheListReadsEveryScreen() {
        let model = model()
        model.handle(.message(.sessionList([info(waiting, .blocked), info(plain, nil)])))
        #expect(Set(readScreens(link.sent)) == [waiting, plain])
        #expect(link.sent.contains(.readScreen(id: waiting, lines: SessionListModel.previewReadLines)))
    }

    @Test func metaAndBellRefreshButAtMostOncePerSecond() async {
        let model = model()
        model.handle(.message(.sessionList([info(waiting, .blocked)])))
        link.clearSent()
        // 刚读过：一个间隔内的变化合并成到点时的一次。
        model.handle(.message(.meta(id: waiting, meta: SessionMeta(title: "x"))))
        model.handle(.message(.bell(id: waiting)))
        model.handle(.message(.meta(id: waiting, meta: SessionMeta(title: "y"))))
        #expect(readScreens(link.sent).isEmpty)
        clock.advance(.seconds(2))
        model.handle(.message(.bell(id: waiting)))
        #expect(readScreens(link.sent).isEmpty, "the deferred read is already scheduled")
        // 排着的那次到点后发出去，只发一次。
        #expect(await eventually { readScreens(link.sent) == [waiting] })
    }

    @Test func screenTextBecomesThePreview() {
        let model = model()
        model.handle(.message(.sessionList([info(waiting, .blocked)])))
        let screen = "● 跑测试\n\n────────\n Do you want to proceed?\n ❯ 1. Yes   \n   2. No\n\n\n"
        model.handle(.message(.screenText(id: waiting, text: screen, truncated: false)))
        #expect(model.previews[waiting] == [" Do you want to proceed?", " ❯ 1. Yes", "   2. No"])
        // 不在列表里的会话的回话不收。
        model.handle(.message(.screenText(id: plain, text: "x", truncated: false)))
        #expect(model.previews[plain] == nil)
    }

    func idleShell() -> SessionInfo {
        var shell = info(plain, nil)
        shell.meta.foregroundIsShell = true
        return shell
    }

    /// shell 在提示符上等输入时读最近一条命令的输出（`command: 1`），宿主切掉了提示符，两行式提示符
    /// 也不会混进预览；agent 在跑的会话照旧读屏幕底部。
    @Test func idleShellsPreviewTheirLastCommand() {
        let model = model()
        model.handle(.message(.sessionList([idleShell(), info(waiting, .blocked)])))
        #expect(link.sent.contains(.readScreen(id: plain, lines: nil, command: 1)))
        #expect(link.sent.contains(.readScreen(id: waiting, lines: SessionListModel.previewReadLines)))
        let output = "Cargo.lock  apps\n\nREADME.md  crates\n"
        model.handle(.message(.screenText(id: plain, text: output, truncated: false)))
        #expect(model.previews[plain] == ["Cargo.lock  apps", "README.md  crates"])
    }

    /// 读命令输出出错（没有 shell 集成）：马上退回读屏幕底部，并去掉光标所在的提示符那一行。
    @Test func lastCommandErrorsFallBackToTheScreen() {
        let model = model()
        model.handle(.message(.sessionList([idleShell()])))
        link.clearSent()
        model.handle(.message(.error(req: nil, id: plain, message: "this needs shell integration")))
        #expect(link.sent == [.readScreen(id: plain, lines: SessionListModel.previewReadLines)])
        model.handle(.message(.screenText(id: plain, text: "ls\nCargo.lock  apps\n~/runode ❯\n", truncated: false)))
        #expect(model.previews[plain] == ["ls", "Cargo.lock  apps"])
        #expect(model.session(plain) != nil)
    }

    /// 上一条命令没有输出（`cd` 这类），或者还没跑过命令：同样退回读屏幕底部。
    @Test func emptyLastCommandFallsBackToTheScreen() {
        let model = model()
        model.handle(.message(.sessionList([idleShell()])))
        link.clearSent()
        model.handle(.message(.screenText(id: plain, text: "\n  \n", truncated: false)))
        #expect(link.sent == [.readScreen(id: plain, lines: SessionListModel.previewReadLines)])
        #expect(model.previews[plain] == nil)
        model.handle(.message(.screenText(id: plain, text: "make\nok\n~/runode ❯", truncated: false)))
        #expect(model.previews[plain] == ["make", "ok"])
    }

    /// 读屏幕失败（比如超时）不是会话没了，不能把它从列表里拿掉。
    @Test func readErrorsDoNotRemoveSessions() {
        let model = model()
        model.handle(.message(.sessionList([info(waiting, .blocked)])))
        model.handle(.message(.error(req: nil, id: waiting, message: "session \(waiting) did not answer")))
        #expect(model.session(waiting) != nil)
        model.handle(.message(.error(req: nil, id: waiting, message: "no session \(waiting)")))
        #expect(model.session(waiting) == nil)
    }

    @Test func nothingIsReadWhileDisconnected() {
        let model = model()
        model.handle(.message(.sessionList([info(waiting, .blocked)])))
        model.handle(.state(.waiting(reason: "断了", retryAt: .now)))
        link.clearSent()
        clock.advance(.seconds(5))
        model.refreshPreviews()
        #expect(readScreens(link.sent).isEmpty)
    }
}

@MainActor
@Suite struct QuickReplyTests {
    let link = FakeLink()

    @Test func keysGoOutAsSendKeys() async {
        let reply = QuickReplyModel(sessionId: waiting, link: link)
        await reply.press(QuickKey.standard.first { $0.key == "enter" }!)
        guard case .sendKeys(let req, waiting, ["enter"])? = link.sent.first else {
            Issue.record("expected send_keys enter, got \(link.sent)")
            return
        }
        #expect(reply.isSending)
        #expect(reply.handle(.done(req: req)))
        #expect(!reply.isSending)
        #expect(reply.deliveredCount == 1)
        // 别人的回话不收。
        #expect(!reply.handle(.done(req: req + 100)))
    }

    @Test func textIsPastedThenEntered() async {
        var delivered: [SessionId] = []
        let reply = QuickReplyModel(sessionId: waiting, link: link) { delivered.append($0) }
        reply.draft = "   "
        #expect(!reply.canSendDraft)
        await reply.sendDraft()
        #expect(link.sent.isEmpty)
        reply.draft = "用方案 2"
        await reply.sendDraft()
        #expect(reply.draft.isEmpty)
        guard link.sent.count == 2, case .paste(let paste, waiting, "用方案 2") = link.sent[0],
            case .sendKeys(let enter, waiting, ["enter"]) = link.sent[1]
        else {
            Issue.record("expected paste then enter, got \(link.sent)")
            return
        }
        reply.handle(.done(req: paste))
        #expect(reply.deliveredCount == 0, "only half of the batch is done")
        reply.handle(.done(req: enter))
        #expect(reply.deliveredCount == 1)
        #expect(delivered == [waiting])
    }

    @Test func failuresAreShown() async {
        let reply = QuickReplyModel(sessionId: waiting, link: link)
        await reply.press(QuickKey.standard[0])
        guard case .sendKeys(let req, _, _)? = link.sent.first else { return }
        reply.handle(.error(req: req, id: waiting, message: "no session"))
        #expect(reply.failedCount == 1)
        #expect(reply.errorMessage?.contains("no session") == true)
        #expect(reply.deliveredCount == 0)
        // 下一次发送清掉旧的错误。
        await reply.press(QuickKey.standard[0])
        #expect(reply.errorMessage == nil)
        reply.connectionLost()
        #expect(reply.failedCount == 2)
    }

    /// 列表上的快速回复：不用连上会话，送到后刷新这个会话的预览。
    @Test func listRepliesRefreshThePreview() async {
        let clock = ManualClock()
        let model = SessionListModel(machine: machineRecord(), link: link, now: { clock.now })
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(waiting, .blocked)])))
        link.clearSent()
        let reply = model.quickReply(for: waiting)
        #expect(model.quickReply(for: waiting) === reply)
        await reply.press(QuickKey.standard[0])
        #expect(!link.sent.contains { if case .attach = $0 { return true } else { return false } })
        guard case .sendKeys(let req, _, _)? = link.sent.first else { return }
        clock.advance(.seconds(2))
        model.handle(.message(.done(req: req)))
        #expect(reply.deliveredCount == 1)
        #expect(readScreens(link.sent) == [waiting])
    }
}

@MainActor
@Suite struct NavigationTests {
    /// 从列表打开终端页：列表知道有没有 Mac 在显示它，据此决定一开始就适配手机还是跟随 Mac。
    @Test func terminalsKnowWhetherTheMacShowsThem() async throws {
        let store = InMemoryMachineStore()
        let machine = machineRecord()
        await store.upsert(machine)
        let link = FakeLink()
        let app = AppModel(
            dependencies: AppDependencies(
                store: store, keyStore: InMemoryKeyStore(), pairing: FakePairing { _ in machine },
                makeLink: { _ in link }, deviceName: "测试 iPhone"))
        await app.machineList.load()
        app.path = [.machine(machine.id)]
        let list = try #require(app.sessionList(for: machine.id))
        list.handle(.ready(generation: 1))
        list.handle(.message(.sessionList([info(waiting, .blocked, owner: "Mac"), info(plain, nil)])))
        let shown = try #require(app.terminal(machine: machine.id, session: waiting))
        let background = try #require(app.terminal(machine: machine.id, session: plain))
        #expect(!shown.fitsPhone)
        #expect(background.fitsPhone)
        #expect(shown.isAwaitingAnswer)
    }
}

@Suite struct StatusTextTests {
    @Test func reconnectCountdown() {
        let now = Date(timeIntervalSince1970: 1000)
        #expect(Presentation.linkStatus(.waiting(reason: "断了", retryAt: now + 4.2), now: now) == "已断开，5 秒后重连")
        #expect(Presentation.linkStatus(.waiting(reason: "断了", retryAt: now - 1), now: now) == "已断开，正在重连")
        #expect(Presentation.linkStatus(.connecting) == "连接中…")
    }

    @Test func terminalSubtitleCombinesAgentAndLink() {
        let agent = Agent(kind: AgentKind("claude"), state: .blocked)
        #expect(
            Presentation.terminalSubtitle(agent: agent, link: .connected(hostName: "Mac", address: nil), phase: .live)
                == "Claude Code · 等你回答 · 已连接")
        #expect(Presentation.terminalSubtitle(agent: nil, link: .connecting, phase: .exited(0)) == "shell 已退出")
    }

    @Test func addressesAreShownWithoutTheInterfaceScope() {
        #expect(Presentation.displayAddress("10.0.0.10%en0") == "10.0.0.10")
        #expect(Presentation.displayAddress("fe80::1c2:3%en0") == "fe80::1c2:3")
        #expect(Presentation.displayAddress("fd7a:115c:a1e0::1") == "fd7a:115c:a1e0::1")
        #expect(Presentation.displayAddress("192.168.1.20") == "192.168.1.20")
    }

    /// shell 在提示符上等输入时，最后一个有字的行（光标所在的提示符）不进预览。
    @Test func previewsSkipThePromptTheShellWaitsAt() {
        let screen = "$ cargo build\n   Compiling runode v0.1.0\n    Finished dev\n\u{E0B0} ~/runode \u{E0A0} main ❯\n\n"
        #expect(
            Presentation.previewLines(screen, limit: 3, atPrompt: true)
                == ["$ cargo build", "   Compiling runode v0.1.0", "    Finished dev"])
        #expect(Presentation.previewLines(screen, limit: 3).last == "\u{E0B0} ~/runode \u{E0A0} main ❯")
        #expect(Presentation.previewLines("❯", limit: 3, atPrompt: true).isEmpty)
    }

    @Test func previewsDropBlankAndRuleLines() {
        #expect(Presentation.previewLines("a\n\n  \nb  \n────\n╭──╮\nc\n", limit: 3) == ["a", "b", "c"])
        #expect(Presentation.previewLines("1\n2\n3\n4", limit: 3) == ["2", "3", "4"])
        #expect(Presentation.previewLines("", limit: 3).isEmpty)
    }
}
