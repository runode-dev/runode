import Foundation
import Observation
import RunodeConnection
import RunodeProtocol

/// 一条交图片、视频给电脑的连接，真的是 `MediaConnection`，测试换成假的。
public protocol MediaUploading: AnyObject, Sendable {
    /// 发一个文件的内容，返回它在电脑上的绝对路径。
    func send(_ data: Data, progress: MediaConnection.Progress?) async throws -> String
    /// 从文件里一块块读着发，返回它在电脑上的绝对路径。
    func send(file: URL, progress: MediaConnection.Progress?) async throws -> String
    func close()
}

extension MediaConnection: MediaUploading {}

/// 连上一台电脑收图片、视频的地方。App 入口给真的（`MediaConnection.open`），演示模式和测试不给。
public typealias MediaOpener = @Sendable (MachineRecord) async throws -> any MediaUploading

/// 把一段视频压成发给电脑的文件，返回新文件；真的是 `VideoPreparation.prepare`，测试换成假的。
public typealias VideoPreparer = @Sendable (URL) async throws -> URL

/// 开始一段退到后台后还能接着跑的时间，返回结束它的函数；系统叫停时由开始它的那边自己结束。App 入口给
/// `UIApplication` 的后台任务，测试和演示模式不给。
public typealias BackgroundTaskStarter = @MainActor (_ name: String) -> @MainActor () -> Void

/// 要发给终端的一项。
public enum MediaItem: Hashable, Sendable {
    /// 图片文件的内容。
    case image(Data)
    /// 一个视频文件。交给 `MediaSendModel.send` 后归它管，发完、出错或没轮到都会删掉。
    case video(URL)
}

/// 把手机上的图片、视频发给终端里的 agent：按先后一项项处理，图片重新编码（`ImagePreparation`），视频先压
/// （`VideoPreparation`）再从文件流式地发，经单独的一条连接交给电脑存好，再把电脑上的路径当粘贴打进终端。
/// 全 app 一份；一批还没发完时再来的一批排在后面。
@Observable
@MainActor
public final class MediaSendModel {
    public enum Phase: Hashable, Sendable {
        case idle
        /// 正在压一批里的第 `index` 项（从 1 数，是视频），这一批共 `count` 项。
        case preparing(index: Int, count: Int)
        /// 正在发一批里的第 `index` 项，这一批共 `count` 项。`fraction` 是发出去的比例，只有视频报，图片为空。
        case sending(index: Int, count: Int, fraction: Double?)
    }

    public private(set) var phase: Phase = .idle
    /// 发送出的错，终端页弹出来说。
    public var errorMessage: String?

    /// 存好一项时终端的连接没连着，最多等它重新连上这么久再粘贴；App 退到后台时连接会断开，回到前台再连上。
    public static let reconnectTimeout: Duration = .seconds(120)

    @ObservationIgnored private let open: MediaOpener?
    @ObservationIgnored private let prepareVideo: VideoPreparer
    @ObservationIgnored private let beginBackgroundTask: BackgroundTaskStarter?
    @ObservationIgnored private let reconnectWait: Duration
    /// 最后排上的一批，下一批等它发完再开始。
    @ObservationIgnored private var last: Task<Void, Never>?

    public init(
        open: MediaOpener?, prepareVideo: @escaping VideoPreparer = { try await VideoPreparation.prepare($0) },
        beginBackgroundTask: BackgroundTaskStarter? = nil, reconnectWait: Duration = MediaSendModel.reconnectTimeout
    ) {
        self.open = open
        self.prepareVideo = prepareVideo
        self.beginBackgroundTask = beginBackgroundTask
        self.reconnectWait = reconnectWait
    }

    /// 现在能不能往 `link` 连着的电脑发图片、视频；不能时设好 `errorMessage`。终端页在弹出挑选的界面之前先问。
    @discardableResult
    public func checkAvailable(link: any HostLink) -> Bool {
        if open == nil {
            errorMessage = String(localized: "演示模式下不能发送图片和视频")
            return false
        }
        // 没连上时 `HostLink.pastesPaths` 是上次连上时的，不能拿它说电脑太旧。
        if !link.isConnected {
            errorMessage = String(localized: "未连接")
            return false
        }
        if !link.pastesPaths {
            errorMessage = String(localized: "电脑上的 Runode 版本太旧，更新后才能发送图片和视频")
            return false
        }
        return true
    }

