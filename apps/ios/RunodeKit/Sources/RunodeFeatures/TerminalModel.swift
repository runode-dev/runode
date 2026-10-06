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

/// 用户在终端页上选的尺寸方式。
public enum SizePreference: String, Hashable, Sendable, CaseIterable, Codable {
    /// 按情况自动选：没有别的前端在决定尺寸时适配手机，有电脑在显示时跟随电脑。
    case automatic
    /// 按手机屏幕决定尺寸。
    case fitPhone
    /// 跟随电脑的尺寸，手机这边缩放、平移着看。
    case followMachine
}

/// 打开终端页时从会话列表知道的、这个会话有没有别的前端在决定尺寸。
public enum SizeOwnerHint: Hashable, Sendable {
    /// 没有：电脑上没有窗口在显示它（手机新开的、后台会话）。
    case none
    /// 有，比如电脑上的窗口。
    case someone
    /// 不知道（不是从列表打开的）：连上后等宿主的 `SizeOwner`，等不到就当作没有。
    case unknown
}

/// 终端页的视图模型：连上一个会话、持有手机这边的那份 VT、把用户的输入编码后发出去。
///
/// 尺寸：自动模式下，会话没有尺寸 owner 时（`SizeOwnerHint.none`，或连上后宿主没报 `SizeOwner`）
/// 按手机屏幕适配，带尺寸 `Attach` 或发 `Resize` 加 `Focus` 接管；有电脑在决定尺寸时不带尺寸
/// `Attach`，跟随电脑，视图按可读的最小字号缩放、横向平移。owner 变了（`SizeOwner`）自动模式跟着
/// 重新判断；用户手动选过以后以用户为准。从适配换成跟随时 `Detach` 后不带尺寸重新 `Attach`，丢掉
/// 宿主记着的这条连接请求过的尺寸：宿主对没请求过尺寸的连接，打字不算交互，不会把尺寸抢回来。
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
    public private(set) var agent: Agent?
    public private(set) var phase: Phase = .connecting
    public private(set) var linkState: LinkState = .idle
    /// 宿主那边会话现在的网格尺寸。
    public private(set) var gridSize: GridSize?
    public private(set) var sizeOwnership: SizeOwnership = .unknown
    public private(set) var sizePreference: SizePreference = .automatic
    /// 视口在看回滚历史，没跟着最新的输出。
    public private(set) var scrolledBack = false
    /// 软键盘（或硬件键盘的输入焦点）在终端上。
    public private(set) var keyboardVisible = false
    /// 终端的背景色，导航栏、空白区跟着它。
    public private(set) var background = TermSettings.default.background
    public var errorMessage: String?
    /// 在等用户确认结束会话。
    public var isConfirmingKill = false
    /// 底部的快速回复：agent 停下来等回答时出现。
    public let quickReply: QuickReplyModel

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
    /// 这次连接上已经替手机要过的尺寸；不要重复发。
    @ObservationIgnored private var requestedFit: GridSize?
    /// 自动模式下现在要不要适配手机。
    @ObservationIgnored private var autoFit: Bool
    @ObservationIgnored private let ownerHint: SizeOwnerHint
    @ObservationIgnored private let ownerProbeDelay: Duration
    @ObservationIgnored private let onOpen: @MainActor (SessionId) -> Void
    @ObservationIgnored private let onClose: @MainActor (SessionId) -> Void

    /// `onOpen`、`onClose` 告诉会话列表这个会话正被终端页看着（列表就不再给它发只看状态的 `Attach`，
    /// 免得换掉这里的订阅）；关掉时由列表改回只看状态。`ownerProbeDelay` 是 `ownerHint` 为 `unknown`
    /// 时，重放完以后等宿主报 `SizeOwner` 的时间，等不到就当作没有 owner。
    public init(
        sessionId: SessionId, title: String, agent: Agent? = nil, link: any HostLink,
        ownerHint: SizeOwnerHint = .unknown, sizePreference: SizePreference = .automatic,
        ownerProbeDelay: Duration = .milliseconds(400),
        onOpen: @escaping @MainActor (SessionId) -> Void = { _ in },
        onClose: @escaping @MainActor (SessionId) -> Void
    ) {
        self.sessionId = sessionId
        self.title = title
        self.agent = agent
        self.link = link
        self.ownerHint = ownerHint
        self.sizePreference = sizePreference
        self.ownerProbeDelay = ownerProbeDelay
        self.autoFit = ownerHint == .none
        self.onOpen = onOpen
        self.onClose = onClose
        self.quickReply = QuickReplyModel(sessionId: sessionId, link: link)
    }

    /// 现在按手机屏幕决定尺寸。
    public var fitsPhone: Bool {
        switch sizePreference {
        case .automatic: autoFit
        case .fitPhone: true
        case .followMachine: false
        }
    }

    /// agent 停下来等用户回答。
    public var isAwaitingAnswer: Bool {
        agent?.state == .blocked && !isExited
    }

    private var isExited: Bool {
        if case .exited = phase { return true }
        return false
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
        display.terminalSizeModeDidChange(fitsPhone: fitsPhone)
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
            case .wheel(let lines, let column, let row): terminal.encodeWheel(lines: lines, column: column, row: row)
            case .click(let column, let row): terminal.encodeClick(column: column, row: row)
            }
        guard !bytes.isEmpty else { return }
        link.sendInput(Data(bytes), channel: channel, generation: generation)
    }

    /// 视图按自己的大小算出来的「适配手机」的网格。正在适配时跟着改（转屏、弹键盘）。
    public func updateFitSize(_ size: GridSize) {
        fitSize = size
        if fitsPhone { requestFit() }
    }

    /// 用户在导航栏上选尺寸方式。
    public func setSizePreference(_ preference: SizePreference) {
        let wasFitting = fitsPhone
        sizePreference = preference
        if preference == .automatic {
            switch sizeOwnership {
            case .mine: autoFit = true
            case .other: autoFit = false
            case .unknown: break
            }
        }
        applySizeMode(wasFitting: wasFitting)
    }

    public func setScrolledBack(_ scrolledBack: Bool) {
        self.scrolledBack = scrolledBack
    }

    public func setKeyboardVisible(_ visible: Bool) {
        keyboardVisible = visible
    }

    /// 唤起键盘。
    public func showKeyboard() {
        display?.terminalShowKeyboard()
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

    /// 手动重连：等着重试的马上试，停着的重新开始。
    public func reconnect() {
        let link = self.link
        Task { await link.reconnectNow() }
    }

    // MARK: 尺寸

    /// 适配手机和跟随电脑之间换了：适配时要尺寸，跟随时放手。
    private func applySizeMode(wasFitting: Bool) {
        let fitting = fitsPhone
        display?.terminalSizeModeDidChange(fitsPhone: fitting)
        guard fitting != wasFitting else { return }
        if fitting {
            requestFit()
        } else {
            releaseSize()
        }
    }

    /// 要按手机屏幕的尺寸：先告诉宿主想要的尺寸，再算一次交互，轮到这部手机决定。
    private func requestFit() {
        guard let fitSize, channel != nil, !awaitingAttach, fitSize != requestedFit else { return }
        requestedFit = fitSize
        link.send(.resize(id: sessionId, size: fitSize))
        link.send(.focus(id: sessionId, focused: true))
    }

    /// 不再决定尺寸：断开后不带尺寸重新连上，宿主忘掉这条连接请求过的尺寸，尺寸交还给电脑上最近用过
    /// 它的前端。
    private func releaseSize() {
        guard generation != nil, requestedFit != nil || channel != nil else { return }
        link.send(.detach(id: sessionId))
        attach()
    }

    // MARK: 事件

    private func attach() {
        awaitingAttach = true
        channel = nil
        let size = fitsPhone ? fitSize : nil
        requestedFit = size
        if case .exited = phase {} else { phase = .connecting }
        link.send(.attach(id: sessionId, size: size, mode: .vtReplay))
    }

    func handle(_ event: HostEvent) {
        switch event {
        case .state(let state):
            linkState = state
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
            if quickReply.handle(message) { return }
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
        requestedFit = nil
        quickReply.connectionLost()
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
            background = settings.background
            if let title = attached.meta.displayTitle { self.title = title }
            agent = attached.meta.agent
            if case .exited = phase {} else { phase = .replaying }
            display?.terminalDidReset(terminal, settings: settings)
            // 适配手机，但 `Attach` 时还不知道视图多大：现在补上。
            if fitsPhone, requestedFit == nil { requestFit() }
        case .snapshotEnd:
            guard !awaitingAttach, channel != nil else { return }
            if phase == .replaying { phase = .live }
            probeOwnerIfNeeded()
        case .resized(_, let size):
            guard !awaitingAttach, let terminal else { return }
            terminal.resize(size)
            gridSize = size
            display?.terminalContentDidChange()
        case .themeApplied(_, let newSettings):
            guard !awaitingAttach, let terminal else { return }
            settings = newSettings
            background = newSettings.background
            terminal.applyTheme(newSettings)
            display?.terminalSettingsDidChange(newSettings)
        case .meta(_, let meta):
            if let title = meta.displayTitle { self.title = title }
            agent = meta.agent
        case .resync:
            guard generation != nil else { return }
            attach()
        case .exited(_, let status):
            phase = .exited(status)
        case .bell:
            display?.terminalDidRingBell()
        case .sizeOwner(_, let mine, let owner):
            sizeOwnership = mine ? .mine : .other(owner)
            if sizePreference == .automatic {
                let wasFitting = fitsPhone
                autoFit = mine
                applySizeMode(wasFitting: wasFitting)
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

    /// 不知道有没有 owner 时：重放完以后宿主若有 owner 会马上报 `SizeOwner`；等一会儿没等到就当作
    /// 没有，自动模式改成适配手机。
    private func probeOwnerIfNeeded() {
        guard ownerHint == .unknown, sizePreference == .automatic, sizeOwnership == .unknown else { return }
        let delay = ownerProbeDelay
        Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard let self, self.sizePreference == .automatic, self.sizeOwnership == .unknown,
                !self.awaitingAttach, self.channel != nil
            else { return }
            let wasFitting = self.fitsPhone
            self.autoFit = true
            self.applySizeMode(wasFitting: wasFitting)
        }
    }
}
