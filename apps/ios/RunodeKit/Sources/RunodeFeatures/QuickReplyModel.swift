import Foundation
import Observation
import RunodeConnection
import RunodeProtocol

/// 快速回复栏上的一个键：显示的字、发给宿主的键名（`send_keys` 的写法）、给 VoiceOver 读的名字。
/// 功能键显示 Mac 键盘上的符号（SF Symbols 的名字），这时不显示字。
public struct QuickKey: Hashable, Sendable, Identifiable {
    public let label: String
    public let key: String
    public let accessibilityLabel: String
    public var symbol: String?

    public var id: String { key }

    /// 回答 agent 提问最常用的几个键：选项编号、是否、回车、Esc、上下移动选项。
    public static let standard: [QuickKey] = [
        QuickKey(label: "1", key: "1", accessibilityLabel: "1"),
        QuickKey(label: "2", key: "2", accessibilityLabel: "2"),
        QuickKey(label: "3", key: "3", accessibilityLabel: "3"),
        QuickKey(label: "y", key: "y", accessibilityLabel: "y"),
        QuickKey(label: "n", key: "n", accessibilityLabel: "n"),
        QuickKey(label: "⏎", key: "enter", accessibilityLabel: "回车", symbol: "return"),
        QuickKey(label: "Esc", key: "esc", accessibilityLabel: "Esc", symbol: "escape"),
        QuickKey(label: "↑", key: "up", accessibilityLabel: "上", symbol: "arrowtriangle.up.fill"),
        QuickKey(label: "↓", key: "down", accessibilityLabel: "下", symbol: "arrowtriangle.down.fill"),
    ]
}

/// 一个会话的快速回复：按键经 `send_keys`、文字经 `paste` 加 `send_keys ["enter"]` 发给宿主，不用
/// 连上会话（宿主按它那份 VT 当前的模式编码）。宿主回 `Done` 算送到，回带着请求编号的 `Error` 算
/// 失败。会话列表上等回答的每一行、终端页底部各有一个。
@Observable
@MainActor
public final class QuickReplyModel {
    public let sessionId: SessionId
    /// 文本框里的字。
    public var draft = ""
    /// 还有请求没回话。
    public private(set) var isSending = false
    /// 每送到一批加一，视图据此轻震一下。
    public private(set) var deliveredCount = 0
    /// 每失败一批加一，视图据此震一下失败的样子。
    public private(set) var failedCount = 0
    public var errorMessage: String?

    @ObservationIgnored private let link: any HostLink
    @ObservationIgnored private var pending: Set<UInt32> = []
    @ObservationIgnored private var batchFailed = false
    @ObservationIgnored private let onDelivered: @MainActor (SessionId) -> Void

    /// `onDelivered` 在一批请求都送到后调，比如刷新这个会话的预览。
    public init(sessionId: SessionId, link: any HostLink, onDelivered: @escaping @MainActor (SessionId) -> Void = { _ in }) {
        self.sessionId = sessionId
        self.link = link
        self.onDelivered = onDelivered
    }

    public var canSendDraft: Bool {
        !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// 按一个键。
    public func press(_ key: QuickKey) async {
        let req = await link.nextRequestId()
        begin([req])
        link.send(.sendKeys(req: req, id: sessionId, keys: [key.key]))
    }

    /// 把文本框里的字粘贴进去再按回车。
    public func sendDraft() async {
        let text = draft
        guard canSendDraft else { return }
        draft = ""
        let paste = await link.nextRequestId()
        let enter = await link.nextRequestId()
        begin([paste, enter])
        link.send(.paste(req: paste, id: sessionId, text: text))
        link.send(.sendKeys(req: enter, id: sessionId, keys: ["enter"]))
    }

    private func begin(_ requests: [UInt32]) {
        if pending.isEmpty { batchFailed = false }
        pending.formUnion(requests)
        isSending = true
        errorMessage = nil
    }

    /// 看是不是回给这里的请求的；是的话处理掉，返回 true。
    @discardableResult
    public func handle(_ message: HostMsg) -> Bool {
        switch message {
        case .done(let req) where pending.contains(req):
            pending.remove(req)
        case .error(let req?, _, let message) where pending.contains(req):
            pending.remove(req)
            batchFailed = true
            errorMessage = "没发出去：\(message)"
        default:
            return false
        }
        if pending.isEmpty {
            isSending = false
            if batchFailed {
                failedCount += 1
            } else {
                deliveredCount += 1
                onDelivered(sessionId)
            }
        }
        return true
    }

    /// 连接断了：还没回话的请求不会再有回音，算失败。
    public func connectionLost() {
        guard !pending.isEmpty else { return }
        pending.removeAll()
        isSending = false
        failedCount += 1
        errorMessage = "连接断了，回复可能没送到"
    }
}
