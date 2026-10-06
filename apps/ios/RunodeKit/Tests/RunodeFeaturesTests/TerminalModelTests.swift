import Foundation
import RunodeConnection
import RunodeProtocol
import RunodeTerminal
import Testing

@testable import RunodeFeatures

@MainActor
@Suite struct TerminalModelTests {
    let link = FakeLink()
    let display = FakeDisplay()

    func model() -> TerminalModel {
        let model = TerminalModel(sessionId: sessionA, title: "zsh", link: link, onClose: { _ in })
        model.attachDisplay(display)
        return model
    }

    func attached(channel: UInt32, size: GridSize = smallGrid) -> HostEvent {
        .message(
            .attached(
                .init(
                    id: sessionA, channel: channel, size: size, mode: .vtReplay, meta: SessionMeta(title: "构建"),
                    settings: .default)))
    }

    /// 连上、收完重放，返回模型。
    func live() -> TerminalModel {
        let model = model()
        model.handle(.ready(generation: 1))
        model.handle(attached(channel: 5))
        model.handle(.frame(Frame(kind: .snapshot, channel: 5, payload: Data("replayed\r\n".utf8)), generation: 1))
        model.handle(.message(.snapshotEnd(id: sessionA)))
        return model
    }

    @Test func attachesWithoutTakingTheSize() {
        let model = model()
        model.handle(.ready(generation: 1))
        #expect(link.sent == [.attach(id: sessionA, size: nil, mode: .vtReplay)])
        #expect(model.phase == .connecting)
    }

    @Test func replaysThenFeedsOutputOnItsChannel() throws {
        let model = live()
        #expect(model.phase == .live)
        #expect(model.title == "构建")
        #expect(model.gridSize == smallGrid)
        #expect(display.terminal != nil)
        model.handle(.frame(Frame(kind: .output, channel: 5, payload: Data("live".utf8)), generation: 1))
        // 别的通道、旧连接的输出不喂。
        model.handle(.frame(Frame(kind: .output, channel: 6, payload: Data("other".utf8)), generation: 1))
        model.handle(.frame(Frame(kind: .output, channel: 5, payload: Data("stale".utf8)), generation: 0))
        let lines = try #require(model.terminal).screenLines()
        #expect(lines[0] == "replayed")
        #expect(lines[1] == "live")
    }

    @Test func resizeAndThemeHappenAtTheirMarkers() throws {
        let model = live()
        let bigger = GridSize(cols: 30, rows: 6, cellWidthPx: 8, cellHeightPx: 16)
        model.handle(.message(.resized(id: sessionA, size: bigger)))
        #expect(model.terminal?.size == bigger)
        #expect(model.gridSize == bigger)
        var theme = TermSettings.default
        theme.background = Rgb(1, 2, 3)
        model.handle(.message(.themeApplied(id: sessionA, settings: theme)))
        #expect(model.terminal?.settings == theme)
        #expect(display.settings == theme)
        // 别的会话的标记不碰这里的 VT。
        model.handle(.message(.resized(id: sessionB, size: smallGrid)))
        #expect(model.terminal?.size == bigger)
    }

    @Test func resyncDropsTheOldStreamUntilAttached() throws {
        let model = live()
        link.clearSent()
        model.handle(.message(.resync(id: sessionA, reason: "slow")))
        #expect(link.sent == [.attach(id: sessionA, size: nil, mode: .vtReplay)])
        #expect(model.phase == .connecting)
        // 新的 `Attached` 之前到的旧通道输出和标记都丢掉。
        model.handle(.frame(Frame(kind: .output, channel: 5, payload: Data("lost".utf8)), generation: 1))
        model.handle(.message(.resized(id: sessionA, size: GridSize(cols: 50, rows: 9, cellWidthPx: 8, cellHeightPx: 16))))
        model.handle(attached(channel: 9))
        #expect(model.terminal?.size == smallGrid)
        model.handle(.frame(Frame(kind: .snapshot, channel: 9, payload: Data("fresh".utf8)), generation: 1))
        model.handle(.message(.snapshotEnd(id: sessionA)))
        #expect(try #require(model.terminal).screenLines()[0] == "fresh")
        #expect(display.resets >= 2)
    }

    @Test func inputIsEncodedForTheTerminal() {
        let model = live()
        model.send(.text("ls"))
        model.send(.key(KeyInput.typing("c", modifiers: .control)!))
        model.send(.paste("a\nb"))
        let inputs = link.inputs
        #expect(inputs.map(\.data) == [Data("ls".utf8), Data([0x03]), Data("a\rb".utf8)])
        #expect(inputs.allSatisfy { $0.channel == 5 && $0.generation == 1 })
    }

    @Test func noInputBeforeAttached() {
        let model = model()
        model.handle(.ready(generation: 1))
        model.send(.text("ls"))
        #expect(link.inputs.isEmpty)
    }

    let fit = GridSize(cols: 45, rows: 30, cellWidthPx: 24, cellHeightPx: 48)

