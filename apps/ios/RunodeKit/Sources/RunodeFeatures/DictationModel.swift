import Foundation
import Observation
import RunodeConnection
import RunodeProtocol
import RunodeSpeech
import RunodeTerminal
import Synchronization

/// 连上一台电脑的识别。App 入口给真的（`SpeechConnection.open`），演示模式和测试不给。
public typealias SpeechOpener = @Sendable (MachineRecord) async throws -> SpeechConnection

/// 语音输入在哪里识别。
public enum DictationSource: String, Hashable, Sendable, CaseIterable, Codable {
    /// 手机上识别；手机上还没下载模型时交给电脑。
    case phone
    /// 交给终端所在的电脑识别，用电脑设置里的模型。
    case computer
}

/// 录音的一方，真的是 `SpeechRecorder`，测试换成假的。
protocol DictationRecorder: AnyObject, Sendable {
    func start() throws
    func stop() throws -> [Float]
}

extension SpeechRecorder: DictationRecorder {}

/// 语音输入：手机上识别模型的下载（设置页看它），和终端页上的一次次录音、识别。全 app 一份，同一时刻
/// 只有一段在录。辅助栏上的麦克风键按住超过 `hold` 再松开就结束，点一下就一直录到再按一次；录满
/// `maxSpeechSeconds` 自己结束。识别出的文字交给开始录音时给的 `insert`。
@Observable
@MainActor
public final class DictationModel {
    public enum Download: Hashable, Sendable {
        case idle
        /// 已经下了这么多字节，总数是 `SpeechModels.downloadSize`。
        case running(Int64)
        case failed(String)
    }

    public enum Phase: Hashable, Sendable {
        case idle
        case recording
        case transcribing
    }

    static let hold: Duration = .milliseconds(350)

    public private(set) var download: Download = .idle
    /// 手机上的模型下好了。
    public private(set) var installed: Bool
    public private(set) var phase: Phase = .idle
    /// 录音、识别出的错，终端页弹出来说。
    public var errorMessage: String?

    @ObservationIgnored private let root: URL
    @ObservationIgnored private let openSpeech: SpeechOpener?
    @ObservationIgnored private let makeRecorder: (@escaping @Sendable ([Float]) -> Void) -> any DictationRecorder
    @ObservationIgnored private let transcribe: @Sendable ([Float]) async throws -> String
    @ObservationIgnored private var downloading: Task<Void, Never>?
    @ObservationIgnored private var take: Take?

    /// 正在录的一段。
    private final class Take {
        let recorder: any DictationRecorder
        /// 交给电脑识别时连电脑的那次连接。
        let remote: Task<SpeechConnection, any Error>?
        let insert: (String) -> Void
        let started = ContinuousClock.now
        /// 麦克风键还按着。
        var held = true

        init(recorder: any DictationRecorder, remote: Task<SpeechConnection, any Error>?, insert: @escaping (String) -> Void) {
            self.recorder = recorder
            self.remote = remote
            self.insert = insert
        }

        /// 放弃交给电脑的识别：还在连的不连了，连上了的关掉。
        func closeRemote() {
            guard let remote else { return }
            remote.cancel()
            Task { try? await remote.value.close() }
        }
    }

    public convenience init(root: URL, openSpeech: SpeechOpener?) {
        let engine = SpeechEngine(root: root)
        self.init(
            root: root, openSpeech: openSpeech, makeRecorder: { SpeechRecorder(onChunk: $0) },
            transcribe: { try await engine.transcribe($0) })
    }

    init(
        root: URL, openSpeech: SpeechOpener?,
        makeRecorder: @escaping (@escaping @Sendable ([Float]) -> Void) -> any DictationRecorder,
        transcribe: @escaping @Sendable ([Float]) async throws -> String
    ) {
        self.root = root
        self.openSpeech = openSpeech
        self.makeRecorder = makeRecorder
        self.transcribe = transcribe
        installed = SpeechModels.isInstalled(in: root)
    }

    // MARK: 模型

    /// 在后台下载手机上的模型。中文界面先试 ModelScope，国内连 HuggingFace 常常不通。
    public func startDownload() {
        guard downloading == nil, !installed else { return }
        let chinaFirst = Locale.preferredLanguages.first?.hasPrefix("zh") ?? false
        download = .running(0)
        let root = root
        downloading = Task { [weak self] in
            let outcome: Result<Void, any Error>
            do {
                try await SpeechModels.download(to: root, chinaFirst: chinaFirst) { done in
                    Task { @MainActor in self?.progressed(done) }
                }
                outcome = .success(())
            } catch {
                outcome = .failure(error)
            }
            self?.downloaded(outcome)
        }
    }

    public func cancelDownload() {
        downloading?.cancel()
    }

    public func remove() {
        do {
            try SpeechModels.remove(from: root)
            installed = false
        } catch {
            download = .failed(error.localizedDescription)
        }
    }

    private func progressed(_ done: Int64) {
        guard case .running(let shown) = download else { return }
        // 按百分比刷新：每到一小块都刷的话一次下载要刷几千次界面。
        let total = SpeechModels.downloadSize
        if done * 100 / total != shown * 100 / total { download = .running(done) }
    }

