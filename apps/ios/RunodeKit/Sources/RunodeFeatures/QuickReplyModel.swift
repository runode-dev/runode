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
    /// 选提问里一项的键：所答的提问。
    let answers: AgentPrompt.Asked?

    public var id: String { key }

    init(
        label: String, key: String, accessibilityLabel: String, symbol: String? = nil,
        answers: AgentPrompt.Asked? = nil
    ) {
        self.label = label
        self.key = key
        self.accessibilityLabel = accessibilityLabel
        self.symbol = symbol
        self.answers = answers
    }

    /// 回答 agent 提问最常用的几个键：选项编号、是否、回车、Esc、上下移动选项。
    public static let standard: [QuickKey] = [
        QuickKey(label: "1", key: "1", accessibilityLabel: "1"),
        QuickKey(label: "2", key: "2", accessibilityLabel: "2"),
        QuickKey(label: "3", key: "3", accessibilityLabel: "3"),
        QuickKey(label: "y", key: "y", accessibilityLabel: "y"),
        QuickKey(label: "n", key: "n", accessibilityLabel: "n"),
        QuickKey(label: "⏎", key: "enter", accessibilityLabel: String(localized: "回车"), symbol: "return"),
        QuickKey(label: "esc", key: "esc", accessibilityLabel: "Esc"),
        QuickKey(label: "↑", key: "up", accessibilityLabel: String(localized: "上"), symbol: "arrowtriangle.up.fill"),
        QuickKey(label: "↓", key: "down", accessibilityLabel: String(localized: "下"), symbol: "arrowtriangle.down.fill"),
    ]

    /// 终端页底部回答那一行的键：选项编号。终端本身能打字，别的键不放。
    public static let options = Array(standard.prefix(3))
}

/// 一个会话的快速回复：按键经 `send_keys`、文字经 `paste` 加 `send_keys ["enter"]` 发给宿主，不用
/// 连上会话（宿主按它那份 VT 当前的模式编码）。宿主回 `Done` 算送到，回带着请求编号的 `Error` 算
/// 失败。会话列表上等回答的每一行、终端页底部各有一个，连接断开、连上由它们告诉这里。
@Observable
@MainActor
public final class QuickReplyModel {
    public let sessionId: SessionId
    /// 文本框里的字。
    public var draft = ""
    /// 连着电脑：断开时视图禁用回复栏，按键和发送也不发。
    public internal(set) var isConnected: Bool
    /// 每送到一批加一，视图据此轻震一下。
    public private(set) var deliveredCount = 0
    /// 每失败一批加一，视图据此震一下失败的样子。
    public private(set) var failedCount = 0
    public var errorMessage: String?

    @ObservationIgnored private let link: any HostLink
    @ObservationIgnored private var pending: Set<UInt32> = []
    /// 还没回话的粘贴请求发的字：没送到时放回文本框，用户不用重打。
    @ObservationIgnored private var unsent: [UInt32: String] = [:]
    /// 上次放回文本框的字：文本框还是它时，再有没送到的接在后面，几条都失败时不只剩第一条。
    @ObservationIgnored private var restored: String?
    @ObservationIgnored private var batchFailed = false
    /// 回答前重读屏幕的请求：所答的提问和要选的编号。
    @ObservationIgnored private var rereads: [UInt32: (asked: AgentPrompt.Asked, number: Int)] = [:]
    /// 在路上的那个回答眼下等的请求：先是重读，再是按键。同时只有一个回答在路上，前一个的键还没送到时重读到的
    /// 还是所答的提问，看不出它已经答过了。
    @ObservationIgnored private var answering: UInt32?
    /// 有回答在路上时又点的选项，按点的先后排着。
    @ObservationIgnored private var queued: [(asked: AgentPrompt.Asked, number: Int)] = []
    /// 在路上的是单选题的按键：它一送到这一问就答完了，agent 却未必已经重画，这时又点的对的是答过的这一问，作废。
    /// 多选题点一项只切换勾选、不离开这一问，排着的照答。
    @ObservationIgnored private var answeredOnce = false
    @ObservationIgnored private let onDelivered: @MainActor (SessionId) -> Void
    @ObservationIgnored private let onStale: @MainActor (SessionId) -> Void

    /// `onDelivered` 在一批请求都送到后调，比如刷新这个会话的预览。`onStale` 在回答前重读到的屏幕上换了提问、
    /// 没有发键时调，界面上的提问该跟着刷新了。
    public init(
        sessionId: SessionId, link: any HostLink, connected: Bool = true,
        onDelivered: @escaping @MainActor (SessionId) -> Void = { _ in },
        onStale: @escaping @MainActor (SessionId) -> Void = { _ in }
    ) {
        self.sessionId = sessionId
        self.link = link
        self.isConnected = connected
        self.onStale = onStale
        self.onDelivered = onDelivered
    }

