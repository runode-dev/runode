import CoreGraphics
import Foundation
import Observation
import RunodeConnection
import RunodeProtocol

/// 连上一台电脑的远程桌面。App 入口给真的（`ScreenConnection.open`），演示模式和测试不给。
public typealias ScreenOpener = @Sendable (MachineRecord) async throws -> ScreenConnection

/// 远程桌面页：连上电脑的远程桌面、按屏幕的样子要画面，画面交给显示层，手势和键盘变成电脑上的键鼠。
@Observable
@MainActor
public final class ScreenModel {
    public enum Phase: Equatable {
        case connecting
        case live
        case failed(String)
    }

    /// 画面最长的一边最多这么多像素：再大手机屏幕也显示不出来，只多占带宽。
    nonisolated static let maxSide: UInt32 = 2560
    static let fps: UInt32 = 60
    /// 平均码率，单位 bit/s。
    static let bitrate: UInt32 = 10_000_000
    /// 两次点按隔得这么近（时间和手机上的点数）时算双击，第二次点在第一次的位置上：手指点不准，
    /// 电脑认双击要求两次落在几个点以内。
    static let doubleTapInterval: Duration = .milliseconds(400)
    static let doubleTapSlop: CGFloat = 24

    public let machine: MachineRecord
    public private(set) var phase: Phase = .connecting
    /// 电脑报的各块屏幕，主屏排第一。
    public private(set) var displays: [ScreenDisplay] = []
    /// 正在看的那块屏幕，开始采集后才有。
    public private(set) var display: ScreenDisplay?
    /// 电脑缺的授权，给人看的说明；都有时为空。
    public private(set) var missingPermissions: [String] = []
    /// 画面的像素尺寸，开始采集后才有。
    public private(set) var videoSize: CGSize?
    /// 按住不放的修饰键，下一次按键或输入文字时带上，用完松开。
    public var stickyMods = ScreenMods()
    /// 电脑锁着屏。
    public private(set) var locked = false
    /// 发了密码，在等电脑解开。
    public private(set) var unlocking = false
    /// 要用户输登录密码：没存过，存的不对，或者用户没过 Face ID。
    public var askingPassword = false
    /// 上一次解锁为什么没成，给人看。
    public private(set) var unlockError: String?
    /// 收到的画面交给它（显示层）。
    @ObservationIgnored public var onVideo: ((VideoFrame) -> Void)?
    /// 重新开始采集了（切了屏幕）：显示层清掉旧画面，等新的关键帧。
    @ObservationIgnored public var onVideoReset: (() -> Void)?
    /// 触屏：手指点哪里电脑上就点哪里。触控板：单指挪的是一个虚拟光标，点按、拖动都落在光标上。
    public enum PointerMode: String, Sendable {
        case touch, trackpad
    }
    public var pointerMode: PointerMode = .touch
    /// 触控板模式的光标位置，画面上的比例。
    @ObservationIgnored public private(set) var cursor = (x: 0.5, y: 0.5)
    /// 光标挪了：显示层把光标画到新位置。
    @ObservationIgnored public var onCursor: ((Double, Double) -> Void)?

    @ObservationIgnored private let openConnection: ScreenOpener
    @ObservationIgnored private let passwords: (any UnlockPasswordStore)?
    /// 发出密码后最多等电脑这么久解开，过了当成密码不对。
    @ObservationIgnored var unlockWait: Duration = .seconds(8)
    /// 这次锁屏已经自动解过一次：没解开时不再自动试，免得一直拿错的密码去敲。
    @ObservationIgnored private var triedAutomatically = false
    /// 解开以后要存下的密码（用户勾了记住）。
    @ObservationIgnored private var passwordToSave: String?
    /// 这次发的是存着的密码：没解开时删掉它。
    @ObservationIgnored private var sentStoredPassword = false
    /// 在等 Face ID 或等电脑解开；解开、没解开、断开时取消，迟到的结果不再算数。
    @ObservationIgnored private var unlockTask: Task<Void, Never>?
    @ObservationIgnored private var connection: ScreenConnection?
    @ObservationIgnored private var task: Task<Void, Never>?
    /// 第几次 `start`，旧的那次结束时据此认出自己已经过时。
    @ObservationIgnored private var attempt = 0
    /// 用户选的屏幕，断线重连后接着看它。
    @ObservationIgnored private var chosenDisplay: UInt32?
    @ObservationIgnored private var lastTap: (view: CGPoint, x: Double, y: Double, at: ContinuousClock.Instant)?

