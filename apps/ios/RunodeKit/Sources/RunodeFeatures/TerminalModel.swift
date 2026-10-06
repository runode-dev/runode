import Foundation
import Observation
import RunodeConnection
import RunodeProtocol
import RunodeTerminal

/// 谁的视图尺寸决定这个会话的尺寸，见宿主的 `HostMsg::SizeOwner`。
public enum SizeOwnership: Hashable, Sendable {
    case unknown
    /// 这部手机。
    case mine
    /// 别的前端；带着它在 `Hello` 里报的设备名，没报时为空。
    case other(String?)
}

/// 终端页的视图模型：连上一个会话、持有手机这边的那份 VT、把用户的输入编码后发出去。
///
/// 尺寸：默认不带尺寸 `Attach`，不抢 Mac 上终端的尺寸，按宿主的网格画（视图里缩放、平移）；
/// 「适配本机屏幕」时发带尺寸的 `Resize` 再发 `Focus`，轮到这部手机决定尺寸；「跟随 Mac」时
/// `Detach` 后不带尺寸重新 `Attach`，尺寸交还给最近交互过的那个前端。
///
/// 帧的先后：宿主保证一个会话的控制消息和输出在连接上按发生的先后到达，`Resized`、`ThemeApplied`
/// 就是 VT 要改的位置；这里严格按事件的先后处理。发出 `Attach` 到收到 `Attached` 之间，旧订阅
/// 剩下的输出和标记都丢掉，`Attached` 时按宿主给的尺寸和主题新建一份 VT 再喂重放。
@Observable
@MainActor
public final class TerminalModel {
    public enum Phase: Hashable, Sendable {
        /// 在等宿主回 `Attached`。
        case connecting
        /// 在喂 VT 重放，还没到 `SnapshotEnd`。
        case replaying
        case live
        /// shell 退出了，带着退出码（拿不到时为空）。
        case exited(Int32?)
        /// 连接断了，正在重连；带着原因。
        case disconnected(String)
        /// 宿主说没有这个会话了。
        case gone(String)
    }

    public let sessionId: SessionId
    public private(set) var title: String
    public private(set) var phase: Phase = .connecting
    /// 宿主那边会话现在的网格尺寸。
    public private(set) var gridSize: GridSize?
    public private(set) var sizeOwnership: SizeOwnership = .unknown
    /// 用户要求按本机屏幕决定尺寸。
    public private(set) var fitsScreen = false
    /// 视口在看回滚历史，没跟着最新的输出。
    public private(set) var scrolledBack = false
    public var errorMessage: String?
    /// 在等用户确认结束会话。
    public var isConfirmingKill = false

    /// 手机这边的 VT。归主 actor 上的这个模型管，视图只借来读和滚视口。
    @ObservationIgnored public private(set) var terminal: VTerminal?
    @ObservationIgnored private var settings = TermSettings.default
    @ObservationIgnored private weak var display: (any TerminalDisplay)?
    @ObservationIgnored private let link: any HostLink
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var generation: UInt64?
    @ObservationIgnored private var channel: UInt32?
    @ObservationIgnored private var awaitingAttach = false
    @ObservationIgnored private var fitSize: GridSize?
    @ObservationIgnored private let onOpen: @MainActor (SessionId) -> Void
    @ObservationIgnored private let onClose: @MainActor (SessionId) -> Void

    /// `onOpen`、`onClose` 告诉会话列表这个会话正被终端页看着（列表就不再给它发只看状态的 `Attach`，
    /// 免得换掉这里的订阅）；关掉时由列表改回只看状态。
    public init(
        sessionId: SessionId, title: String, link: any HostLink,
        onOpen: @escaping @MainActor (SessionId) -> Void = { _ in },
        onClose: @escaping @MainActor (SessionId) -> Void
    ) {
        self.sessionId = sessionId
        self.title = title
        self.link = link
        self.onOpen = onOpen
        self.onClose = onClose
    }

    /// 开始看这个会话：订阅连接的事件，连上了就 `Attach`。连接本身的开、停归会话列表管。
    public func open() {
        guard task == nil else { return }
        onOpen(sessionId)
        let link = self.link
        task = Task { [weak self] in
            let events = await link.events()
            for await event in events {
                guard let self else { return }
                self.handle(event)
            }
        }
    }

    /// 不看了：停止订阅，会话交回列表（改回只看状态）。
    public func close() {
        task?.cancel()
        task = nil
        onClose(sessionId)
        terminal = nil
        channel = nil
        display?.terminalDidReset(nil, settings: settings)
    }

    /// 接上画终端的一方；已经有 VT 时马上画出来。
    public func attachDisplay(_ display: any TerminalDisplay) {
        self.display = display
        display.terminalDidReset(terminal, settings: settings)
    }

    // MARK: 用户操作

