import Foundation

#if canImport(SherpaOnnxC)
    import SherpaOnnxC
#endif

/// 手机上的识别：第一次用时加载 `SpeechModels` 下好的模型，闲了 `idleUnload` 之后卸掉，模型加载后占
/// 几百 MB 内存。
public actor SpeechEngine {
    static let idleUnload: Duration = .seconds(120)

    private let root: URL
    private var recognizer: Recognizer?
    private var unload: Task<Void, Never>?

    public init(root: URL) {
        self.root = root
    }

    /// 识别 `samples`（`SpeechRecorder.sampleRate` 的单声道）；没听出话来时是空的。
    public func transcribe(_ samples: [Float]) throws -> String {
        let recognizer = try self.recognizer ?? Recognizer(root: root)
        self.recognizer = recognizer
        let text = recognizer.transcribe(samples)
        unload?.cancel()
        unload = Task {
            try? await Task.sleep(for: Self.idleUnload)
            if !Task.isCancelled { self.recognizer = nil }
        }
        return text
    }
}

/// 把一句接到前面的文字后面；两边都是西文时中间加空格，中文之间不加。
func append(_ sentence: String, to text: inout String) {
    func latin(_ scalar: Unicode.Scalar?) -> Bool { scalar.map { (0x21...0x7E).contains($0.value) } ?? false }
    if latin(text.unicodeScalars.last), latin(sentence.unicodeScalars.first) { text += " " }
    text += sentence
}

#if canImport(SherpaOnnxC)
    /// SenseVoice 加 Silero VAD：先按停顿切成句，一句一句识别再接起来，长录音也不会整段塞进模型。
    private final class Recognizer: @unchecked Sendable {
        private let asr: OpaquePointer
        private let vad: OpaquePointer
        private static let window = 512

        init(root: URL) throws {
            guard SpeechModels.isInstalled(in: root) else { throw SpeechError.modelUnavailable }
            // C 接口只在创建时读这些字符串。
            var owned: [UnsafeMutablePointer<CChar>] = []
            defer { owned.forEach { free($0) } }
            func c(_ string: String) -> UnsafePointer<CChar> {
                let copy = strdup(string)!
                owned.append(copy)
                return UnsafePointer(copy)
            }
            var config = SherpaOnnxOfflineRecognizerConfig()
            config.feat_config.sample_rate = Int32(SpeechRecorder.sampleRate)
            config.feat_config.feature_dim = 80
            config.model_config.tokens = c(SpeechModels.tokens.path(in: root).path)
            config.model_config.num_threads = Int32(min(4, ProcessInfo.processInfo.activeProcessorCount))
            config.model_config.sense_voice.model = c(SpeechModels.senseVoice.path(in: root).path)
            config.model_config.sense_voice.language = c("auto")
            // 加标点、数字写成阿拉伯数字。
            config.model_config.sense_voice.use_itn = 1
            config.decoding_method = c("greedy_search")
            guard let asr = SherpaOnnxCreateOfflineRecognizer(&config) else { throw SpeechError.modelUnavailable }
            var vadConfig = SherpaOnnxVadModelConfig()
            vadConfig.silero_vad.model = c(SpeechModels.vad.path(in: root).path)
            vadConfig.silero_vad.threshold = 0.5
            vadConfig.silero_vad.min_silence_duration = 0.5
            vadConfig.silero_vad.min_speech_duration = 0.25
            vadConfig.silero_vad.window_size = Int32(Self.window)
            vadConfig.silero_vad.max_speech_duration = 20
            vadConfig.sample_rate = Int32(SpeechRecorder.sampleRate)
            vadConfig.num_threads = 1
            guard let vad = SherpaOnnxCreateVoiceActivityDetector(&vadConfig, 30) else {
                SherpaOnnxDestroyOfflineRecognizer(asr)
                throw SpeechError.modelUnavailable
            }
            self.asr = asr
            self.vad = vad
        }

        deinit {
            SherpaOnnxDestroyVoiceActivityDetector(vad)
            SherpaOnnxDestroyOfflineRecognizer(asr)
        }

        func transcribe(_ samples: [Float]) -> String {
            SherpaOnnxVoiceActivityDetectorReset(vad)
            var text = ""
            samples.withUnsafeBufferPointer { all in
                var at = 0
                while at < all.count {
                    let count = min(Self.window, all.count - at)
                    SherpaOnnxVoiceActivityDetectorAcceptWaveform(vad, all.baseAddress! + at, Int32(count))
                    at += count
                    drain(into: &text)
                }
            }
            SherpaOnnxVoiceActivityDetectorFlush(vad)
            drain(into: &text)
            return text
        }

        private func drain(into text: inout String) {
            while SherpaOnnxVoiceActivityDetectorEmpty(vad) == 0 {
                if let segment = SherpaOnnxVoiceActivityDetectorFront(vad) {
                    let sentence = decode(segment.pointee.samples, count: segment.pointee.n)
                    SherpaOnnxDestroySpeechSegment(segment)
                    if !sentence.isEmpty { append(sentence, to: &text) }
                }
                SherpaOnnxVoiceActivityDetectorPop(vad)
            }
        }

        private func decode(_ samples: UnsafePointer<Float>?, count: Int32) -> String {
            guard let samples, let stream = SherpaOnnxCreateOfflineStream(asr) else { return "" }
            defer { SherpaOnnxDestroyOfflineStream(stream) }
            SherpaOnnxAcceptWaveformOffline(stream, Int32(SpeechRecorder.sampleRate), samples, count)
            SherpaOnnxDecodeOfflineStream(asr, stream)
            guard let result = SherpaOnnxGetOfflineStreamResult(stream) else { return "" }
            defer { SherpaOnnxDestroyOfflineRecognizerResult(result) }
            return result.pointee.text.map { String(cString: $0) }?.trimmingCharacters(in: .whitespaces) ?? ""
        }
    }
#else
    private final class Recognizer: Sendable {
        init(root: URL) throws { throw SpeechError.unsupported }
        func transcribe(_ samples: [Float]) -> String { "" }
    }
#endif