    public init(machine: MachineRecord, open: @escaping ScreenOpener, passwords: (any UnlockPasswordStore)? = nil) {
        self.machine = machine
        openConnection = open
        self.passwords = passwords
    }

    /// 连上并开始采集；已经在连或者连着时什么都不做。
    public func start() {
        guard task == nil else { return }
        phase = .connecting
        attempt += 1
        let attempt = attempt
        task = Task { [weak self, openConnection, machine] in
            var failure: String?
            do {
                let connection = try await openConnection(machine)
                guard let self, !Task.isCancelled, self.attempt == attempt else { return connection.close() }
                try await self.run(connection)
            } catch is CancellationError {
            } catch {
                failure = error.localizedDescription
            }
            // `retry` 已经开了新的一次：这次的结果不算数，别把新的状态冲掉。
            guard let self, self.attempt == attempt else { return }
            if let failure { phase = .failed(failure) }
            connection = nil
            task = nil
            forgetLock()
        }
    }

    private func run(_ connection: ScreenConnection) async throws {
        self.connection = connection
        displays = connection.displays
        missingPermissions = Self.missing(screenCapture: connection.screenCapture, postEvent: connection.postEvent)
        request(displays.first { $0.id == chosenDisplay } ?? displays[0])
        for try await event in connection.events() {
            handle(event)
        }
    }

    func handle(_ event: ScreenEvent) {
        switch event {
        case .video(let frame):
            onVideo?(frame)
        case .message(.started(let id, let width, let height)):
            display = displays.first { $0.id == id } ?? display
            videoSize = CGSize(width: Int(width), height: Int(height))
            onVideoReset?()
            phase = .live
        case .message(.stopped(let reason)):
            phase = .failed(reason)
        case .message(.lock(let isLocked)):
            locked = isLocked
            if !isLocked {
                unlocked()
            } else if !triedAutomatically {
                triedAutomatically = true
                unlock()
            }
        case .message(.unlockFailed):
            unlockFailed(String(localized: "密码里有美式键盘打不出的字符，只能在电脑前解锁"))
        case .message:
            break
        }
    }

    // MARK: 解锁

    /// 用存着的密码解锁（要过 Face ID）；没存过、没过验证时请用户输。
    public func unlock() {
        guard locked, !unlocking else { return }
        guard let passwords, passwords.hasPassword(for: machine.id) else {
            askingPassword = true
            return
        }
        // 等 Face ID 时也算在解锁：再点一次按钮、电脑又报一次锁屏时不再弹一次。
        unlocking = true
        let machine = machine.id
        unlockTask = Task { [weak self] in
            let password = try? await passwords.password(for: machine, reason: String(localized: "解开电脑的锁屏"))
            guard let self, !Task.isCancelled else { return }
            if let password {
                sendUnlock(password, save: nil, stored: true)
            } else {
                unlocking = false
                askingPassword = true
            }
        }
    }

    /// 用用户输的密码解锁；`remember` 时解开以后存下（之后要过 Face ID 才取得出）。
    public func unlock(with password: String, remember: Bool) {
        askingPassword = false
        sendUnlock(password, save: remember ? password : nil, stored: false)
    }

