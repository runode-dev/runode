import Foundation
import Observation
import RunodeConnection
import RunodeProtocol

/// 配对页：扫电脑上的二维码，或者粘贴 `runode://pair?...` 链接（模拟器上没有摄像头时用）。
@Observable
@MainActor
public final class PairingModel {
    public enum Phase: Hashable, Sendable {
        case idle
        /// 正在和这台电脑配对。
        case pairing(hostName: String)
        case paired(MachineRecord)
        case failed(String)
    }

    public var linkText = ""
    /// 扫不了码的原因（没有摄像头、没给权限）；为空时显示取景框。
    public var scannerUnavailable: String?
    public private(set) var phase: Phase = .idle

    @ObservationIgnored private let pairing: any Pairing
    @ObservationIgnored private let deviceName: String
    @ObservationIgnored private let onPaired: @MainActor (MachineRecord) async throws -> Void
    @ObservationIgnored private let now: @Sendable () -> Date
    /// 正在跑的配对；配对页关掉时撤掉。
    @ObservationIgnored private var running: Task<MachineRecord, any Error>?
    /// 配对页关掉了：之后配好的不再存。
    @ObservationIgnored private var cancelled = false

    /// `onPaired` 把配好的电脑存下来，存不下时抛出，配对页显示失败。
    public init(
        pairing: any Pairing, deviceName: String, now: @escaping @Sendable () -> Date = { .now },
        onPaired: @escaping @MainActor (MachineRecord) async throws -> Void
    ) {
        self.pairing = pairing
        self.deviceName = deviceName
        self.now = now
        self.onPaired = onPaired
    }

    /// 输入框里的链接解析出来的邀请，配对前给用户看要连的电脑；不是配对链接时为空。
    public var invitation: PairingInvitation? {
        try? PairingInvitation.parse(linkText)
    }

    public var isBusy: Bool {
        if case .pairing = phase { return true }
        return false
    }

    /// 用输入框里的链接配对。
    public func submitLink() async {
        await pair(text: linkText)
    }

    /// 扫到一个二维码。正在配对或者已经配好时忽略。
    public func scanned(_ code: String) async {
        switch phase {
        case .pairing, .paired: return
        default: break
        }
        linkText = code
        await pair(text: code)
    }

    public func reset() {
        phase = .idle
    }

    /// 配对页关掉了：撤掉正在跑的配对，配好了也不存（不换掉同一台电脑的旧配对）。已经在存的照样存完。
    public func cancel() {
        cancelled = true
        running?.cancel()
    }

    private func pair(text: String) async {
        guard !isBusy, !cancelled else { return }
        let invitation: PairingInvitation
        do {
            invitation = try PairingInvitation.parse(text)
        } catch {
            phase = .failed(error.errorDescription ?? String(localized: "这不是 Runode 的配对链接"))
            return
        }
        guard !invitation.isExpired(at: now()) else {
            phase = .failed(LinkFailure.invitationExpired.errorDescription ?? String(localized: "二维码已经过期"))
            return
        }
        phase = .pairing(hostName: invitation.hostName)
        let pairing = self.pairing
        let deviceName = self.deviceName
        let running = Task { try await pairing.pair(with: invitation, deviceName: deviceName) }
        self.running = running
        defer { self.running = nil }
        do {
            let machine = try await running.value
            guard !cancelled else { return }
            do {
                try await onPaired(machine)
            } catch {
                phase = .failed(String(localized: "存不下这台电脑：\(error.localizedDescription)"))
                return
            }
            phase = .paired(machine)
        } catch where cancelled {
            return
        } catch let failure as LinkFailure {
            phase = .failed(failure.errorDescription ?? String(localized: "配对失败"))
        } catch {
            phase = .failed(String(localized: "配对失败：\(error.localizedDescription)"))
        }
    }
}