    /// 把用户的输入按 VT 当前的模式编码后发给宿主。
    public func send(_ input: TerminalInput) {
        guard let terminal, let channel, let generation, !awaitingAttach else { return }
        switch phase {
        case .live, .replaying: break
        default: return
        }
        let bytes: [UInt8] =
            switch input {
            case .key(let key): terminal.encode(key)
            case .text(let text): Array(text.utf8)
            case .paste(let text): terminal.encodePaste(text)
            }
        guard !bytes.isEmpty else { return }
        link.sendInput(Data(bytes), channel: channel, generation: generation)
    }

    /// 视图按自己的大小算出来的「适配本机屏幕」的网格。已经在适配、尺寸又归这部手机时跟着改。
    public func updateFitSize(_ size: GridSize) {
        fitSize = size
        guard fitsScreen, sizeOwnership == .mine, size != gridSize, channel != nil else { return }
        link.send(.resize(id: sessionId, size: size))
    }

    /// 按本机屏幕决定尺寸：先告诉宿主想要的尺寸，再算一次交互，轮到这部手机决定。
    public func fitToScreen() {
        guard let fitSize else { return }
        fitsScreen = true
        display?.terminalWillFitScreen()
        guard channel != nil else { return }
        link.send(.resize(id: sessionId, size: fitSize))
        link.send(.focus(id: sessionId, focused: true))
    }

    /// 不再决定尺寸：断开后不带尺寸重新连上，尺寸交还给 Mac 上最近用过它的前端。
    public func followHostSize() {
        fitsScreen = false
        guard generation != nil else { return }
        link.send(.detach(id: sessionId))
        attach()
    }

    public func setScrolledBack(_ scrolledBack: Bool) {
        self.scrolledBack = scrolledBack
    }

    /// 回到最底下，跟着新输出走。
    public func scrollToBottom() {
        terminal?.scrollToBottom()
        display?.terminalContentDidChange()
    }

    /// 结束会话（要先让用户确认）。
    public func kill() {
        link.send(.kill(id: sessionId))
    }

    /// 连接停着（比如被拒绝后）时手动重连。
    public func reconnect() {
        let link = self.link
        Task { await link.reconnectNow() }
    }

    // MARK: 事件

    private func attach() {
        awaitingAttach = true
        channel = nil
        if case .exited = phase {} else { phase = .connecting }
        link.send(.attach(id: sessionId, size: fitsScreen ? fitSize : nil, mode: .vtReplay))
    }

    func handle(_ event: HostEvent) {
        switch event {
        case .state(let state):
            switch state {
            case .connected:
                break
            case .waiting(let reason, _):
                lostConnection(reason)
            case .failed(let failure):
                lostConnection(failure.errorDescription ?? "连接失败")
            case .connecting, .idle:
                if generation != nil { lostConnection("正在重新连接") }
            }
        case .ready(let generation):
            self.generation = generation
            attach()
        case .message(let message):
            guard message.sessionId == sessionId else { return }
            handle(message)
        case .frame(let frame, let frameGeneration):
            guard frameGeneration == generation, frame.channel == channel, !awaitingAttach, let terminal else {
                return
            }
            terminal.feed(frame.payload)
            display?.terminalContentDidChange()
        }
    }

    private func lostConnection(_ reason: String) {
        generation = nil
        channel = nil
        awaitingAttach = false
        switch phase {
        case .exited, .gone: break
        default: phase = .disconnected(reason)
        }
    }

    private func handle(_ message: HostMsg) {
        switch message {
        case .attached(let attached) where attached.mode != .metaOnly:
            settings = attached.settings ?? .default
            do {
                terminal = try VTerminal(size: attached.size, settings: settings)
            } catch {
                errorMessage = "建不了终端：\(error)"
                return
            }
            channel = attached.channel
            awaitingAttach = false
            gridSize = attached.size
            if let title = attached.meta.displayTitle { self.title = title }
            if case .exited = phase {} else { phase = .replaying }
            display?.terminalDidReset(terminal, settings: settings)
        case .snapshotEnd:
            guard !awaitingAttach, channel != nil else { return }
            if phase == .replaying { phase = .live }
        case .resized(_, let size):
            guard !awaitingAttach, let terminal else { return }
            terminal.resize(size)
            gridSize = size
            display?.terminalContentDidChange()
        case .themeApplied(_, let newSettings):
            guard !awaitingAttach, let terminal else { return }
            settings = newSettings
            terminal.applyTheme(newSettings)
            display?.terminalSettingsDidChange(newSettings)
        case .meta(_, let meta):
            if let title = meta.displayTitle { self.title = title }
        case .resync:
            guard generation != nil else { return }
            attach()
        case .exited(_, let status):
            phase = .exited(status)
        case .bell:
            display?.terminalDidRingBell()
        case .sizeOwner(_, let mine, let owner):
            sizeOwnership = mine ? .mine : .other(owner)
            if mine, fitsScreen, let fitSize, fitSize != gridSize {
                link.send(.resize(id: sessionId, size: fitSize))
            }
        case .error(_, _, let message):
            if awaitingAttach {
                awaitingAttach = false
                phase = .gone(message)
            } else {
                errorMessage = message
            }
        default:
            break
        }
    }
}