    private func sendUnlock(_ password: String, save: String?, stored: Bool) {
        unlocking = true
        unlockError = nil
        passwordToSave = save
        sentStoredPassword = stored
        connection?.send(.unlock(password: password))
        unlockTask?.cancel()
        unlockTask = Task { [weak self, unlockWait] in
            try? await Task.sleep(for: unlockWait)
            guard !Task.isCancelled else { return }
            self?.unlockFailed(String(localized: "电脑还锁着，密码可能不对"))
        }
    }

    private func unlocked() {
        unlockTask?.cancel()
        if unlocking, let passwordToSave {
            try? passwords?.save(passwordToSave, for: machine.id)
        }
        unlocking = false
        passwordToSave = nil
        unlockError = nil
        askingPassword = false
        triedAutomatically = false
    }

    private func unlockFailed(_ message: String) {
        guard unlocking else { return }
        unlockTask?.cancel()
        if sentStoredPassword {
            passwords?.deletePassword(for: machine.id)
        }
        unlocking = false
        passwordToSave = nil
        unlockError = message
        askingPassword = true
    }

    /// 换到另一块屏幕看。
    public func switchDisplay(to display: ScreenDisplay) {
        guard display.id != self.display?.id else { return }
        chosenDisplay = display.id
        request(display)
    }

    /// 屏幕在列表里的名字：主屏、副屏 1、副屏 2…，带上分辨率。
    public func name(of display: ScreenDisplay) -> String {
        let size = "\(display.width)×\(display.height)"
        guard let index = displays.firstIndex(of: display), index > 0 else {
            return String(localized: "主屏 · \(size)")
        }
        return String(localized: "副屏 \(index) · \(size)")
    }

    private func request(_ display: ScreenDisplay) {
        let (width, height) = Self.captureSize(display)
        connection?.send(.start(display: display.id, width: width, height: height, fps: Self.fps, bitrate: Self.bitrate))
    }

    /// 断开；离开页面、退到后台时调。
    public func close() {
        attempt += 1
        task?.cancel()
        task = nil
        connection?.close()
        connection = nil
        forgetLock()
    }

    /// 断开不算密码不对：作废在等的解锁，新的连接会重新报锁屏。
    private func forgetLock() {
        unlockTask?.cancel()
        unlocking = false
        passwordToSave = nil
        locked = false
        triedAutomatically = false
    }

    /// 断了以后重新连。
    public func retry() {
        close()
        start()
    }

    /// 显示层解不出画面（还没收到关键帧、解码出错）时要一个关键帧。
    public func requestKeyframe() {
        connection?.send(.keyframe)
    }

    // MARK: 输入。坐标是画面上的比例，(0, 0) 是左上角。

    /// 点一下：挪过去再按下松开左键。`view` 是手机上点的位置，用来认双击。
    public func tap(x: Double, y: Double, view: CGPoint) {
        let (x, y) = tapTarget(x: x, y: y, view: view)
        click(.left, x: x, y: y)
    }

    /// 这次点按落在电脑上的哪里：紧跟着上一次、离得近时落在上一次的位置上。
    func tapTarget(x: Double, y: Double, view: CGPoint, now: ContinuousClock.Instant = .now) -> (Double, Double) {
        var (x, y) = (x, y)
        if let last = lastTap, now - last.at <= Self.doubleTapInterval,
            hypot(view.x - last.view.x, view.y - last.view.y) <= Self.doubleTapSlop
        {
            (x, y) = (last.x, last.y)
        }
        lastTap = (view, x, y, now)
        return (x, y)
    }

    public func rightClick(x: Double, y: Double) {
        click(.right, x: x, y: y)
    }

    private func click(_ button: ScreenMouseButton, x: Double, y: Double) {
        let mods = takeMods()
        send(.mouseMove(x: x, y: y))
        send(.mouseButton(button, down: true, x: x, y: y, mods: mods))
        send(.mouseButton(button, down: false, x: x, y: y, mods: mods))
    }