    public var canSendDraft: Bool {
        !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// 按一个键。选提问里一项的键（`QuickKey.answers`）先重读屏幕，上面还是所答的提问才按读到的提问上选这一项的键
    /// （`AgentPrompt.keys(for:)`）；换了提问就不按，算失败。有回答在路上时先排着，它的键送到或失败了再重读。
    public func press(_ key: QuickKey) async {
        guard ensureConnected() else { return }
        if let asked = key.answers, let number = Int(key.key) {
            guard !answeredOnce else { return }
            begin([])
            if answering == nil {
                answer(asked, number)
            } else {
                queued.append((asked, number))
            }
            return
        }
        let req = link.nextRequestId()
        begin([req])
        link.send(.sendKeys(req: req, id: sessionId, keys: [key.key]))
    }

    private func answer(_ asked: AgentPrompt.Asked, _ number: Int) {
        let req = link.nextRequestId()
        pending.insert(req)
        rereads[req] = (asked, number)
        answering = req
        link.send(.readScreen(id: sessionId, lines: SessionListModel.waitingReadLines, req: req))
    }

    /// 在路上的回答完了（送到、失败或换了提问）：排着的下一个接着答。
    private func answerEnded() {
        answering = nil
        answeredOnce = false
        guard !queued.isEmpty else { return }
        let (asked, number) = queued.removeFirst()
        answer(asked, number)
    }

    /// 把文本框里的字粘贴进去再按回车。
    public func sendDraft() async {
        let text = draft
        guard canSendDraft, fits(text), ensureConnected() else { return }
        draft = ""
        restored = nil
        let paste = link.nextRequestId()
        let enter = link.nextRequestId()
        begin([paste, enter])
        unsent[paste] = text
        link.send(.paste(req: paste, id: sessionId, text: text))
        link.send(.sendKeys(req: enter, id: sessionId, keys: ["enter"]))
    }

    /// 这段字装得进一条控制消息：超了电脑断开连接，放回文本框重发又断。不拆成几次粘贴，那样程序收到的是
    /// 几段分开的粘贴。装不下时按失败报出来，草稿留着。
    private func fits(_ text: String) -> Bool {
        // 请求编号取最长的，算出来的就是发出去时最大的样子。
        let message = ClientMsg.paste(req: .max, id: sessionId, text: text)
        if let size = try? Frame.control(message).payload.count, size <= Frame.maxControlPayload { return true }
        failedCount += 1
        errorMessage = String(localized: "文字太长了，发不出去")
        return false
    }

    /// 断着时 `HostConnection` 会把消息丢掉：不发，按失败报出来，草稿留着。
    private func ensureConnected() -> Bool {
        guard !isConnected else { return true }
        failedCount += 1
        errorMessage = String(localized: "未连接")
        return false
    }

    /// 没送到的字放回文本框；用户已经打了新的字时不盖掉，文本框里还是刚放回的字时接在后面。
    private func restoreDraft(_ text: String?) {
        guard let text else { return }
        if draft.isEmpty {
            draft = text
        } else if draft == restored {
            draft += "\n" + text
        } else {
            return
        }
        restored = draft
    }

    private func begin(_ requests: [UInt32]) {
        if pending.isEmpty { batchFailed = false }
        pending.formUnion(requests)
        errorMessage = nil
    }

    /// 看是不是回给这里的请求的；是的话处理掉，返回 true。
    @discardableResult
    public func handle(_ message: HostMsg) -> Bool {
        switch message {
        case .done(let req) where pending.contains(req):
            pending.remove(req)
            unsent[req] = nil
            if req == answering { answerEnded() }
        case .screenText(_, let text, _, let req?) where rereads[req] != nil:
            let (asked, number) = rereads.removeValue(forKey: req)!
            let found = AgentPrompt.parse(text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init))
            if let found, found.choices == asked, let keys = found.keys(for: number) {
                let send = link.nextRequestId()
                pending.insert(send)
                answering = send
                if !found.isMultiSelect {
                    answeredOnce = true
                    queued.removeAll()
                }
                link.send(.sendKeys(req: send, id: sessionId, keys: keys))
            } else {
                batchFailed = true
                errorMessage = String(localized: "提问已经变了，没有发")
                onStale(sessionId)
                answerEnded()
            }
            pending.remove(req)
        case .error(let req?, _, let message) where pending.contains(req):
            pending.remove(req)
            rereads[req] = nil
            restoreDraft(unsent.removeValue(forKey: req))
            batchFailed = true
            errorMessage = String(localized: "没发出去：\(message)")
            if req == answering { answerEnded() }
        default:
            return false
        }
        if pending.isEmpty {
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
        isConnected = false
        guard !pending.isEmpty else { return }
        pending.removeAll()
        rereads.removeAll()
        answering = nil
        answeredOnce = false
        queued.removeAll()
        // 连着发了几条都没回话时按发出的先后拼回去，只放回一段的话别的就丢了。
        if !unsent.isEmpty { restoreDraft(unsent.keys.sorted().compactMap { unsent[$0] }.joined(separator: "\n")) }
        unsent.removeAll()
        failedCount += 1
        errorMessage = String(localized: "连接断了，回复可能没送到")
    }
}