    /// 把 `items` 按先后发给 `machine`，每项存好后把路径粘贴进 `sessionId` 这个会话。中途出错时已经发出的
    /// 留着，后面的不发了；存好一项时终端的连接断着、`reconnectWait` 内没重新连上也停下，那项不粘贴。
    /// 排着和发着时开着后台任务，App 退到后台后还能接着发一会儿。
    public func send(_ items: [MediaItem], machine: MachineRecord, link: any HostLink, sessionId: SessionId) async {
        guard !items.isEmpty, checkAvailable(link: link), let open else {
            Self.discard(items)
            return
        }
        let previous = last
        let endBackgroundTask = beginBackgroundTask?("Send media")
        let batch = Task {
            defer { endBackgroundTask?() }
            await previous?.value
            await deliver(items, machine: machine, link: link, sessionId: sessionId, open: open)
        }
        last = batch
        await batch.value
    }

    private func deliver(
        _ items: [MediaItem], machine: MachineRecord, link: any HostLink, sessionId: SessionId, open: MediaOpener
    ) async {
        defer { phase = .idle }
        defer { Self.discard(items) }
        var connection: (any MediaUploading)?
        defer { connection?.close() }
        let count = items.count
        do {
            for (offset, item) in items.enumerated() {
                let index = offset + 1
                let path: String
                switch item {
                case .image(let data):
                    phase = .sending(index: index, count: count, fraction: nil)
                    let prepared = try await Task.detached { try ImagePreparation.prepare(data) }.value
                    if connection == nil { connection = try await open(machine) }
                    guard let connection else { return }
                    path = try await connection.send(prepared, progress: nil)
                case .video(let source):
                    phase = .preparing(index: index, count: count)
                    let prepared = try await prepareVideo(source)
                    defer { try? FileManager.default.removeItem(at: prepared) }
                    if connection == nil { connection = try await open(machine) }
                    guard let connection else { return }
                    phase = .sending(index: index, count: count, fraction: 0)
                    path = try await connection.send(file: prepared) { [weak self] sent, total in
                        let fraction = Double(sent) / Double(max(total, 1))
                        Task { @MainActor in self?.advance(index: index, count: count, to: fraction) }
                    }
                }
                guard await waitUntilConnected(link) else { throw TerminalDisconnected() }
                link.send(.pastePath(req: link.nextRequestId(), id: sessionId, path: path))
            }
        } catch is CancellationError {
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// 等 `link` 连上，最多 `reconnectWait`；连着时为真。
    private func waitUntilConnected(_ link: any HostLink) async -> Bool {
        if link.isConnected { return true }
        let events = await link.events()
        let timeout = reconnectWait
        return await withTaskGroup(of: Bool.self) { group in
            group.addTask {
                for await event in events {
                    if case .ready = event, link.isConnected { return true }
                }
                return false
            }
            group.addTask {
                try? await Task.sleep(for: timeout)
                return false
            }
            let connected = await group.next() ?? false
            group.cancelAll()
            return connected
        }
    }

    /// 进度从连接那边转到主线程时可能晚到：只往前走，换到别的项之后不再改。
    private func advance(index: Int, count: Int, to fraction: Double) {
        guard case .sending(index, count, let current?) = phase, fraction > current else { return }
        phase = .sending(index: index, count: count, fraction: fraction)
    }

    private nonisolated static func discard(_ items: [MediaItem]) {
        for case .video(let url) in items {
            try? FileManager.default.removeItem(at: url)
        }
    }
}

/// 终端的连接断了：存好的文件的路径粘贴不进去，`HostLink.send` 会把 `pastePath` 丢掉。
struct TerminalDisconnected: LocalizedError {
    var errorDescription: String? { String(localized: "和电脑的连接断开了") }
}