    /// 拖动：按下、挪动、松开左键。
    public func drag(_ phase: DragPhase, x: Double, y: Double) {
        switch phase {
        case .began:
            send(.mouseMove(x: x, y: y))
            send(.mouseButton(.left, down: true, x: x, y: y))
        case .moved:
            send(.mouseMove(x: x, y: y))
        case .ended:
            send(.mouseButton(.left, down: false, x: x, y: y))
        }
    }

    public enum DragPhase {
        case began, moved, ended
    }

    /// 触控板模式：光标挪 (`dx`, `dy`)，是画面上的比例；到边为止。
    public func moveCursor(dx: Double, dy: Double) {
        cursor = (Self.clamped(cursor.x + dx), Self.clamped(cursor.y + dy))
        onCursor?(cursor.x, cursor.y)
        send(.mouseMove(x: cursor.x, y: cursor.y))
    }

    /// 触控板模式：在光标处点一下。
    public func clickAtCursor(_ button: ScreenMouseButton) {
        click(button, x: cursor.x, y: cursor.y)
    }

    /// 触控板模式：在光标处按下、松开左键；中间用 `moveCursor` 拖。
    public func dragAtCursor(down: Bool) {
        send(.mouseButton(.left, down: down, x: cursor.x, y: cursor.y))
    }

    nonisolated static func clamped(_ fraction: Double) -> Double { min(max(fraction, 0), 1) }

    /// 两指滑动，`dx`、`dy` 是手机上的点数，和手指拖动的方向一致；按画面在手机上的宽度（`viewWidth`）
    /// 换成电脑上的点数。
    public func scroll(dx: CGFloat, dy: CGFloat, viewWidth: CGFloat) {
        guard let display, viewWidth > 0 else { return }
        let scale = CGFloat(display.width) / viewWidth
        let (x, y) = (Int32((dx * scale).rounded()), Int32((dy * scale).rounded()))
        guard x != 0 || y != 0 else { return }
        send(.scroll(dx: x, dy: y))
    }

    /// 键盘上输入的文字；带着按住的修饰键时变成按键（`⌘C` 这类快捷键）。
    public func type(_ text: String) {
        let mods = takeMods()
        send(Self.input(for: text, mods: mods))
    }

    /// 按一个键，`key` 是电脑那边 `Key` 的名字。
    public func press(_ key: String) {
        send(.key(key, mods: takeMods()))
    }

    private func takeMods() -> ScreenMods {
        defer { stickyMods = ScreenMods() }
        return stickyMods
    }

    private func send(_ input: ScreenInput) {
        connection?.send(.input(input))
    }

    // MARK: 不碰连接的计算

    /// 要的画面尺寸：电脑屏幕的像素数，最长的一边超过 `maxSide` 时等比缩小，取偶数。
    nonisolated static func captureSize(_ display: ScreenDisplay) -> (UInt32, UInt32) {
        let (width, height) = (max(display.pixelWidth, 2), max(display.pixelHeight, 2))
        let scale = min(1, Double(maxSide) / Double(max(width, height)))
        let even = { (value: UInt32) in UInt32((Double(value) * scale).rounded()) & ~1 }
        return (even(width), even(height))
    }

    /// 文字和修饰键变成输入：带修饰键的单个字母、数字按键处理，别的照文字输入。
    nonisolated static func input(for text: String, mods: ScreenMods) -> ScreenInput {
        if text == "\n" { return .key("Enter", mods: mods) }
        guard !mods.isEmpty, text.count == 1, let char = text.first, char.isASCII else { return .text(text) }
        if char.isLetter { return .key(char.uppercased(), mods: mods) }
        if char.isNumber { return .key("Digit\(char)", mods: mods) }
        return .text(text)
    }

    nonisolated static func missing(screenCapture: Bool, postEvent: Bool) -> [String] {
        var missing: [String] = []
        if !screenCapture { missing.append(String(localized: "屏幕录制")) }
        if !postEvent { missing.append(String(localized: "辅助功能")) }
        return missing
    }
}
