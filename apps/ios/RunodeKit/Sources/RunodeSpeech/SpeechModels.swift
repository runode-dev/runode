import CryptoKit
import Foundation

/// 手机上识别要的模型文件（SenseVoice 和切句用的 Silero VAD）和它们的下载。和电脑那边
/// `runode_speech` 的清单是同一批文件、同样的目录布局。每个文件按大小和 SHA-256 核对过才改成正式的
/// 名字，所以目录里有这个文件就说明它是完整的。
public enum SpeechModels {
    struct Asset: Sendable {
        /// 在模型目录下的子目录。
        let dir: String
        let name: String
        /// HuggingFace 上原作者的仓库。
        let hfRepo: String
        let size: Int64
        let sha256: String

        func path(in root: URL) -> URL {
            root.appending(path: dir).appending(path: name)
        }

        func urls(chinaFirst: Bool) -> [URL] {
            let hf = URL(string: "https://huggingface.co/\(hfRepo)/resolve/main/\(name)")!
            let ms = URL(string: "\(modelScope)/\(dir)/\(name)")!
            return chinaFirst ? [ms, hf] : [hf, ms]
        }
    }

    /// ModelScope 上 runode 自己的镜像，按 `<dir>/<name>` 放和 HuggingFace 上一样的文件。
    static let modelScope = "https://modelscope.cn/models/runode/speech-models/resolve/master"

    static let senseVoice = Asset(
        dir: "sense-voice-int8-2024-07-17", name: "model.int8.onnx",
        hfRepo: "csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17", size: 239_233_841,
        sha256: "c71f0ce00bec95b07744e116345e33d8cbbe08cef896382cf907bf4b51a2cd51")
    static let tokens = Asset(
        dir: "sense-voice-int8-2024-07-17", name: "tokens.txt",
        hfRepo: "csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17", size: 315_894,
        sha256: "f449eb28dc567533d7fa59be34e2abca8784f771850c78a47fb731a31429a1dc")
    static let vad = Asset(
        dir: "silero-vad", name: "silero_vad.onnx", hfRepo: "csukuangfj/vad", size: 1_807_522,
        sha256: "a35ebf52fd3ce5f1469b2a36158dba761bc47b973ea3382b3186ca15b1f5af28")
    static let assets = [senseVoice, tokens, vad]

    /// 全部文件的字节数。
    public static var downloadSize: Int64 { assets.reduce(0) { $0 + $1.size } }

    public static func isInstalled(in root: URL) -> Bool {
        assets.allSatisfy { FileManager.default.fileExists(atPath: $0.path(in: root).path) }
    }

    public static func remove(from root: URL) throws {
        for dir in Set(assets.map(\.dir)) {
            let url = root.appending(path: dir)
            if FileManager.default.fileExists(atPath: url.path) { try FileManager.default.removeItem(at: url) }
        }
    }

    /// 把缺的文件下载到 `root` 下。`chinaFirst` 时先试 ModelScope 再试 HuggingFace，否则反过来；一个源
    /// 失败（连不上、断了、核对不过）就换另一个。下载期间不时调 `progress(已有的字节)`。任务取消时停下。
    ///
    /// shortcut: 断了从头下，App 退到后台久了系统会掐断下载；常见了改用后台 `URLSession` 加断点续传
    public static func download(
        to root: URL, chinaFirst: Bool, progress: @escaping @Sendable (Int64) -> Void
    ) async throws {
        var done: Int64 = 0
        for asset in assets {
            let target = asset.path(in: root)
            if FileManager.default.fileExists(atPath: target.path) {
                done += asset.size
                continue
            }
            try FileManager.default.createDirectory(at: target.deletingLastPathComponent(), withIntermediateDirectories: true)
            var failure: (any Error)?
            for url in asset.urls(chinaFirst: chinaFirst) {
                try Task.checkCancellation()
                do {
                    let before = done
                    let file = try await fetch(url) { progress(before + $0) }
                    try verify(file, against: asset)
                    try FileManager.default.moveItem(at: file, to: target)
                    failure = nil
                    break
                } catch is CancellationError {
                    throw CancellationError()
                } catch {
                    failure = error
                }
            }
            if let failure { throw failure }
            done += asset.size
            progress(done)
        }
        var excluded = URLResourceValues()
        excluded.isExcludedFromBackup = true
        var root = root
        try? root.setResourceValues(excluded)
    }

