import AVFoundation
import CoreVideo
import Foundation
import Testing

@testable import RunodeFeatures

/// 压视频：写成 MP4，缩到 720p 以内，去掉拍摄地点。
@Suite struct VideoPreparationTests {
    /// 一段 `width` × `height`、几帧的 H.264 视频，带拍摄地点，写成 QuickTime 文件。
    func makeVideo(width: Int, height: Int, frames: Int = 6) async throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "\(UUID().uuidString).mov")
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        let location = AVMutableMetadataItem()
        location.identifier = .quickTimeMetadataLocationISO6709
        location.value = "+31.2000+121.5000/" as NSString
        location.dataType = kCMMetadataBaseDataType_UTF8 as String
        writer.metadata = [location]
        let input = AVAssetWriterInput(
            mediaType: .video,
            outputSettings: [AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: width, AVVideoHeightKey: height])
        input.expectsMediaDataInRealTime = false
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(
            assetWriterInput: input,
            sourcePixelBufferAttributes: [
                kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
                kCVPixelBufferWidthKey as String: width, kCVPixelBufferHeightKey as String: height,
            ])
        writer.add(input)
        #expect(writer.startWriting())
        writer.startSession(atSourceTime: .zero)
        let pool = try #require(adaptor.pixelBufferPool)
        for frame in 0..<frames {
            while !input.isReadyForMoreMediaData { try await Task.sleep(for: .milliseconds(5)) }
            var buffer: CVPixelBuffer?
            CVPixelBufferPoolCreatePixelBuffer(nil, pool, &buffer)
            let pixels = try #require(buffer)
            CVPixelBufferLockBaseAddress(pixels, [])
            memset(CVPixelBufferGetBaseAddress(pixels), Int32(frame * 40), CVPixelBufferGetDataSize(pixels))
            CVPixelBufferUnlockBaseAddress(pixels, [])
            #expect(adaptor.append(pixels, withPresentationTime: CMTime(value: CMTimeValue(frame), timescale: 30)))
        }
        input.markAsFinished()
        await writer.finishWriting()
        #expect(writer.status == .completed)
        return url
    }

    func hasLocation(_ url: URL) async throws -> Bool {
        let metadata = try await AVURLAsset(url: url).load(.metadata)
        return metadata.contains { $0.identifier == .quickTimeMetadataLocationISO6709 }
    }

    @Test func aLargeVideoBecomesA720pMP4WithoutLocation() async throws {
        let source = try await makeVideo(width: 1920, height: 1080)
        defer { try? FileManager.default.removeItem(at: source) }
        #expect(try await hasLocation(source))

        let output = try await VideoPreparation.prepare(source)
        defer { try? FileManager.default.removeItem(at: output) }
        #expect(output.pathExtension == "mp4")
        // MP4 的第一个 box 是 `ftyp`。
        let head = try FileHandle(forReadingFrom: output).read(upToCount: 8) ?? Data()
        #expect(head.count == 8 && String(decoding: head.suffix(4), as: UTF8.self) == "ftyp")

        let asset = AVURLAsset(url: output)
        let track = try #require(try await asset.loadTracks(withMediaType: .video).first)
        let size = try await track.load(.naturalSize)
        #expect(size.width <= 1280 && size.height <= 720)
        #expect(size.width > 0 && size.height > 0)
        #expect(try await !hasLocation(output))
    }

    @Test func garbageFailsAndLeavesNoOutput() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let source = directory.appending(path: "not-a-video.mov")
        try Data("hello".utf8).write(to: source)
        await #expect(throws: VideoPreparationError.self) {
            _ = try await VideoPreparation.prepare(source, in: directory)
        }
        #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path(percentEncoded: false)) == ["not-a-video.mov"])
    }
}
