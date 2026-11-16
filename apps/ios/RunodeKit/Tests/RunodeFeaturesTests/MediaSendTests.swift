import CoreGraphics
import Foundation
import ImageIO
import RunodeConnection
import RunodeProtocol
import Synchronization
import Testing
import UniformTypeIdentifiers

@testable import RunodeFeatures

/// 假的交文件连接：按收到的先后给路径，`failAt` 那项（从 0 数）回失败。每项发之前先调 `willSend`。
final class FakeMediaUpload: MediaUploading {
    enum Sent: Equatable {
        /// 整段交来的内容。
        case data(Data)
        /// 从文件读着发的，带文件的内容。
        case file(Data)
    }

    private struct State {
        var received: [Sent] = []
        var closed = false
    }

    private let state = Mutex(State())
    let failAt: Int?
    let willSend: @Sendable (_ index: Int, _ progress: MediaConnection.Progress?) async -> Void

    init(
        failAt: Int? = nil,
        willSend: @escaping @Sendable (_ index: Int, _ progress: MediaConnection.Progress?) async -> Void = { _, _ in }
    ) {
        self.failAt = failAt
        self.willSend = willSend
    }

    var received: [Sent] { state.withLock { $0.received } }
    var closed: Bool { state.withLock { $0.closed } }

    func send(_ data: Data, progress: MediaConnection.Progress?) async throws -> String {
        try await record(.data(data), progress: progress)
    }

    func send(file: URL, progress: MediaConnection.Progress?) async throws -> String {
        try await record(.file(try Data(contentsOf: file)), progress: progress)
    }

    private func record(_ sent: Sent, progress: MediaConnection.Progress?) async throws -> String {
        let index = state.withLock { state in
            state.received.append(sent)
            return state.received.count - 1
        }
        await willSend(index, progress)
        if index == failAt { throw MediaFailure(reason: "disk full") }
        return "/tmp/文件 \(index)"
    }

    func close() {
        state.withLock { $0.closed = true }
    }
}

/// 测试里挡一下的地方：`wait` 等到 `open` 为止。
final class Gate: Sendable {
    private let state = Mutex<(open: Bool, waiters: [CheckedContinuation<Void, Never>])>((false, []))

    func wait() async {
        await withCheckedContinuation { continuation in
            let ready = state.withLock { state in
                if state.open { return true }
                state.waiters.append(continuation)
                return false
            }
            if ready { continuation.resume() }
        }
    }

    func open() {
        let waiters = state.withLock { state in
            state.open = true
            defer { state.waiters = [] }
            return state.waiters
        }
        for waiter in waiters { waiter.resume() }
    }
}

/// 假的压视频：把原文件拷成一个新的 `.mp4`，记下交出的文件。
final class FakeVideoPreparer: Sendable {
    private let outputs = Mutex<[URL]>([])
    let before: @Sendable () async -> Void

    init(before: @escaping @Sendable () async -> Void = {}) {
        self.before = before
    }

    var prepared: [URL] { outputs.withLock { $0 } }

    var prepare: VideoPreparer {
        { [self] source in
            await before()
            let output = FileManager.default.temporaryDirectory.appending(path: "\(UUID().uuidString).mp4")
            try FileManager.default.copyItem(at: source, to: output)
            outputs.withLock { $0.append(output) }
            return output
        }
    }
}

/// 临时目录里一个内容是 `content` 的视频文件。
func videoFile(_ content: Data = Data("movie".utf8)) throws -> URL {
    let url = FileManager.default.temporaryDirectory.appending(path: "\(UUID().uuidString).mov")
    try content.write(to: url)
    return url
}

/// 一张 `width` × `height` 的纯色图片，按 `type` 编码，带上 `properties`（EXIF 方向、GPS 这些）。
func imageData(
    width: Int, height: Int, type: UTType = .png, properties: [CFString: Any] = [:], noise: Bool = false
) throws -> Data {
    let context = try #require(
        CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
    context.setFillColor(red: 1, green: 0, blue: 0, alpha: 1)
    context.fill(CGRect(x: 0, y: 0, width: width, height: height))
    if noise, let pixels = context.data?.assumingMemoryBound(to: UInt32.self) {
        var generator = SystemRandomNumberGenerator()
        for index in 0..<(context.bytesPerRow / 4 * height) {
            pixels[index] = generator.next() | 0xff00_0000
        }
    }
    let image = try #require(context.makeImage())
    let output = NSMutableData()
    let destination = try #require(CGImageDestinationCreateWithData(output, type.identifier as CFString, 1, nil))
    CGImageDestinationAddImage(destination, image, properties as CFDictionary)
    #expect(CGImageDestinationFinalize(destination))
    return output as Data
}