    /// 下载 `url` 到一个临时文件，返回它的位置。
    private static func fetch(_ url: URL, progress: @escaping @Sendable (Int64) -> Void) async throws -> URL {
        let fetcher = Fetcher(progress: progress)
        let session = URLSession(configuration: .ephemeral, delegate: fetcher, delegateQueue: nil)
        defer { session.finishTasksAndInvalidate() }
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                fetcher.start(session.downloadTask(with: url), continuation: continuation)
            }
        } onCancel: {
            fetcher.cancel()
        }
    }

    private static func verify(_ file: URL, against asset: Asset) throws {
        let handle = try FileHandle(forReadingFrom: file)
        defer { try? handle.close() }
        var hash = SHA256()
        var size: Int64 = 0
        while let chunk = try handle.read(upToCount: 1 << 20), !chunk.isEmpty {
            hash.update(data: chunk)
            size += Int64(chunk.count)
        }
        let digest = hash.finalize().map { String(format: "%02x", $0) }.joined()
        guard size == asset.size, digest == asset.sha256 else {
            try? FileManager.default.removeItem(at: file)
            throw SpeechError.corruptDownload(asset.name)
        }
    }
}

public enum SpeechError: Error, Equatable, Sendable {
    /// 下载的文件大小或 SHA-256 对不上。
    case corruptDownload(String)
    /// 服务器回了出错的状态码。
    case httpStatus(Int)
    /// 麦克风交来的样本全是 0：没有录音权限时系统不报错，只给静音。
    case silent
    /// 模型文件不全或者读不了。
    case modelUnavailable
    /// 没有能用的麦克风，或者它的格式转不成识别要的。
    case microphoneUnavailable
    /// 这个平台上没有识别（`swift test` 跑在 Mac 上时）。
    case unsupported
}

/// 一次下载的代理：报进度，下完把临时文件挪到自己的位置（系统在代理方法返回后就删掉它）。
private final class Fetcher: NSObject, URLSessionDownloadDelegate, @unchecked Sendable {
    private let progress: @Sendable (Int64) -> Void
    private let lock = NSLock()
    private var task: URLSessionDownloadTask?
    private var continuation: CheckedContinuation<URL, any Error>?
    private var file: URL?
    private var cancelled = false

    init(progress: @escaping @Sendable (Int64) -> Void) {
        self.progress = progress
    }

    func start(_ task: URLSessionDownloadTask, continuation: CheckedContinuation<URL, any Error>) {
        let cancelled = lock.withLock {
            self.task = task
            self.continuation = continuation
            return self.cancelled
        }
        if cancelled { task.cancel() } else { task.resume() }
    }

    func cancel() {
        let task = lock.withLock {
            cancelled = true
            return self.task
        }
        task?.cancel()
    }

    func urlSession(
        _ session: URLSession, downloadTask: URLSessionDownloadTask, didWriteData _: Int64, totalBytesWritten: Int64,
        totalBytesExpectedToWrite _: Int64
    ) {
        progress(totalBytesWritten)
    }

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
        if let http = downloadTask.response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) { return }
        let kept = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        if (try? FileManager.default.moveItem(at: location, to: kept)) != nil {
            lock.withLock { file = kept }
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: (any Error)?) {
        let (continuation, file, cancelled) = lock.withLock {
            defer { self.continuation = nil }
            return (self.continuation, self.file, self.cancelled)
        }
        if cancelled {
            continuation?.resume(throwing: CancellationError())
        } else if let error {
            continuation?.resume(throwing: error)
        } else if let file {
            continuation?.resume(returning: file)
        } else {
            let status = (task.response as? HTTPURLResponse)?.statusCode ?? 0
            continuation?.resume(throwing: SpeechError.httpStatus(status))
        }
    }
}