    /// 有电脑在显示的会话（列表里有尺寸 owner）：不带尺寸连上，跟随电脑。
    func followingModel() -> TerminalModel {
        let model = TerminalModel(sessionId: sessionA, title: "zsh", link: link, ownerHint: .someone, onClose: { _ in })
        model.attachDisplay(display)
        model.updateFitSize(fit)
        model.handle(.ready(generation: 1))
        model.handle(attached(channel: 5))
        model.handle(.message(.snapshotEnd(id: sessionA)))
        return model
    }

    /// 没有 owner 的会话（电脑上没有窗口在显示它）：自动适配手机，连上时就带着尺寸。
    @Test func sessionsWithoutAnOwnerFitThePhone() {
        let model = TerminalModel(sessionId: sessionA, title: "zsh", link: link, ownerHint: .none, onClose: { _ in })
        model.attachDisplay(display)
        model.updateFitSize(fit)
        model.handle(.ready(generation: 1))
        #expect(link.sent == [.attach(id: sessionA, size: fit, mode: .vtReplay)])
        #expect(model.fitsPhone)
        #expect(display.sizeModes.last == true)
    }

    /// 视图还没排好、不知道手机的尺寸时先不带尺寸连上，`Attached` 以后补发 `Resize` 加 `Focus`。
    @Test func fitIsSentOnceTheViewKnowsItsSize() {
        let model = TerminalModel(sessionId: sessionA, title: "zsh", link: link, ownerHint: .none, onClose: { _ in })
        model.handle(.ready(generation: 1))
        #expect(link.sent == [.attach(id: sessionA, size: nil, mode: .vtReplay)])
        model.handle(attached(channel: 5))
        link.clearSent()
        model.updateFitSize(fit)
        #expect(link.sent == [.resize(id: sessionA, size: fit), .focus(id: sessionA, focused: true)])
        // 同样的尺寸不重复发。
        link.clearSent()
        model.updateFitSize(fit)
        #expect(link.sent.isEmpty)
    }

    @Test func sessionsShownOnTheMachineFollowIt() {
        let model = followingModel()
        #expect(link.sent == [.attach(id: sessionA, size: nil, mode: .vtReplay)])
        model.handle(.message(.sizeOwner(id: sessionA, mine: false, owner: "Ethan 的 MacBook")))
        #expect(!model.fitsPhone)
        #expect(model.sizeOwnership == .other("Ethan 的 MacBook"))
    }

    /// 不知道有没有 owner 时：重放完以后宿主没报 `SizeOwner`，就当作没有，改成适配手机。
    @Test func noOwnerReportMeansFitThePhone() async {
        let model = TerminalModel(
            sessionId: sessionA, title: "zsh", link: link, ownerProbeDelay: .milliseconds(10), onClose: { _ in })
        model.attachDisplay(display)
        model.updateFitSize(fit)
        model.handle(.ready(generation: 1))
        #expect(link.sent == [.attach(id: sessionA, size: nil, mode: .vtReplay)])
        model.handle(attached(channel: 5))
        model.handle(.message(.snapshotEnd(id: sessionA)))
        #expect(await eventually { model.fitsPhone })
        #expect(link.sent.suffix(2) == [.resize(id: sessionA, size: fit), .focus(id: sessionA, focused: true)])
    }

    /// 宿主报了 owner 就不再自动适配。
    @Test func anOwnerReportStopsTheProbe() async throws {
        let model = TerminalModel(
            sessionId: sessionA, title: "zsh", link: link, ownerProbeDelay: .milliseconds(10), onClose: { _ in })
        model.updateFitSize(fit)
        model.handle(.ready(generation: 1))
        model.handle(attached(channel: 5))
        model.handle(.message(.snapshotEnd(id: sessionA)))
        model.handle(.message(.sizeOwner(id: sessionA, mine: false, owner: "homelab")))
        try await Task.sleep(for: .milliseconds(50))
        #expect(!model.fitsPhone)
        #expect(!link.sent.contains(.resize(id: sessionA, size: fit)))
    }

    /// 自动模式下 owner 变了跟着重新判断：电脑关了窗口、尺寸轮到手机时适配手机；电脑又接管时放手，
    /// 不带尺寸重新连上，免得手机这边打字把尺寸抢回来。
    @Test func automaticFollowsOwnerChanges() {
        let model = followingModel()
        model.handle(.message(.sizeOwner(id: sessionA, mine: false, owner: "homelab")))
        link.clearSent()
        model.handle(.message(.sizeOwner(id: sessionA, mine: true, owner: "测试 iPhone")))
        #expect(model.fitsPhone)
        #expect(link.sent == [.resize(id: sessionA, size: fit), .focus(id: sessionA, focused: true)])
        link.clearSent()
        model.handle(.message(.sizeOwner(id: sessionA, mine: false, owner: "homelab")))
        #expect(!model.fitsPhone)
        #expect(link.sent == [.detach(id: sessionA), .attach(id: sessionA, size: nil, mode: .vtReplay)])
    }