@MainActor @Suite struct MediaSendTests {
    let link = FakeLink()
    let machine = machineRecord()
    let session = SessionId("0123456789abcdef0011223344556677")!

    /// 粘贴进 `session` 的路径，按先后。
    var pasted: [String] {
        link.sent.compactMap { message -> String? in
            guard case .pastePath(_, let id, let path) = message, id == session else { return nil }
            return path
        }
    }

    func exists(_ url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.path(percentEncoded: false))
    }

    @Test func eachImageIsPastedInOrder() async throws {
        let upload = FakeMediaUpload()
        let model = MediaSendModel(open: { _ in upload })
        let images = try [1, 2, 3].map { MediaItem.image(try imageData(width: $0, height: 1)) }
        await model.send(images, machine: machine, link: link, sessionId: session)
        #expect(upload.received.count == 3)
        #expect(upload.closed)
        #expect(pasted == ["/tmp/文件 0", "/tmp/文件 1", "/tmp/文件 2"])
        #expect(model.phase == .idle)
        #expect(model.errorMessage == nil)
    }

    /// 视频先压，再从压好的文件流式地发；原文件和压好的文件发完都删掉。
    @Test func aVideoIsPreparedThenStreamedFromTheFile() async throws {
        let upload = FakeMediaUpload()
        let preparer = FakeVideoPreparer()
        let model = MediaSendModel(open: { _ in upload }, prepareVideo: preparer.prepare)
        let source = try videoFile(Data("clip".utf8))
        await model.send([.video(source)], machine: machine, link: link, sessionId: session)
        #expect(upload.received == [.file(Data("clip".utf8))])
        #expect(pasted == ["/tmp/文件 0"])
        #expect(model.errorMessage == nil)
        #expect(!exists(source))
        let prepared = try #require(preparer.prepared.first)
        #expect(!exists(prepared))
    }

    /// 图片和视频混着挑时按挑的先后发、先后粘贴。
    @Test func mixedImagesAndVideosKeepTheirOrder() async throws {
        let upload = FakeMediaUpload()
        let preparer = FakeVideoPreparer()
        let model = MediaSendModel(open: { _ in upload }, prepareVideo: preparer.prepare)
        let first = try imageData(width: 2, height: 1)
        let last = try imageData(width: 3, height: 1)
        let video = try videoFile(Data("middle".utf8))
        await model.send(
            [.image(first), .video(video), .image(last)], machine: machine, link: link, sessionId: session)
        let kinds = upload.received.map { sent -> String in
            switch sent {
            case .data: "data"
            case .file(let content): content == Data("middle".utf8) ? "video" : "file?"
            }
        }
        #expect(kinds == ["data", "video", "data"])
        #expect(pasted == ["/tmp/文件 0", "/tmp/文件 1", "/tmp/文件 2"])
        #expect(upload.closed)
    }

    /// 发到一半终端的连接断了、等不到重新连上：存好的那项不粘贴（`send` 会丢掉），报连接断开，后面的不发。
    @Test func aTerminalThatStaysDisconnectedGetsNoPastePath() async throws {
        let link = link
        let upload = FakeMediaUpload { index, _ in
            if index == 1 { link.isConnected = false }
        }
        let model = MediaSendModel(open: { _ in upload }, reconnectWait: .milliseconds(50))
        let images = try [1, 2, 3].map { MediaItem.image(try imageData(width: $0, height: 1)) }
        await model.send(images, machine: machine, link: link, sessionId: session)
        #expect(upload.received.count == 2)
        #expect(pasted == ["/tmp/文件 0"])
        #expect(model.errorMessage == String(localized: "和电脑的连接断开了"))
        #expect(upload.closed)
        #expect(model.phase == .idle)
    }

    /// 存好一项时终端的连接断着（App 退到后台），重新连上后照样粘贴，接着发后面的。
    @Test func aPathWaitsForTheTerminalToReconnect() async throws {
        let link = link
        let upload = FakeMediaUpload { index, _ in
            if index == 1 { link.isConnected = false }
        }
        let model = MediaSendModel(open: { _ in upload }, reconnectWait: .seconds(30))
        let images = try [1, 2, 3].map { MediaItem.image(try imageData(width: $0, height: 1)) }
        let machine = machine
        let session = session
        let task = Task { await model.send(images, machine: machine, link: link, sessionId: session) }
        #expect(await eventually { link.subscriberCount == 1 })
        #expect(pasted == ["/tmp/文件 0"])
        link.isConnected = true
        link.emit(.ready(generation: 2))
        await task.value
        #expect(pasted == ["/tmp/文件 0", "/tmp/文件 1", "/tmp/文件 2"])
        #expect(model.errorMessage == nil)
    }

    /// 一批从排上到发完开着一段后台任务。
    @Test func aBatchHoldsABackgroundTask() async throws {
        let begun = Mutex(0)
        let ended = Mutex(0)
        let gate = Gate()
        let upload = FakeMediaUpload { _, _ in await gate.wait() }
        let model = MediaSendModel(
            open: { _ in upload },
            beginBackgroundTask: { _ in
                begun.withLock { $0 += 1 }
                return { ended.withLock { $0 += 1 } }
            })
        let items: [MediaItem] = [.image(try imageData(width: 1, height: 1))]
        let link = link
        let machine = machine
        let session = session
        let task = Task { await model.send(items, machine: machine, link: link, sessionId: session) }
        #expect(await eventually { upload.received.count == 1 })
        #expect(begun.withLock { $0 } == 1)
        #expect(ended.withLock { $0 } == 0)
        gate.open()
        await task.value
        #expect(ended.withLock { $0 } == 1)
    }

    /// 压视频时报 `preparing`，发视频时报发出去的比例，图片不报比例；发完回到 `idle`。
    @Test func phaseFollowsPreparingAndSending() async throws {
        let preparing = Gate()
        let sending = Gate()
        let upload = FakeMediaUpload { _, progress in
            guard let progress else { return }
            progress(1, 4)
            await sending.wait()
        }
        let preparer = FakeVideoPreparer { await preparing.wait() }
        let model = MediaSendModel(open: { _ in upload }, prepareVideo: preparer.prepare)
        let items: [MediaItem] = [.image(try imageData(width: 1, height: 1)), .video(try videoFile())]
        let link = link
        let machine = machine
        let session = session
        let task = Task { await model.send(items, machine: machine, link: link, sessionId: session) }
        #expect(await eventually { model.phase == .preparing(index: 2, count: 2) })
        preparing.open()
        #expect(await eventually { model.phase == .sending(index: 2, count: 2, fraction: 0.25) })
        sending.open()
        await task.value
        #expect(model.phase == .idle)
        #expect(pasted.count == 2)
    }

    /// 电脑上的 Runode 不认得 `pastePath` 时什么都不发，直接报版本太旧；交来的视频文件也删掉。
    @Test func anOldComputerIsReported() async throws {
        link.pastesPaths = false
        let opened = Mutex(0)
        let model = MediaSendModel(open: { _ in
            opened.withLock { $0 += 1 }
            return FakeMediaUpload()
        })
        #expect(!model.checkAvailable(link: link))
        model.errorMessage = nil
        let video = try videoFile()
        await model.send(
            [.image(try imageData(width: 1, height: 1)), .video(video)], machine: machine, link: link,
            sessionId: session)
        #expect(model.errorMessage == String(localized: "电脑上的 Runode 版本太旧，更新后才能发送图片和视频"))
        #expect(opened.withLock { $0 } == 0)
        #expect(link.sent.isEmpty)
        #expect(!exists(video))
    }

    /// 没连上时报未连接，不拿上次连上时的 `pastesPaths` 说电脑太旧。
    @Test func aDisconnectedComputerIsNotCalledOld() {
        link.pastesPaths = false
        link.isConnected = false
        let model = MediaSendModel(open: { _ in FakeMediaUpload() })
        #expect(!model.checkAvailable(link: link))
        #expect(model.errorMessage == String(localized: "未连接"))
    }

    @Test func withoutAnOpenerNothingIsSent() async throws {
        let model = MediaSendModel(open: nil)
        #expect(!model.checkAvailable(link: link))
        #expect(model.errorMessage != nil)
    }

    /// 电脑存不了一项时报出原因，前面的已经粘贴了，后面的不发，没轮到的视频文件也删掉。
    @Test func aFailureStopsTheBatch() async throws {
        let upload = FakeMediaUpload(failAt: 1)
        let preparer = FakeVideoPreparer()
        let model = MediaSendModel(open: { _ in upload }, prepareVideo: preparer.prepare)
        let video = try videoFile()
        let items = try [1, 2].map { MediaItem.image(try imageData(width: $0, height: 1)) } + [.video(video)]
        await model.send(items, machine: machine, link: link, sessionId: session)
        #expect(upload.received.count == 2)
        #expect(link.sent.count == 1)
        #expect(model.errorMessage == MediaFailure(reason: "disk full").errorDescription)
        #expect(upload.closed)
        #expect(model.phase == .idle)
        #expect(preparer.prepared.isEmpty)
        #expect(!exists(video))
    }

    @Test func unreadableDataIsReported() async throws {
        let upload = FakeMediaUpload()
        let model = MediaSendModel(open: { _ in upload })
        await model.send([.image(Data("not an image".utf8))], machine: machine, link: link, sessionId: session)
        #expect(upload.received.isEmpty)
        #expect(model.errorMessage == ImagePreparationError.unreadable.errorDescription)
    }
}

