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

    @Test func fittingTheScreenAsksForTheSize() {
        let model = live()
        let fit = GridSize(cols: 45, rows: 30, cellWidthPx: 24, cellHeightPx: 48)
        model.updateFitSize(fit)
        link.clearSent()
        model.fitToScreen()
        #expect(link.sent == [.resize(id: sessionA, size: fit), .focus(id: sessionA, focused: true)])
        #expect(display.fits == 1)
        model.handle(.message(.sizeOwner(id: sessionA, mine: true, owner: "测试 iPhone")))
        #expect(model.sizeOwnership == .mine)
        // 适配着的时候视图大小变了（转屏、弹键盘）跟着改。
        link.clearSent()
        let rotated = GridSize(cols: 90, rows: 15, cellWidthPx: 24, cellHeightPx: 48)
        model.updateFitSize(rotated)
        #expect(link.sent == [.resize(id: sessionA, size: rotated)])
    }

    @Test func followingTheMacGivesTheSizeBack() {
        let model = live()
        model.updateFitSize(GridSize(cols: 45, rows: 30, cellWidthPx: 24, cellHeightPx: 48))
        model.fitToScreen()
        link.clearSent()
        model.followHostSize()
        #expect(link.sent == [.detach(id: sessionA), .attach(id: sessionA, size: nil, mode: .vtReplay)])
        #expect(!model.fitsScreen)
        model.handle(.message(.sizeOwner(id: sessionA, mine: false, owner: "Ethan 的 MacBook")))
        #expect(model.sizeOwnership == .other("Ethan 的 MacBook"))
    }

    /// 「跟随 Mac」时手机不带尺寸：打字、视图大小变了、尺寸归属变了都不发 `Resize`、`Focus`，也不带尺寸
    /// `Attach`。宿主据此不让这条连接的打字抢走尺寸归属。
    @Test func followingTheMacNeverAsksForASize() {
        let model = live()
        link.clearSent()
        model.updateFitSize(GridSize(cols: 45, rows: 30, cellWidthPx: 24, cellHeightPx: 48))
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

    @Test func reattachingAfterReconnectKeepsTheFit() {
        let model = live()
        let fit = GridSize(cols: 45, rows: 30, cellWidthPx: 24, cellHeightPx: 48)
        model.updateFitSize(fit)
        model.fitToScreen()
        model.handle(.state(.waiting(reason: "断了", retryAt: .now)))
        #expect(model.phase == .disconnected("断了"))
        link.clearSent()
        model.handle(.ready(generation: 2))
        #expect(link.sent == [.attach(id: sessionA, size: fit, mode: .vtReplay)])
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