    /// 用户手动选了以后以用户为准：owner 再怎么变也不自动切换。
    @Test func manualChoiceWins() {
        let model = followingModel()
        link.clearSent()
        model.setSizePreference(.fitPhone)
        #expect(model.fitsPhone)
        #expect(link.sent == [.resize(id: sessionA, size: fit), .focus(id: sessionA, focused: true)])
        #expect(display.sizeModes.last == true)
        link.clearSent()
        model.handle(.message(.sizeOwner(id: sessionA, mine: false, owner: "homelab")))
        #expect(model.fitsPhone)
        #expect(link.sent.isEmpty)
        // 适配着的时候视图大小变了（转屏、弹键盘）跟着改。
        model.handle(.message(.sizeOwner(id: sessionA, mine: true, owner: "测试 iPhone")))
        link.clearSent()
        let rotated = GridSize(cols: 90, rows: 15, cellWidthPx: 24, cellHeightPx: 48)
        model.updateFitSize(rotated)
        #expect(link.sent.first == .resize(id: sessionA, size: rotated))
        // 换回跟随电脑：放手尺寸。
        link.clearSent()
        model.setSizePreference(.followMachine)
        #expect(!model.fitsPhone)
        #expect(link.sent == [.detach(id: sessionA), .attach(id: sessionA, size: nil, mode: .vtReplay)])
        model.handle(.message(.sizeOwner(id: sessionA, mine: true, owner: "测试 iPhone")))
        #expect(!model.fitsPhone)
    }

    @Test func reattachingAfterReconnectKeepsTheFit() {
        let model = followingModel()
        model.setSizePreference(.fitPhone)
        model.handle(.state(.waiting(reason: "断了", retryAt: .now)))
        #expect(model.phase == .disconnected("断了"))
        link.clearSent()
        model.handle(.ready(generation: 2))
        #expect(link.sent == [.attach(id: sessionA, size: fit, mode: .vtReplay)])
    }

    /// 跟随电脑时手机不带尺寸：打字、视图大小变了、`Resync` 后重新连上，都不发 `Resize`、`Focus`，也不带
    /// 尺寸 `Attach`。宿主据此不让这条连接的打字抢走尺寸归属。
    @Test func followingTheMachineNeverAsksForASize() {
        let model = followingModel()
        link.clearSent()
        model.updateFitSize(GridSize(cols: 50, rows: 20, cellWidthPx: 24, cellHeightPx: 48))
        model.send(.text("ls\r"))
        model.handle(.message(.sizeOwner(id: sessionA, mine: false, owner: "Ethan 的 MacBook")))
        model.handle(.message(.resync(id: sessionA, reason: "slow")))
        model.handle(.ready(generation: 2))
        for message in link.sent {
            switch message {
            case .resize, .focus: Issue.record("asked for a size: \(message)")
            case .attach(_, let size, _): #expect(size == nil)
            default: break
            }
        }
        #expect(!link.inputs.isEmpty)
    }

    /// agent 停下来等回答时出现快速回复；快速回复的回话由它自己收，不当作这个会话的错误。
    @Test func quickReplyAppearsWhileTheAgentWaits() async {
        let model = followingModel()
        #expect(!model.isAwaitingAnswer)
        model.handle(.message(.meta(id: sessionA, meta: SessionMeta(agent: Agent(kind: AgentKind("claude"), state: .blocked)))))
        #expect(model.isAwaitingAnswer)
        link.clearSent()
        await model.quickReply.press(QuickKey.standard[0])
        guard case .sendKeys(let req, sessionA, ["1"])? = link.sent.first else {
            Issue.record("expected send_keys, got \(link.sent)")
            return
        }
        model.handle(.message(.done(req: req)))
        #expect(model.quickReply.deliveredCount == 1)
        #expect(model.errorMessage == nil)
        model.handle(.message(.meta(id: sessionA, meta: SessionMeta(agent: Agent(kind: AgentKind("claude"), state: .working)))))
        #expect(!model.isAwaitingAnswer)
    }

    @Test func keyboardStateAndRequests() {
        let model = followingModel()
        model.showKeyboard()
        #expect(display.keyboardRequests == 1)
        model.setKeyboardVisible(true)
        #expect(model.keyboardVisible)
    }

    @Test func exitBellAndGone() {
        let model = live()
        model.handle(.message(.bell(id: sessionA)))
        #expect(display.bells == 1)
        model.handle(.message(.exited(id: sessionA, status: 3)))
        #expect(model.phase == .exited(3))

        let other = TerminalModel(sessionId: sessionB, title: "x", link: link, onClose: { _ in })
        other.handle(.ready(generation: 1))
        other.handle(.message(.error(req: nil, id: sessionB, message: "no session")))
        #expect(other.phase == .gone("no session"))
    }

    @Test func openingSubscribesAndClosingHandsBack() async {
        var opened: [SessionId] = []
        var closed: [SessionId] = []
        let model = TerminalModel(
            sessionId: sessionA, title: "zsh", link: link, onOpen: { opened.append($0) }, onClose: { closed.append($0) })
        model.open()
        #expect(opened == [sessionA])
        #expect(await eventually { link.subscriberCount == 1 })
        link.emit(.ready(generation: 1))
        #expect(await eventually { link.sent.contains(.attach(id: sessionA, size: nil, mode: .vtReplay)) })
        model.close()
        #expect(closed == [sessionA])
    }
}