    private func downloaded(_ outcome: Result<Void, any Error>) {
        downloading = nil
        switch outcome {
        case .success:
            installed = true
            download = .idle
        case .failure(let error) where error is CancellationError:
            download = .idle
        case .failure(let error):
            download = .failed(Self.message(for: error))
        }
    }

    // MARK: 录音

    /// 麦克风键的一次动作。`machine` 是终端所在的电脑，交给电脑识别时连它。
    public func press(
        _ press: DictationPress, machine: MachineRecord?, source: DictationSource, insert: @escaping (String) -> Void
    ) {
        switch (press, phase) {
        case (.down, .idle):
            start(machine: machine, source: source, insert: insert)
        case (.down, .recording) where take?.held == false:
            finish()
        case (.up, .recording) where take?.held == true:
            take?.held = false
            if let take, ContinuousClock.now - take.started >= Self.hold { finish() }
        case (.cancel, .recording) where take?.held == true:
            cancel()
        default:
            break
        }
    }

    /// 丢掉正在录的（离开终端页时）。
    public func cancel() {
        guard let take else { return }
        self.take = nil
        phase = .idle
        _ = try? take.recorder.stop()
        take.closeRemote()
    }

    private func start(machine: MachineRecord?, source: DictationSource, insert: @escaping (String) -> Void) {
        switch SpeechRecorder.permission {
        case .granted:
            break
        case .undetermined:
            // 这一次只问权限：系统的框弹出来时手指早松开了，等用户再按。
            Task { _ = await SpeechRecorder.requestPermission() }
            return
        case .denied:
            errorMessage = String(localized: "Runode 没有麦克风权限，请到系统设置里打开")
            return
        }
        let local = source == .phone && installed
        guard local || (machine != nil && openSpeech != nil) else {
            errorMessage = String(localized: "请先在设置里下载语音模型")
            return
        }
        let feed = Feed()
        let onChunk: @Sendable ([Float]) -> Void = { chunk in if !local { feed.push(chunk) } }
        let recorder = makeRecorder(onChunk)
        var remote: Task<SpeechConnection, any Error>?
        if !local, let machine, let openSpeech {
            remote = Task {
                let connection = try await openSpeech(machine)
                feed.attach(connection)
                return connection
            }
        }
        do {
            try recorder.start()
        } catch {
            if let remote {
                remote.cancel()
                Task { try? await remote.value.close() }
            }
            errorMessage = Self.message(for: error)
            return
        }
        let take = Take(recorder: recorder, remote: remote, insert: insert)
        self.take = take
        phase = .recording
        Task { [weak self, weak take] in
            try? await Task.sleep(for: .seconds(maxSpeechSeconds))
            if let take, self?.take === take { self?.finish() }
        }
    }

    private func finish() {
        guard let take else { return }
        self.take = nil
        phase = .transcribing
        let transcribe = transcribe
        Task {
            do {
                let samples = try take.recorder.stop()
                let text: String
                if let remote = take.remote {
                    let connection = try await remote.value
                    guard connection.installed else {
                        connection.close()
                        throw DictationFailure.noModelAnywhere
                    }
                    text = try await connection.finish()
                } else {
                    text = try await transcribe(samples)
                }
                if !text.isEmpty { take.insert(text) }
            } catch {
                take.closeRemote()
                if !(error is CancellationError) { errorMessage = Self.message(for: error) }
            }
            phase = .idle
        }
    }

    static func message(for error: any Error) -> String {
        switch error {
        case SpeechError.silent: String(localized: "麦克风只录到了静音，请检查 Runode 的麦克风权限")
        case SpeechError.microphoneUnavailable: String(localized: "没有能用的麦克风")
        case SpeechError.modelUnavailable: String(localized: "语音模型不完整，请在设置里删除后重新下载")
        case SpeechError.corruptDownload: String(localized: "下载的模型文件校验不过，请重试")
        case SpeechError.httpStatus(let status): String(localized: "下载失败（HTTP \(status)）")
        case SpeechError.unsupported: String(localized: "这台设备上不能识别")
        case DictationFailure.noModelAnywhere: String(localized: "手机和电脑上都还没下载语音模型，请在设置里下载")
        default: error.localizedDescription
        }
    }
}

enum DictationFailure: Error {
    /// 手机上没下模型，交给电脑时电脑上也没下。
    case noModelAnywhere
}

/// 交给电脑识别时的录音：连上电脑之前录到的先攒着，连上后一次补发，之后边录边发。超过
/// `maxSpeechSeconds` 的不发：定时结束总比录音晚到一点，多出的几块会让电脑拒掉整段。
private final class Feed: Sendable {
    private struct State {
        var connection: SpeechConnection?
        var pending: [[Float]] = []
        /// 还能发的样本数。
        var budget = maxSpeechSeconds * speechSampleRate
    }

    private let state = Mutex(State())

    func push(_ chunk: [Float]) {
        state.withLock { state in
            let chunk = Array(chunk.prefix(state.budget))
            guard !chunk.isEmpty else { return }
            state.budget -= chunk.count
            if let connection = state.connection { connection.send(chunk) } else { state.pending.append(chunk) }
        }
    }

    func attach(_ connection: SpeechConnection) {
        state.withLock { state in
            state.connection = connection
            state.pending.forEach(connection.send)
            state.pending = []
        }
    }
}