/// 重新编码：转正、缩小、去掉元数据，PNG 还是 PNG。
@Suite struct ImagePreparationTests {
    func properties(_ data: Data) throws -> (type: String?, properties: [CFString: Any]) {
        let source = try #require(CGImageSourceCreateWithData(data as CFData, nil))
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] ?? [:]
        return (CGImageSourceGetType(source) as String?, properties)
    }

    @Test func aRotatedJPEGIsUprightAndLosesItsMetadata() throws {
        let original = try imageData(
            width: 40, height: 20, type: .jpeg,
            properties: [
                kCGImagePropertyOrientation: 6,
                kCGImagePropertyGPSDictionary: [kCGImagePropertyGPSLatitude: 31.2, kCGImagePropertyGPSLongitude: 121.5],
                kCGImagePropertyTIFFDictionary: [kCGImagePropertyTIFFMake: "Apple", kCGImagePropertyTIFFModel: "iPhone"],
            ])
        #expect(try properties(original).properties[kCGImagePropertyGPSDictionary] != nil)

        let prepared = try ImagePreparation.prepare(original)
        let (type, read) = try properties(prepared)
        #expect(type == UTType.jpeg.identifier)
        // 方向 6 是顺时针转 90 度：转正后宽高对调，不再带方向。
        #expect(read[kCGImagePropertyPixelWidth] as? Int == 20)
        #expect(read[kCGImagePropertyPixelHeight] as? Int == 40)
        #expect((read[kCGImagePropertyOrientation] as? Int ?? 1) == 1)
        #expect(read[kCGImagePropertyGPSDictionary] == nil)
        let tiff = read[kCGImagePropertyTIFFDictionary] as? [CFString: Any]
        #expect(tiff?[kCGImagePropertyTIFFMake] == nil)
        #expect(tiff?[kCGImagePropertyTIFFModel] == nil)
    }

    @Test func aLargeImageIsScaledDown() throws {
        let prepared = try ImagePreparation.prepare(try imageData(width: 4000, height: 1000))
        let (type, read) = try properties(prepared)
        #expect(type == UTType.png.identifier)
        #expect(read[kCGImagePropertyPixelWidth] as? Int == ImagePreparation.maxPixelSize)
        #expect(read[kCGImagePropertyPixelHeight] as? Int == ImagePreparation.maxPixelSize / 4)
    }

    @Test func aSmallPNGStaysAPNGOfTheSameSize() throws {
        let prepared = try ImagePreparation.prepare(try imageData(width: 30, height: 10))
        let (type, read) = try properties(prepared)
        #expect(type == UTType.png.identifier)
        #expect(read[kCGImagePropertyPixelWidth] as? Int == 30)
        #expect(read[kCGImagePropertyPixelHeight] as? Int == 10)
    }

    /// 噪点图编成 PNG 太大，改写成 JPEG。
    @Test func anOversizedPNGBecomesAJPEG() throws {
        let original = try imageData(width: 2048, height: 2048, noise: true)
        #expect(original.count > ImagePreparation.pngLimit)
        let prepared = try ImagePreparation.prepare(original)
        #expect(try properties(prepared).type == UTType.jpeg.identifier)
    }

    @Test func aGIFIsSentAsIs() throws {
        let gif = try imageData(width: 8, height: 8, type: .gif)
        #expect(try ImagePreparation.prepare(gif) == gif)
    }

    @Test func garbageIsUnreadable() {
        #expect(throws: ImagePreparationError.unreadable) { try ImagePreparation.prepare(Data("hello".utf8)) }
    }
}
