@preconcurrency import AVFoundation
import Foundation

/// 录麦克风，交出 `SpeechRecorder.sampleRate` 的单声道样本。边录边把新的一段交给 `onChunk`（交给电脑
/// 识别时边录边发），停下时交出整段。
public final class SpeechRecorder: @unchecked Sendable {
    /// 交出的采样率，识别模型和交给电脑的录音都是这个。
    public static let sampleRate = 16_000.0

    private let engine = AVAudioEngine()
    /// AVAudioSession 的启停会卡住调用的线程，所以启停都放在这条串行队列上，按调用的先后做。
    /// 所有录音器共用一条：session 全 app 一份，上一段的停用不能排到下一段的启用之后。
    private static let queue = DispatchQueue(label: "dev.runode.speech.recorder")
    private let onChunk: (@Sendable ([Float]) -> Void)?
    private let lock = NSLock()
    private var samples: [Float] = []

    public init(onChunk: (@Sendable ([Float]) -> Void)? = nil) {
        self.onChunk = onChunk
    }

    public enum Permission: Sendable {
        case granted
        case denied
        /// 还没问过用户。
        case undetermined
    }

    public static var permission: Permission {
        #if os(iOS)
            switch AVAudioApplication.shared.recordPermission {
            case .granted: .granted
            case .denied: .denied
            default: .undetermined
            }
        #else
            .granted
        #endif
    }

    /// 弹系统的框问用户要麦克风权限，返回给没给。
    public static func requestPermission() async -> Bool {
        #if os(iOS)
            await AVAudioApplication.requestRecordPermission()
        #else
            true
        #endif
    }

    /// 开始录。调之前先要到麦克风权限（`permission`）。
    public func start() async throws {
        try await onQueue { try self.startNow() }
    }

    /// 停下，交出整段录音；样本全是 0 时报 `SpeechError.silent`。
    public func stop() async throws -> [Float] {
        try await onQueue { try self.stopNow() }
    }

    private func onQueue<T: Sendable>(_ work: @escaping @Sendable () throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { continuation in
            Self.queue.async { continuation.resume(with: Result { try work() }) }
        }
    }

    private func startNow() throws {
        #if os(iOS)
            let session = AVAudioSession.sharedInstance()
            // 不停掉别的 App 在放的声音，只压低它。
            try session.setCategory(.playAndRecord, mode: .default, options: [.duckOthers, .defaultToSpeaker])
            try session.setActive(true)
        #endif
        do {
            try startEngine()
        } catch {
            #if os(iOS)
                try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            #endif
            throw error
        }
    }

    private func startEngine() throws {
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard let target = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Self.sampleRate, channels: 1, interleaved: false),
            let converter = AVAudioConverter(from: format, to: target)
        else { throw SpeechError.microphoneUnavailable }
        input.installTap(onBus: 0, bufferSize: 4096, format: format) { [weak self] buffer, _ in
            self?.take(buffer, converter: converter, target: target)
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            throw error
        }
    }

    private func stopNow() throws -> [Float] {
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        #if os(iOS)
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
        let samples = lock.withLock { self.samples }
        if !samples.isEmpty, samples.allSatisfy({ $0 == 0 }) { throw SpeechError.silent }
        return samples
    }

    private func take(_ buffer: AVAudioPCMBuffer, converter: AVAudioConverter, target: AVAudioFormat) {
        let capacity = AVAudioFrameCount(Double(buffer.frameLength) * target.sampleRate / buffer.format.sampleRate) + 1
        guard let output = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return }
        var fed = false
        var error: NSError?
        converter.convert(to: output, error: &error) { _, status in
            if fed {
                status.pointee = .noDataNow
                return nil
            }
            fed = true
            status.pointee = .haveData
            return buffer
        }
        guard error == nil, let channel = output.floatChannelData?[0], output.frameLength > 0 else { return }
        let chunk = Array(UnsafeBufferPointer(start: channel, count: Int(output.frameLength)))
        lock.withLock { samples.append(contentsOf: chunk) }
        onChunk?(chunk)
    }
}
